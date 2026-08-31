//! 项目部署收口、冲突检查与受管目录解析。

use super::Orchestrator;
use super::storage::{
    copy_auxiliary, resolve_existing_prefix, sibling_temporary, validate_generated_files,
    write_attachment, write_project_file,
};
use crate::config::set_mode;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use crate::docker;
use crate::spec::{BindMount, StackSpec, validate_name};
use crate::template::GeneratedFile;
use anyhow::Context;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

impl Orchestrator {
    /// 执行全部检查、暂存全部文件并原子提交一个项目。
    pub(super) fn deploy(
        &self,
        spec: &StackSpec,
        files: &[GeneratedFile],
        force: bool,
    ) -> anyhow::Result<()> {
        spec.validate()?;
        self.validate_mounts(spec)?;
        self.ensure_no_conflicts(spec)?;
        validate_generated_files(files)?;
        fs::create_dir_all(&self.config.stacks_root).with_context(|| {
            format!("无法创建项目根目录: {}", self.config.stacks_root.display())
        })?;
        set_mode(&self.config.stacks_root, 0o750)?;
        let target = self.project_dir(&spec.name)?;
        if target.exists() && !force {
            anyhow::bail!("项目 {} 已存在；确认覆盖请使用 --force", spec.name);
        }
        let stage = sibling_temporary(&target, "stage")?;
        if stage.exists() {
            anyhow::bail!("临时项目目录已存在: {}", stage.display());
        }
        fs::create_dir(&stage)?;
        set_mode(&stage, 0o750)?;
        let result = (|| -> anyhow::Result<()> {
            if target.is_dir() {
                copy_auxiliary(&target, &stage)?;
            }
            if files.iter().any(|file| file.path.starts_with("site")) {
                let staged_site = stage.join("site");
                if staged_site.exists() {
                    fs::remove_dir_all(&staged_site).with_context(|| {
                        format!("无法替换静态站点目录: {}", staged_site.display())
                    })?;
                }
            }
            write_project_file(
                &stage.join(COMPOSE_FILE),
                spec.compose_yaml()?.as_bytes(),
                0o640,
            )?;
            write_project_file(&stage.join(ENV_FILE), spec.env_file().as_bytes(), 0o600)?;
            for file in files {
                write_attachment(&stage, file)?;
            }
            let _validated = docker::compose_config(&self.config, &stage)?;
            self.commit_stage(&stage, &target)?;
            Ok(())
        })();
        if result.is_err() && stage.exists() {
            fs::remove_dir_all(&stage)
                .with_context(|| format!("部署失败后无法清理临时目录: {}", stage.display()))?;
        }
        result
    }

    /// 将已校验的暂存目录替换到目标位置，并支持失败回滚。
    fn commit_stage(&self, stage: &Path, target: &Path) -> anyhow::Result<()> {
        let backup = sibling_temporary(target, "backup")?;
        let had_target = target.exists();
        if had_target {
            fs::rename(target, &backup)
                .with_context(|| format!("无法备份当前项目: {}", target.display()))?;
        }
        if let Err(error) = fs::rename(stage, target) {
            if had_target {
                fs::rename(&backup, target).context("无法恢复项目备份")?;
            }
            return Err(error).context("无法原子替换项目目录");
        }
        if let Err(error) = docker::compose_config(&self.config, target) {
            let failed = sibling_temporary(target, "failed")?;
            fs::rename(target, &failed).context("无法隔离验证失败的项目")?;
            if had_target {
                fs::rename(&backup, target).context("无法恢复项目备份")?;
            }
            fs::remove_dir_all(&failed).context("无法清理验证失败的项目")?;
            return Err(error).context("Compose 验证失败，已恢复原项目");
        }
        if had_target {
            fs::remove_dir_all(&backup)
                .with_context(|| format!("无法清理项目备份: {}", backup.display()))?;
        }
        Ok(())
    }

    /// 解析 bind mount 源路径并执行由配置推导的白名单。
    fn validate_mounts(&self, spec: &StackSpec) -> anyhow::Result<()> {
        let roots: Vec<PathBuf> = self
            .config
            .data_roots
            .iter()
            .chain(std::iter::once(&self.config.stacks_root))
            .map(|path| resolve_existing_prefix(path))
            .collect::<anyhow::Result<_>>()?;
        let docker_socket = resolve_existing_prefix(&self.config.docker_socket)?;
        for (service_name, service) in &spec.document.services {
            for value in &service.volumes {
                let mount = BindMount::parse(value)?;
                let source = resolve_existing_prefix(Path::new(&mount.host_path))?;
                let allowed =
                    source == docker_socket || roots.iter().any(|root| source.starts_with(root));
                if !allowed {
                    anyhow::bail!(
                        "服务 {service_name} 的 bind mount 不在白名单内: {}",
                        mount.host_path
                    );
                }
            }
        }
        Ok(())
    }

    /// 拒绝全部受管项目之间重复的路由主机名或宿主机端口。
    fn ensure_no_conflicts(&self, requested: &StackSpec) -> anyhow::Result<()> {
        let requested_hosts = unique_hosts(requested)?;
        let requested_ports = unique_ports(requested)?;
        if !self.config.stacks_root.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(&self.config.stacks_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir()
                || entry.file_name() == requested.name.as_str()
                || entry.file_name().to_string_lossy().starts_with('.')
                || !entry.path().join(COMPOSE_FILE).is_file()
            {
                continue;
            }
            let existing = StackSpec::load(&entry.path())
                .with_context(|| format!("无法检查现有项目冲突: {}", entry.path().display()))?;
            for host in unique_hosts(&existing)? {
                if requested_hosts.contains(&host) {
                    anyhow::bail!("域名 {host} 已被项目 {} 使用", existing.name);
                }
            }
            for port in unique_ports(&existing)? {
                if requested_ports.contains(&port) {
                    anyhow::bail!(
                        "宿主机端口 {}/{} 已被项目 {} 使用",
                        port.0,
                        port.1.as_str(),
                        existing.name
                    );
                }
            }
        }
        Ok(())
    }

    /// 加载单个项目当前由 Compose 支撑的状态。
    pub(super) fn load(&self, name: &str) -> anyhow::Result<StackSpec> {
        StackSpec::load(&self.existing_project_dir(name)?)
    }

    /// 在 `stacks_root` 下解析已校验的项目名。
    pub(super) fn project_dir(&self, name: &str) -> anyhow::Result<PathBuf> {
        validate_name("项目名", name)?;
        Ok(self.config.stacks_root.join(name))
    }

    /// 解析项目目录并要求该目录存在。
    pub(super) fn existing_project_dir(&self, name: &str) -> anyhow::Result<PathBuf> {
        let directory = self.project_dir(name)?;
        if !directory.is_dir() {
            anyhow::bail!("项目不存在: {name}");
        }
        Ok(directory)
    }
}

/// 收集路由主机名并拒绝同一项目内的重复值。
fn unique_hosts(spec: &StackSpec) -> anyhow::Result<BTreeSet<String>> {
    let hosts = spec.route_hosts()?;
    let output: BTreeSet<String> = hosts.iter().cloned().collect();
    if output.len() != hosts.len() {
        anyhow::bail!("项目 {} 重复声明 Traefik host", spec.name);
    }
    Ok(output)
}

/// 收集宿主机端口并拒绝同一项目内的重复值。
fn unique_ports(spec: &StackSpec) -> anyhow::Result<BTreeSet<(u16, crate::spec::PortProtocol)>> {
    let ports = spec.host_ports()?;
    let output: BTreeSet<_> = ports
        .iter()
        .map(|port| (port.host_port, port.protocol))
        .collect();
    if output.len() != ports.len() {
        anyhow::bail!("项目 {} 重复发布宿主机端口", spec.name);
    }
    Ok(output)
}
