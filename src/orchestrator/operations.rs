//! 项目应用、导入、查询与生命周期操作。

use super::oidc::OidcChange;
use super::storage::sibling_temporary;
use super::{Asset, FilesUpload, Orchestrator, StackInfo};
use crate::config::Config;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use crate::docker::{self, PullProgress};
use crate::spec::HookStage;
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

    /// 更新主域名并持久化到 daemon 配置文件。
    ///
    /// 已部署项目里的完整域名不会自动改写，因此一并返回仍需手工调整的项目名，
    /// 供调用方提示运维人员。
    ///
    /// # 错误
    ///
    /// 域名无效或配置文件无法写入时返回错误。
    pub fn set_domain(&self, domain: &str) -> anyhow::Result<Vec<String>> {
        crate::config::validate_domain(domain)?;
        let mut candidate = self.config.clone();
        candidate.domain = domain.to_string();
        candidate.validate()?;
        let mut affected = Vec::new();
        for info in self.list()? {
            let spec = self.load(&info.name)?;
            if spec.domains()?.iter().any(|value| value != domain) {
                affected.push(spec.name.clone());
            }
        }
        crate::config::write_system_config(&candidate)?;
        Ok(affected)
    }

    /// 解析 TOML、展开模板并通过唯一收口部署。
    ///
    /// # 错误
    ///
    /// 任一校验失败时返回错误，且不替换项目状态。
    pub fn apply(&self, request: &ApplyRequest<'_>) -> anyhow::Result<String> {
        if request.files_only {
            return self.apply_files_only(request);
        }
        let project_directory = project_directory_from_toml(request.config_toml)?;
        let mut generated = template::apply(
            request.config_toml,
            &self.config,
            (!request.project_files.is_empty()).then_some(request.files_into),
        )?;
        if let Some(directory) = &project_directory {
            debug_assert_eq!(directory, &generated.spec.name);
        }
        if generated.kind == TemplateKind::Static {
            if !request.assets_provided {
                let target = self.project_dir(&generated.spec.name)?;
                if !target.join("site").is_dir() {
                    anyhow::bail!("static 模板首次部署必须提供非空的 --assets <目录>");
                }
            }
            // 合并模式只覆盖同名文件，因此 `up --force` 不会清空站点目录；需要
            // 删除已下线的旧文件时显式使用 `--assets-mode replace`。
            attach_assets(
                &mut generated.files,
                request.assets,
                request.replace_assets,
                request.asset_permissions,
            );
        } else if request.assets_provided || !request.assets.is_empty() {
            anyhow::bail!("--assets 仅能与 static 模板一起使用；其它模板请使用 --files");
        }
        generated
            .files
            .extend(request.project_files.iter().map(|file| file.file.clone()));
        // Authelia 项目每次整体替换 config/oidc-clients，必须先收集现有应用片段，
        // 否则重新应用 authelia.toml 会清空全部客户端声明。
        self.attach_oidc_client_fragments(&mut generated)?;
        let pre_start = generated.spec.service_hooks(HookStage::PreStart)?;
        let post_start = generated.spec.service_hooks(HookStage::PostStart)?;
        let oidc_change = self.prepare_oidc_change(&generated.spec)?;
        self.deploy(&generated.spec, &generated.files, request.force)?;
        // 客户端已经显式选择时以它为准，否则在确实发生片段变化时顺带重启。
        let restart_dependents =
            request.restart_dependents || matches!(oidc_change, OidcChange::Sync(_));
        let (oidc_updated, oidc_restarted) =
            self.apply_oidc_change(&generated.spec.name, oidc_change, restart_dependents)?;
        let pre_start_ran = self
            .run_hooks(
                &generated.spec.name,
                &pre_start,
                HookStage::PreStart,
                request.report,
            )?
            .is_some();
        if request.start {
            self.recreate_if_running(&generated.spec.name)?;
            let _post = self.run_hooks(
                &generated.spec.name,
                &post_start,
                HookStage::PostStart,
                request.report,
            )?;
        }
        let mut message = format!("项目 {} 已应用", generated.spec.name);
        message.push_str(oidc_update_suffix(oidc_updated, oidc_restarted));
        if pre_start_ran {
            message.push_str("；已执行 pre_start 钩子");
        }
        Ok(message)
    }

    /// 只重新上传 `--files` / `--assets`，保留现有容器不做任何重建。
    ///
    /// 受管项目目录整体替换会让运行中容器持有旧目录 inode，因此这条路径直接在
    /// 现有项目目录内更新附属文件，不触碰 `compose.yaml`、`.env`，也不执行
    /// Compose 与钩子。
    ///
    /// # 错误
    ///
    /// 项目尚未部署、文件路径不安全或写入失败时返回错误。
    fn apply_files_only(&self, request: &ApplyRequest<'_>) -> anyhow::Result<String> {
        let project = project_directory_from_toml(request.config_toml)?.ok_or_else(|| {
            anyhow::anyhow!("--files-only 需要声明项目名（traefik/authelia 模板可省略 name）")
        })?;
        let mut generated = template::apply(request.config_toml, &self.config, None)?;
        if generated.spec.name != project {
            anyhow::bail!(
                "TOML 配置的 name 为 {project}，但 template = \"{}\" 的项目名固定为 {}",
                generated.kind.as_str(),
                generated.spec.name
            );
        }
        if generated.kind == TemplateKind::Static {
            attach_assets(
                &mut generated.files,
                request.assets,
                request.replace_assets,
                request.asset_permissions,
            );
        } else if request.assets_provided || !request.assets.is_empty() {
            anyhow::bail!("--assets 仅能与 static 模板一起使用；其它模板请使用 --files");
        }
        generated
            .files
            .extend(request.project_files.iter().map(|file| file.file.clone()));
        self.sync_files(&project, &generated)?;
        Ok(format!(
            "项目 {project} 的资源已同步；容器未重建，bind mount 内的文件已生效"
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
        let outcome =
            crate::import::import_compose(name, compose_yaml, env_file.unwrap_or(&preserved))?;
        let summary = outcome.summary();
        let spec = outcome.spec;
        let oidc_change = self.prepare_oidc_change(&spec)?;
        self.deploy(&spec, &[], true)?;
        let (oidc_updated, oidc_restarted) = self.apply_oidc_change(name, oidc_change, false)?;
        if start {
            self.recreate_if_running(name)?;
        }
        Ok(format!(
            "项目 {name} 已导入{}{}",
            oidc_update_suffix(oidc_updated, oidc_restarted),
            summary
        ))
    }

    /// 将当前项目状态导出为规范化 TOML。
    ///
    /// `keep_comments` 为真时在结果前追加当前版本的带注释骨架。
    ///
    /// # 错误
    ///
    /// 当前 Compose 状态无法由模板表示时返回错误。
    pub fn export(&self, name: &str, keep_comments: bool) -> anyhow::Result<String> {
        let spec = self.load(name)?;
        let output = template::export(&spec, &self.config)?;
        if !keep_comments {
            return Ok(output);
        }
        let kind = template::detect_kind(&spec)?;
        template::annotate(&output, kind)
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
        docker::compose_up(&self.config, &self.existing_project_dir(name)?, None, false)?;
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
        let oidc_change = self.prepare_oidc_removal(name)?;
        let (oidc_updated, oidc_restarted) = self.apply_oidc_change(name, oidc_change, false)?;
        Ok(format!(
            "项目 {name} 已删除；bind mount 数据未删除{}",
            oidc_update_suffix(oidc_updated, oidc_restarted)
        ))
    }

    /// 依次执行项目的启动钩子。
    ///
    /// 钩子在项目目录中通过 `sh -c` 执行，因此可以使用 shell 语法与项目内相对路径。
    /// 成功时完整回显钩子的 stdout 与 stderr，失败时错误里包含退出码、完整输出与
    /// 当前容器状态，避免把「已创建容器但钩子失败」误读成什么都没发生。
    ///
    /// # 错误
    ///
    /// 钩子记录无效、钩子无法启动或任一钩子返回非零退出码时返回错误。
    fn run_hooks(
        &self,
        name: &str,
        hooks: &[(String, String)],
        stage: HookStage,
        report: &dyn Fn(&str),
    ) -> anyhow::Result<Option<()>> {
        if hooks.is_empty() {
            return Ok(None);
        }
        let directory = self.existing_project_dir(name)?;
        for (service, command) in hooks {
            let outcome = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&directory)
                .output()
                .with_context(|| format!("无法执行服务 {service} 的 {} 钩子", stage.label()))?;
            let stdout = String::from_utf8_lossy(&outcome.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&outcome.stderr).trim().to_string();
            // 输出在成功与失败两种情况下都要回显：先投递再判断退出码，失败时
            // 错误信息里还有退出码、stderr 与容器状态。
            report_hook_output(report, service, stage.label(), &stdout, &stderr);
            if !outcome.status.success() {
                anyhow::bail!(
                    "服务 {service} 的 {} 钩子失败，退出码 {:?}：{}{}{}",
                    stage.label(),
                    outcome.status.code(),
                    if stderr.is_empty() { "" } else { "stderr: " },
                    stderr,
                    self.container_state_hint(name)?
                );
            }
        }
        Ok(Some(()))
    }

    /// 钩子失败时报告本次操作后的容器状态与补救方式。
    ///
    /// # 错误
    ///
    /// 项目目录检查失败时返回错误。
    fn container_state_hint(&self, name: &str) -> anyhow::Result<String> {
        let directory = self.project_dir(name)?;
        if !directory.join(crate::constants::COMPOSE_FILE).is_file() {
            return Ok(String::from(
                "；项目目录不存在，本次未创建任何容器；请修正钩子后重新执行 nsetup up",
            ));
        }
        // 钩子失败发生在部署之后，项目目录与 compose.yaml 都已按本次声明落地；
        // 容器可能已被创建甚至启动，因此必须明确说清状态而不是只报钩子错误。
        let state = match docker::compose_ps(&self.config, &directory) {
            Ok(status) if docker::compose_has_running(&self.config, &directory) => {
                format!("容器已创建并正在运行（{}）", status.trim())
            }
            Ok(status) if status.trim().is_empty() => String::from("容器尚未创建"),
            Ok(status) => format!("容器已创建但未运行（{}）", status.trim()),
            Err(_) => String::from("容器状态未知（无法读取 docker compose ps）"),
        };
        Ok(format!(
            "；项目 {name} 的声明已部署，{state}；可用 nsetup show {name} 查看状态，\
             修正钩子后重新执行 nsetup up -f <文件> --force"
        ))
    }

    /// 项目当前存在运行中容器时以 `--force-recreate` 重新创建它们。
    ///
    /// 受管项目目录是整体原子替换的：正在运行的容器仍持有旧目录 inode，改名后
    /// bind mount 会看到被删除的空目录，直到容器被重新创建。因此应用完成后必须
    /// 重新创建容器，否则静态站点等挂载会短暂或持续显示为空。
    ///
    /// # 错误
    ///
    /// Docker Compose 执行失败时返回错误。
    fn recreate_if_running(&self, name: &str) -> anyhow::Result<()> {
        let directory = self.existing_project_dir(name)?;
        let recreate = docker::compose_has_running(&self.config, &directory);
        docker::compose_up(&self.config, &directory, None, recreate)
    }
}

/// 一次 `up` 应用请求。
pub struct ApplyRequest<'a> {
    /// 完整的 nsetup TOML 配置。
    pub config_toml: &'a str,
    /// 客户端上传的静态站点文件。
    pub assets: &'a [Asset],
    /// 客户端是否显式提供了 `--assets`。
    pub assets_provided: bool,
    /// 上传的站点文件是否整体替换既有站点目录。
    pub replace_assets: bool,
    /// 上传的站点文件在项目目录中使用的权限策略。
    pub asset_permissions: crate::template::AssetPermissions,
    /// 客户端上传的项目附属文件。
    pub project_files: &'a [FilesUpload],
    /// `files/` 在容器内的挂载目标。
    pub files_into: &'a str,
    /// 是否允许整体替换已存在的同名项目。
    pub force: bool,
    /// 写入成功后是否立即启动项目。
    pub start: bool,
    /// Authelia OIDC 客户端变化后是否顺带重启 authelia。
    pub restart_dependents: bool,
    /// 只重新上传 `--files` / `--assets`，保留现有容器。
    pub files_only: bool,
    /// 接收中间进度（钩子输出）的回调。
    pub report: &'a dyn Fn(&str),
}

/// 从 TOML 声明中读出目标项目名，用于在解析模板前提示目录相关错误。
///
/// `traefik` / `authelia` 模板的项目名固定，可以省略 `name`，这里返回 `None`，
/// 由模板解析结果决定最终项目名。
///
/// # 错误
///
/// `name` 存在但不是字符串，或 `template` 值未知时返回错误。
fn project_directory_from_toml(config_toml: &str) -> anyhow::Result<Option<String>> {
    template::resolve_project_name(config_toml, None)
}

/// 把上传的站点文件按指定权限追加为受管附属文件。
///
/// 目录与文件权限默认取 `a+rX`（`0755` / `0644`）：静态站点目录会整体挂进容器，
/// 官方 nginx 镜像的 worker 是非 root 用户，目录 `0750` 会让它无法穿行。
fn attach_assets(
    files: &mut Vec<GeneratedFile>,
    assets: &[Asset],
    replace: bool,
    permissions: crate::template::AssetPermissions,
) {
    let (directory_mode, file_mode) = permissions.modes();
    for asset in assets {
        files.push(GeneratedFile {
            path: PathBuf::from("site").join(&asset.path),
            content: asset.content.clone(),
            mode: file_mode,
            directory_mode,
            replace,
            // `merge` 也要覆盖同名文件，`replace` 只决定是否清空既有目录。
            overwrite: true,
        });
    }
}

/// 把钩子的 stdout / stderr 逐行交给调用方作为进度信息展示。
fn report_hook_output(
    report: &dyn Fn(&str),
    service: &str,
    stage: &str,
    stdout: &str,
    stderr: &str,
) {
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        report(&format!("服务 {service} 的 {stage} 钩子输出: {line}"));
    }
    for line in stderr.lines().filter(|line| !line.trim().is_empty()) {
        report(&format!("服务 {service} 的 {stage} 钩子错误输出: {line}"));
    }
}

/// 返回 OIDC 客户端片段变更后的运维提示。
///
/// `updated` 表示 Authelia 项目状态确实发生变化；`restarted` 表示本次已经顺带
/// 重启 authelia，此时不应再提示用户「重启后生效」。
pub(super) const fn oidc_update_suffix(updated: bool, restarted: bool) -> &'static str {
    if !updated {
        ""
    } else if restarted {
        "；Authelia OIDC 配置已更新，已重启 authelia 使新客户端生效"
    } else {
        "；Authelia OIDC 配置已更新，重启 authelia 后生效（nsetup restart authelia）"
    }
}

#[cfg(test)]
mod tests {
    use super::oidc_update_suffix;

    /// 提示文案必须与实际是否重启 Authelia 一致（R4）。
    ///
    /// 自动流程在片段变化时总会顺带重启，因此第二组断言是防回归：一旦以后取消
    /// 自动重启，文案必须退回「重启后生效 + 确切命令」，不能继续说已重启。
    #[test]
    fn oidc_suffix_matches_actual_restart() {
        assert_eq!(oidc_update_suffix(false, false), "");
        let restarted = oidc_update_suffix(true, true);
        assert!(restarted.contains("已重启 authelia"), "{restarted}");
        assert!(!restarted.contains("重启 authelia 后生效"), "{restarted}");
        let pending = oidc_update_suffix(true, false);
        assert!(pending.contains("nsetup restart authelia"), "{pending}");
        assert!(!pending.contains("已重启"), "{pending}");
    }
}
