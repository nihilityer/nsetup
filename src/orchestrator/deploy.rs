//! 项目部署收口、冲突检查与受管目录解析。

use super::Orchestrator;
use super::storage::{
    copy_auxiliary, is_owned_file, resolve_existing_prefix, sibling_temporary,
    sync_owned_directory, validate_generated_files, write_attachment, write_project_file,
};
use crate::config::set_mode;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use crate::docker;
use crate::spec::{BindMount, RouteBinding, RouteIdentity, StackSpec, validate_name};
use crate::template::GeneratedFile;
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

impl Orchestrator {
    /// 执行全部检查、暂存全部文件并原子提交一个项目。
    ///
    /// # 错误
    ///
    /// 任一校验失败、暂存写入失败或 Compose 校验失败时返回错误；失败时原项目保持不变。
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
            // 受管目录在重新应用时整体重写，避免残留上一次部署的陈旧文件；声明为
            // 「不替换」的文件（merge 上传、用户拥有的 custom.yml）会被保留。
            let owned_directories: Vec<&Path> =
                super::OWNED_DIRECTORIES.iter().map(Path::new).collect();
            for owned in &owned_directories {
                sync_owned_directory(&stage, owned, files)?;
            }
            write_project_file(
                &stage.join(COMPOSE_FILE),
                spec.compose_yaml()?.as_bytes(),
                0o640,
            )?;
            write_project_file(&stage.join(ENV_FILE), spec.env_file().as_bytes(), 0o600)?;
            for file in files {
                if owned_directories
                    .iter()
                    .any(|owned| is_owned_file(file, owned))
                {
                    continue;
                }
                write_attachment(&stage, file, file.directory_mode)?;
            }
            let _validated = docker::compose_config(&self.config, &stage, &spec.name)?;
            self.commit_stage(&stage, &target, &spec.name)?;
            Ok(())
        })();
        if result.is_err() && stage.exists() {
            fs::remove_dir_all(&stage)
                .with_context(|| format!("部署失败后无法清理临时目录: {}", stage.display()))?;
        }
        result
    }

    /// 将已校验的暂存目录替换到目标位置，并支持失败回滚。
    fn commit_stage(&self, stage: &Path, target: &Path, project_name: &str) -> anyhow::Result<()> {
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
        if let Err(error) = docker::compose_config(&self.config, target, project_name) {
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
    ///
    /// 绝对路径必须位于 `data_roots` 或 `stacks_root` 内；相对路径按本项目的目录
    /// 解析，因此天然落在受管项目目录内，但仍会再走一次同一份白名单校验。
    fn validate_mounts(&self, spec: &StackSpec) -> anyhow::Result<()> {
        let roots: Vec<PathBuf> = self
            .config
            .data_roots
            .iter()
            .chain(std::iter::once(&self.config.stacks_root))
            .map(|path| resolve_existing_prefix(path))
            .collect::<anyhow::Result<_>>()?;
        let docker_socket = resolve_existing_prefix(&self.config.docker_socket)?;
        let project_directory = self.project_dir(&spec.name)?;
        for (service_name, service) in &spec.document.services {
            for value in &service.volumes {
                let mount = BindMount::parse(value)?;
                let source =
                    resolve_existing_prefix(&mount.resolved_host_path(&project_directory))?;
                let allowed =
                    source == docker_socket || roots.iter().any(|root| source.starts_with(root));
                if !allowed {
                    anyhow::bail!(
                        "服务 {service_name} 的 bind mount 不在白名单内: {}（只能是 data_roots、stacks_root 或 --files/--assets 部署的项目内文件）",
                        mount.host_path
                    );
                }
            }
        }
        Ok(())
    }

    /// 拒绝全部受管项目之间重复的路由身份或宿主机端口。
    ///
    /// 路由冲突按 `host + path_prefix + entrypoint + protocol` 组合判定，因此同一
    /// 域名下的不同路径或不同后端协议可以共存；报错包含冲突方，便于多项目共用域名
    /// 时定位。
    fn ensure_no_conflicts(&self, requested: &StackSpec) -> anyhow::Result<()> {
        let requested_bindings = merge_bindings(requested.route_bindings()?)?;
        let requested_ports = unique_ports(requested)?;
        let others = self.other_specs(&requested.name)?;
        if let Some(message) = detect_conflicts(&requested_bindings, &requested_ports, &others)?
            .into_iter()
            .next()
        {
            anyhow::bail!("{message}");
        }
        Ok(())
    }

    /// 加载除指定项目之外的全部现有受管项目。
    fn other_specs(&self, excluded: &str) -> anyhow::Result<Vec<StackSpec>> {
        if !self.config.stacks_root.is_dir() {
            return Ok(Vec::new());
        }
        let mut specs = Vec::new();
        for entry in fs::read_dir(&self.config.stacks_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir()
                || entry.file_name() == excluded
                || entry.file_name().to_string_lossy().starts_with('.')
                || !entry.path().join(COMPOSE_FILE).is_file()
            {
                continue;
            }
            specs
                .push(StackSpec::load(&entry.path()).with_context(|| {
                    format!("无法检查现有项目冲突: {}", entry.path().display())
                })?);
        }
        Ok(specs)
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

/// 在其它受管项目中查找与请求状态冲突的路由身份与宿主机端口。
///
/// # 错误
///
/// 任一其它项目的路由 label 无法解析时返回错误。
fn detect_conflicts(
    requested_bindings: &BTreeMap<RouteIdentity, String>,
    requested_ports: &BTreeSet<(u16, crate::spec::PortProtocol)>,
    others: &[StackSpec],
) -> anyhow::Result<Vec<String>> {
    let mut conflicts = Vec::new();
    for existing in others {
        for (identity, owner) in merge_bindings(existing.route_bindings()?)? {
            if let Some(conflict) = requested_bindings.get(&identity) {
                conflicts.push(format!(
                    "Traefik 路由冲突：{} 已被 {owner} 使用，与 {conflict} 重复；\
                     同一 host 下请改用不同的 path_prefix、entrypoint 或 protocol",
                    identity.describe()
                ));
            }
        }
        for port in unique_ports(existing)? {
            if requested_ports.contains(&port) {
                conflicts.push(format!(
                    "宿主机端口 {}/{} 已被项目 {} 使用",
                    port.0,
                    port.1.as_str(),
                    existing.name
                ));
            }
        }
    }
    Ok(conflicts)
}

/// 将路由绑定折叠为身份到占用方的映射，并在同一集合内拒绝重复身份。
///
/// # 错误
///
/// 同一集合中存在两条身份完全相同的路由时返回包含双方的错误。
fn merge_bindings(bindings: Vec<RouteBinding>) -> anyhow::Result<BTreeMap<RouteIdentity, String>> {
    let mut output: BTreeMap<RouteIdentity, String> = BTreeMap::new();
    for binding in bindings {
        let owner = binding.owner();
        if let Some(previous) = output.get(&binding.identity) {
            anyhow::bail!(
                "Traefik 路由重复声明：{} 同时由 {previous} 与 {owner} 声明；\
                 同一 host 下请改用不同的 path_prefix、entrypoint 或 protocol",
                binding.identity.describe()
            );
        }
        output.insert(binding.identity, owner);
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

#[cfg(test)]
mod tests {
    use super::{detect_conflicts, merge_bindings};
    use crate::spec::{PortProtocol, Route, RouteProtocol, Service, StackSpec};
    use std::collections::{BTreeMap, BTreeSet};

    /// 同一 host 上不同路径的路由不再互相冲突（A1）。
    #[test]
    fn same_host_different_paths_do_not_conflict() -> anyhow::Result<()> {
        let requested = spec("media", &[("web", Some("/api"))])?;
        let bindings = merge_bindings(requested.route_bindings()?)?;
        let existing = spec("other", &[("web", Some("/"))])?;
        let conflicts =
            detect_conflicts(&bindings, &BTreeSet::new(), std::slice::from_ref(&existing))?;
        assert!(
            conflicts.is_empty(),
            "不同 path_prefix 不应冲突: {conflicts:?}"
        );

        let same = spec("other", &[("web", Some("/api"))])?;
        let conflicts = detect_conflicts(&bindings, &BTreeSet::new(), &[same])?;
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("other"), "{conflicts:?}");
        assert!(conflicts[0].contains("media"), "{conflicts:?}");
        Ok(())
    }

    /// 宿主机端口冲突仍然被拒绝并指出占用项目。
    #[test]
    fn host_port_conflicts_are_reported() -> anyhow::Result<()> {
        let mut existing = spec("other", &[("web", None)])?;
        existing
            .document
            .services
            .get_mut("web")
            .ok_or_else(|| anyhow::anyhow!("missing service"))?
            .ports = vec![String::from("127.0.0.1:8080:80/tcp")];
        let requested_ports = BTreeSet::from([(8080_u16, PortProtocol::Tcp)]);
        let conflicts = detect_conflicts(&BTreeMap::new(), &requested_ports, &[existing])?;
        assert_eq!(conflicts.len(), 1);
        assert!(
            conflicts[0].contains("宿主机端口 8080/tcp"),
            "{conflicts:?}"
        );
        Ok(())
    }

    /// 构造一个带单条路由的测试项目。
    fn spec(name: &str, routes: &[(&str, Option<&str>)]) -> anyhow::Result<StackSpec> {
        let mut service = Service {
            image: String::from("example/app:1"),
            ..Service::default()
        };
        let routes: Vec<Route> = routes
            .iter()
            .enumerate()
            .map(|(index, (_label, prefix))| Route {
                name: format!("route{index}"),
                hosts: vec![String::from("shared.example.com")],
                path_prefix: prefix.map(str::to_string),
                container_port: Some(80),
                middlewares: Vec::new(),
                protocol: RouteProtocol::Http,
                entrypoint: String::from("https"),
                sticky_cookie: false,
                pass_host_header: None,
                priority: None,
                service: None,
                tls_domains: Vec::new(),
            })
            .collect();
        service.set_routes(name, "web", &routes)?;
        Ok(StackSpec {
            name: name.to_string(),
            document: crate::spec::Document {
                services: BTreeMap::from([(String::from("web"), service)]),
                networks: BTreeMap::new(),
            },
            environment: BTreeMap::new(),
        })
    }
}
