//! 项目应用、导入、查询与生命周期操作。

use super::storage::sibling_temporary;
use super::{Asset, Orchestrator, StackInfo};
use crate::config::Config;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use crate::docker::{self, PullProgress};
use crate::spec::StackSpec;
use crate::template::{self, GeneratedFile, TemplateKind};
use anyhow::Context;
use std::fs;
use std::path::PathBuf;

impl Orchestrator {
    /// 使用已校验配置创建管理器。
    ///
    /// # 错误
    ///
    /// 配置不满足约束时返回错误。
    pub fn new(config: Config) -> anyhow::Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    /// 返回管理器配置。
    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// 解析 TOML、展开模板并通过唯一收口部署。
    ///
    /// # 错误
    ///
    /// 任一校验失败时返回错误，且不替换项目状态。
    pub fn apply(
        &self,
        config_toml: &str,
        assets: Vec<Asset>,
        force: bool,
        start: bool,
    ) -> anyhow::Result<String> {
        let mut generated = template::apply(config_toml, &self.config)?;
        if !assets.is_empty() && generated.kind != TemplateKind::Static {
            anyhow::bail!("--assets 仅能与 static 模板一起使用");
        }
        if generated.kind == TemplateKind::Static && assets.is_empty() {
            let target = self.project_dir(&generated.spec.name)?;
            if !target.join("site").is_dir() {
                anyhow::bail!("static 模板首次部署必须提供 --assets");
            }
        }
        for asset in assets {
            generated.files.push(GeneratedFile {
                path: PathBuf::from("site").join(asset.path),
                content: asset.content,
                mode: 0o640,
                replace: true,
            });
        }
        self.attach_oidc_client_fragments(&mut generated)?;
        let oidc_fragment = if generated.kind == TemplateKind::Authelia {
            None
        } else {
            Some(self.prepare_oidc_client_fragment(&generated.spec)?)
        };
        self.deploy(&generated.spec, &generated.files, force)?;
        let oidc_updated = match oidc_fragment {
            Some(fragment) => {
                self.sync_oidc_client_fragment(&generated.spec.name, fragment.as_ref())?
            }
            None => false,
        };
        if start {
            docker::compose_up(&self.config, &self.project_dir(&generated.spec.name)?, None)?;
        }
        Ok(format!(
            "项目 {} 已应用{}",
            generated.spec.name,
            oidc_update_suffix(oidc_updated)
        ))
    }

    /// 将 Compose 文档导入受支持的 IR 并替换项目。
    ///
    /// # 错误
    ///
    /// 遇到不支持字段或部署校验失败时返回错误。
    pub fn import_compose(
        &self,
        name: &str,
        compose_yaml: &str,
        env_file: Option<&str>,
        start: bool,
    ) -> anyhow::Result<String> {
        let directory = self.project_dir(name)?;
        let preserved = if env_file.is_none() && directory.is_dir() {
            fs::read_to_string(directory.join(ENV_FILE)).unwrap_or_default()
        } else {
            String::new()
        };
        let spec = StackSpec::parse(name, compose_yaml, env_file.unwrap_or(&preserved))?;
        let oidc_fragment = self.prepare_oidc_client_fragment(&spec)?;
        self.deploy(&spec, &[], true)?;
        let oidc_updated = self.sync_oidc_client_fragment(name, oidc_fragment.as_ref())?;
        if start {
            docker::compose_up(&self.config, &directory, None)?;
        }
        Ok(format!(
            "项目 {name} 已导入{}",
            oidc_update_suffix(oidc_updated)
        ))
    }

    /// 将当前项目状态导出为规范化 TOML。
    ///
    /// # 错误
    ///
    /// 当前 Compose 状态无法由模板表示时返回错误。
    pub fn export(&self, name: &str) -> anyhow::Result<String> {
        let spec = self.load(name)?;
        template::export(&spec, &self.config)
    }

    /// 列出全部有效的受管项目。
    ///
    /// # 错误
    ///
    /// 无法枚举项目存储目录时返回错误。
    pub fn list(&self) -> anyhow::Result<Vec<StackInfo>> {
        if !self.config.stacks_root.exists() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.config.stacks_root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && !entry.file_name().to_string_lossy().starts_with('.')
                && entry.path().join(COMPOSE_FILE).is_file()
            {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        names.into_iter().map(|name| self.get(&name)).collect()
    }

    /// 获取单个受管项目及其当前容器状态。
    ///
    /// # 错误
    ///
    /// 项目状态缺失或无效时返回错误。
    pub fn get(&self, name: &str) -> anyhow::Result<StackInfo> {
        let spec = self.load(name)?;
        let directory = self.project_dir(name)?;
        let status = docker::compose_ps(&self.config, &directory)
            .unwrap_or_else(|error| format!("unavailable: {error}"));
        Ok(StackInfo {
            name: spec.name.clone(),
            services: spec.document.services.keys().cloned().collect(),
            compose_yaml: spec.compose_yaml()?,
            environment: spec.environment,
            status,
        })
    }

    /// 启动项目。
    ///
    /// # 错误
    ///
    /// 项目检查或 Docker 操作失败时返回错误。
    pub fn start(&self, name: &str) -> anyhow::Result<String> {
        docker::compose_up(&self.config, &self.existing_project_dir(name)?, None)?;
        Ok(format!("项目 {name} 已启动"))
    }

    /// 停止项目。
    ///
    /// # 错误
    ///
    /// 项目检查或 Docker 操作失败时返回错误。
    pub fn stop(&self, name: &str) -> anyhow::Result<String> {
        docker::compose_stop(&self.config, &self.existing_project_dir(name)?)?;
        Ok(format!("项目 {name} 已停止"))
    }

    /// 重启项目。
    ///
    /// # 错误
    ///
    /// 项目检查或 Docker 操作失败时返回错误。
    pub fn restart(&self, name: &str) -> anyhow::Result<String> {
        docker::compose_restart(&self.config, &self.existing_project_dir(name)?)?;
        Ok(format!("项目 {name} 已重启"))
    }

    /// 构建项目镜像。
    ///
    /// # 错误
    ///
    /// 项目检查或 Docker 操作失败时返回错误。
    pub fn build(&self, name: &str) -> anyhow::Result<String> {
        docker::compose_build(&self.config, &self.existing_project_dir(name)?)?;
        Ok(format!("项目 {name} 构建完成"))
    }

    /// 拉取项目镜像并报告进度。
    ///
    /// # 错误
    ///
    /// 项目检查或 Docker 操作失败时返回错误。
    pub fn pull(
        &self,
        name: &str,
        report: impl FnMut(PullProgress) -> bool,
        connected: impl FnMut() -> bool,
    ) -> anyhow::Result<()> {
        docker::compose_pull(
            &self.config,
            &self.existing_project_dir(name)?,
            report,
            connected,
        )
    }

    /// 流式返回项目日志行。
    ///
    /// # 错误
    ///
    /// 项目检查或 Docker 操作失败时返回错误。
    pub fn logs(
        &self,
        name: &str,
        tail: u32,
        follow: bool,
        report: impl FnMut(String) -> bool,
        connected: impl FnMut() -> bool,
    ) -> anyhow::Result<()> {
        docker::compose_logs(
            &self.config,
            &self.existing_project_dir(name)?,
            tail,
            follow,
            report,
            connected,
        )
    }

    /// 停止容器并仅删除受管项目目录。
    ///
    /// # 错误
    ///
    /// 操作未确认或无法安全完成时返回错误。
    pub fn remove(&self, name: &str, confirmed: bool) -> anyhow::Result<String> {
        if !confirmed {
            anyhow::bail!("删除项目需要确认");
        }
        let directory = self.existing_project_dir(name)?;
        docker::compose_down(&self.config, &directory)?;
        let trash = sibling_temporary(&directory, "removed")?;
        fs::rename(&directory, &trash)
            .with_context(|| format!("无法移动待删除项目: {}", directory.display()))?;
        fs::remove_dir_all(&trash)
            .with_context(|| format!("无法删除项目目录: {}", trash.display()))?;
        let oidc_updated = self.sync_oidc_client_fragment(name, None)?;
        Ok(format!(
            "项目 {name} 已删除；bind mount 数据未删除{}",
            oidc_update_suffix(oidc_updated)
        ))
    }
}

/// 返回 OIDC 客户端片段变更后的运维提示。
pub(super) const fn oidc_update_suffix(updated: bool) -> &'static str {
    if updated {
        "；Authelia OIDC 配置已更新，重启 authelia 后生效"
    } else {
        ""
    }
}
