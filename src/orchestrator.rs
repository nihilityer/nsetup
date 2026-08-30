//! 项目编排、校验、编辑与单一部署收口。

use crate::config::{Config, set_mode};
use crate::constants::{COMPOSE_FILE, ENV_FILE, PROXY_NETWORK};
use crate::docker::{self, PullProgress};
use crate::spec::{
    BindMount, Document, Healthcheck, Network, PublishedPort, Route, Service, StackSpec,
    validate_name,
};
use crate::template::{self, GeneratedFile, TemplateKind};
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

/// daemon 侧有状态项目管理器。
#[derive(Debug, Clone)]
pub struct Orchestrator {
    /// 已校验的 daemon 配置。
    config: Config,
}

/// 随应用请求上传的附属文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// 站点相对路径。
    pub path: PathBuf,
    /// 文件原始字节。
    pub content: Vec<u8>,
}

/// 服务局部修改。
#[derive(Debug, Clone, Default)]
pub struct Edit {
    /// 可选服务选择器。
    pub service: Option<String>,
    /// 可选的新镜像仓库。
    pub image: Option<String>,
    /// 可选的新镜像版本。
    pub version: Option<String>,
    /// 可选的完整命令替换。
    pub command: Option<Vec<String>>,
    /// 可选的默认目标端口，应用于现有路由。
    pub container_port: Option<u16>,
    /// 可选的完整路由替换。
    pub routes: Option<Vec<Route>>,
    /// 可选的完整发布端口替换。
    pub published_ports: Option<Vec<PublishedPort>>,
    /// 追加到服务的 bind mount。
    pub volumes: Vec<BindMount>,
    /// 按键合并的环境变量。
    pub environment: BTreeMap<String, String>,
    /// 可选网络修改。
    pub network: Option<NetworkEdit>,
    /// 可选的中间件替换，应用于全部保留路由。
    pub middlewares: Option<Vec<String>>,
    /// 按键替换的自定义 label。
    pub labels: Vec<String>,
    /// 可选的新健康检查。
    pub healthcheck: Option<Healthcheck>,
    /// 是否移除当前健康检查。
    pub remove_healthcheck: bool,
    /// 是否启动编辑后的服务。
    pub start: bool,
}

/// `edit` 使用的网络修改。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkEdit {
    /// Compose bridge 网络；存在路由时同时加入代理网络。
    Bridge,
    /// 宿主机网络。
    Host,
    /// 指定名称的外部网络。
    External(String),
}

/// `list` 和 `get` 返回的项目信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackInfo {
    /// 项目名。
    pub name: String,
    /// 服务名列表。
    pub services: Vec<String>,
    /// 规范化 Compose YAML。
    pub compose_yaml: String,
    /// 项目 `.env` 键值。
    pub environment: BTreeMap<String, String>,
    /// 尽力获取的 Docker Compose 状态 JSON。
    pub status: String,
}

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
        self.deploy(&generated.spec, &generated.files, force)?;
        if start {
            docker::compose_up(&self.config, &self.project_dir(&generated.spec.name)?, None)?;
        }
        Ok(format!("项目 {} 已应用", generated.spec.name))
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
        self.deploy(&spec, &[], true)?;
        if start {
            docker::compose_up(&self.config, &directory, None)?;
        }
        Ok(format!("项目 {name} 已导入"))
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

    /// 应用服务局部修改，并按需启动该服务。
    ///
    /// # 错误
    ///
    /// 修改无效时在替换状态前返回错误。
    pub fn edit(&self, name: &str, edit: Edit) -> anyhow::Result<String> {
        let mut spec = self.load(name)?;
        let service_name = select_service(&spec.document, edit.service.as_deref())?;
        let mut service = spec
            .document
            .services
            .remove(&service_name)
            .ok_or_else(|| anyhow::anyhow!("服务不存在: {service_name}"))?;
        if edit.image.is_some() || edit.version.is_some() {
            service.set_image_version(edit.image.as_deref(), edit.version.as_deref())?;
        }
        if let Some(command) = edit.command {
            service.command = command;
        }
        let mut routes = if let Some(routes) = edit.routes {
            routes
        } else {
            service.routes()?
        };
        if let Some(port) = edit.container_port {
            if port == 0 {
                anyhow::bail!("容器端口不能为 0");
            }
            for route in &mut routes {
                route.container_port = port;
            }
        }
        if let Some(middlewares) = edit.middlewares {
            validate_middlewares(&middlewares)?;
            for route in &mut routes {
                route.middlewares.clone_from(&middlewares);
            }
        }
        if let Some(ports) = edit.published_ports {
            service.ports = ports.iter().map(PublishedPort::compose_value).collect();
        }
        service
            .volumes
            .extend(edit.volumes.iter().map(BindMount::compose_value));
        service.environment.extend(edit.environment);
        replace_labels(&mut service.labels, &edit.labels)?;
        if edit.remove_healthcheck && edit.healthcheck.is_some() {
            anyhow::bail!("不能同时设置和移除 healthcheck");
        }
        if edit.remove_healthcheck {
            service.healthcheck = None;
        } else if edit.healthcheck.is_some() {
            service.healthcheck = edit.healthcheck;
        }
        if let Some(network) = edit.network {
            apply_network(
                &mut spec.document,
                &service_name,
                &mut service,
                network,
                !routes.is_empty(),
            )?;
        } else if service.network_mode.as_deref() == Some("host") && !routes.is_empty() {
            anyhow::bail!("host 网络模式不能使用 Traefik 容器路由");
        }
        if !routes.is_empty() && !service.networks.iter().any(|name| name == "proxy") {
            add_proxy_network(&mut spec.document);
            service.networks.push(String::from("proxy"));
        }
        service.set_routes(name, &service_name, &routes)?;
        spec.document.services.insert(service_name.clone(), service);
        clean_unused_networks(&mut spec.document);
        spec.validate()?;
        self.deploy(&spec, &[], true)?;
        if edit.start {
            docker::compose_up(&self.config, &self.project_dir(name)?, Some(&service_name))?;
        }
        Ok(format!("项目 {name} 的服务 {service_name} 已更新"))
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
    pub fn pull(&self, name: &str, report: impl FnMut(PullProgress) -> bool) -> anyhow::Result<()> {
        docker::compose_pull(&self.config, &self.existing_project_dir(name)?, report)
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
    ) -> anyhow::Result<()> {
        docker::compose_logs(
            &self.config,
            &self.existing_project_dir(name)?,
            tail,
            follow,
            report,
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
        Ok(format!("项目 {name} 已删除；bind mount 数据未删除"))
    }

    /// 执行全部检查、暂存全部文件并原子提交一个项目。
    fn deploy(&self, spec: &StackSpec, files: &[GeneratedFile], force: bool) -> anyhow::Result<()> {
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
    fn load(&self, name: &str) -> anyhow::Result<StackSpec> {
        StackSpec::load(&self.existing_project_dir(name)?)
    }

    /// 在 `stacks_root` 下解析已校验的项目名。
    fn project_dir(&self, name: &str) -> anyhow::Result<PathBuf> {
        validate_name("项目名", name)?;
        Ok(self.config.stacks_root.join(name))
    }

    /// 解析项目目录并要求该目录存在。
    fn existing_project_dir(&self, name: &str) -> anyhow::Result<PathBuf> {
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

/// 选择明确指定的服务，或选择文档中的唯一服务。
fn select_service(document: &Document, requested: Option<&str>) -> anyhow::Result<String> {
    if let Some(name) = requested {
        validate_name("服务名", name)?;
        if !document.services.contains_key(name) {
            anyhow::bail!("服务不存在: {name}");
        }
        return Ok(name.to_string());
    }
    if document.services.len() != 1 {
        anyhow::bail!("多服务项目必须使用 --service 指定服务");
    }
    document
        .services
        .keys()
        .next()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("项目没有服务"))
}

/// 按键替换自定义 label，同时保护生成的元数据。
fn replace_labels(target: &mut Vec<String>, replacements: &[String]) -> anyhow::Result<()> {
    let mut map = labels_to_map(target)?;
    for label in replacements {
        let (key, value) = label
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("label 必须是 KEY=VALUE: {label}"))?;
        if key.starts_with("traefik.http.routers.nsetup-")
            || key.starts_with("traefik.http.services.nsetup-")
            || key == "io.nsetup.template"
        {
            anyhow::bail!("不能直接修改 nsetup 生成标签: {key}");
        }
        map.insert(key.to_string(), value.to_string());
    }
    *target = map
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    Ok(())
}

/// 将列表形式的 label 解析为确定顺序的键值映射。
fn labels_to_map(values: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    for value in values {
        let (key, content) = value
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("label 必须是 KEY=VALUE: {value}"))?;
        output.insert(key.to_string(), content.to_string());
    }
    Ok(output)
}

/// 对暂时从文档移出的服务应用一次逻辑网络修改。
fn apply_network(
    document: &mut Document,
    service_name: &str,
    service: &mut Service,
    network: NetworkEdit,
    has_routes: bool,
) -> anyhow::Result<()> {
    service.network_mode = None;
    service.networks.clear();
    match network {
        NetworkEdit::Bridge if has_routes => {
            add_proxy_network(document);
            service.networks.push(String::from("proxy"));
        }
        NetworkEdit::Bridge => {}
        NetworkEdit::Host if has_routes => {
            anyhow::bail!("host 网络模式不能使用 Traefik 容器路由");
        }
        NetworkEdit::Host => service.network_mode = Some(String::from("host")),
        NetworkEdit::External(name) => {
            let key = format!("external-{service_name}");
            document.networks.insert(
                key.clone(),
                Network {
                    external: true,
                    name: Some(name),
                },
            );
            service.networks.push(key);
            if has_routes {
                add_proxy_network(document);
                service.networks.push(String::from("proxy"));
            }
        }
    }
    Ok(())
}

/// 确保共享外部代理网络定义存在。
fn add_proxy_network(document: &mut Document) {
    document.networks.insert(
        String::from("proxy"),
        Network {
            external: true,
            name: Some(String::from(PROXY_NETWORK)),
        },
    );
}

/// 编辑后移除没有任何服务引用的顶层网络。
fn clean_unused_networks(document: &mut Document) {
    let used: BTreeSet<String> = document
        .services
        .values()
        .flat_map(|service| service.networks.iter().cloned())
        .collect();
    document.networks.retain(|name, _| used.contains(name));
}

/// 根据内置 Traefik 注册表检查中间件名称。
fn validate_middlewares(values: &[String]) -> anyhow::Result<()> {
    for value in values {
        if !matches!(
            value.as_str(),
            "gzip" | "forwarded-headers" | "internal-only" | "tls"
        ) {
            anyhow::bail!("未知内置 Traefik middleware: {value}");
        }
    }
    Ok(())
}

/// 对可能不存在的路径，解析其最长现有前缀中的符号链接。
fn resolve_existing_prefix(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("路径必须是绝对路径: {}", path.display());
    }
    let normalized = lexical_normalize(path)?;
    let mut existing = normalized.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("无法解析路径: {}", path.display()))?;
        suffix.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("无法解析路径: {}", path.display()))?;
    }
    let mut resolved = existing
        .canonicalize()
        .with_context(|| format!("无法解析路径: {}", existing.display()))?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

/// 移除当前目录和父目录分量，同时禁止越出根目录。
fn lexical_normalize(path: &Path) -> anyhow::Result<PathBuf> {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => output.push(Path::new("/")),
            Component::Normal(value) => output.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    anyhow::bail!("路径越出根目录: {}", path.display());
                }
            }
            Component::Prefix(_) => anyhow::bail!("不支持的平台路径: {}", path.display()),
        }
    }
    Ok(output)
}

/// 校验全部生成的附属文件路径并拒绝重复项。
fn validate_generated_files(files: &[GeneratedFile]) -> anyhow::Result<()> {
    let mut paths = BTreeSet::new();
    for file in files {
        validate_relative_path(&file.path)?;
        if !paths.insert(file.path.clone()) {
            anyhow::bail!("附属文件路径重复: {}", file.path.display());
        }
    }
    Ok(())
}

/// 要求路径为非空相对路径，且仅包含普通分量。
fn validate_relative_path(path: &Path) -> anyhow::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        anyhow::bail!("附属文件路径不安全: {}", path.display());
    }
    Ok(())
}

/// 不跟随符号链接，将不属于 IR 的项目文件复制到暂存目录。
fn copy_auxiliary(source: &Path, target: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == COMPOSE_FILE || entry.file_name() == ENV_FILE {
            continue;
        }
        let metadata = entry.metadata()?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_symlink() {
            anyhow::bail!("项目附属路径不能是符号链接: {}", entry.path().display());
        }
        if metadata.is_dir() {
            fs::create_dir(&destination)?;
            set_mode(&destination, 0o750)?;
            copy_auxiliary(&entry.path(), &destination)?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), &destination)?;
            set_mode(&destination, metadata.permissions().mode() & 0o777)?;
        } else {
            anyhow::bail!("项目附属路径类型不受支持: {}", entry.path().display());
        }
    }
    Ok(())
}

/// 将一个已校验的模板附属文件写入暂存目录。
fn write_attachment(root: &Path, file: &GeneratedFile) -> anyhow::Result<()> {
    validate_relative_path(&file.path)?;
    let destination = root.join(&file.path);
    if !file.replace && destination.is_file() {
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("附属文件缺少父目录"))?;
    create_safe_directories(root, parent)?;
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        anyhow::bail!("拒绝覆盖非普通文件: {}", destination.display());
    }
    write_project_file(&destination, &file.content, file.mode)
}

/// 创建附属文件目录链，同时拒绝符号链接。
fn create_safe_directories(root: &Path, destination: &Path) -> anyhow::Result<()> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("附属文件越出项目目录"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                anyhow::bail!("附属目录不安全: {}", current.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                set_mode(&current, 0o750)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// 写入并同步暂存文件，然后应用明确权限。
fn write_project_file(path: &Path, content: &[u8], mode: u32) -> anyhow::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true).mode(mode);
    let mut file = options
        .open(path)
        .with_context(|| format!("无法写入文件: {}", path.display()))?;
    file.write_all(content)?;
    file.sync_all()?;
    set_mode(path, mode)
}

/// 生成不易冲突的隐藏同级路径。
fn sibling_temporary(target: &Path, kind: &str) -> anyhow::Result<PathBuf> {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("目标项目路径无效: {}", target.display()))?;
    Ok(target.with_file_name(format!(
        ".{name}.{kind}-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    )))
}

use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
mod tests {
    use super::{lexical_normalize, validate_relative_path};
    use std::path::Path;

    #[test]
    fn rejects_asset_traversal() {
        assert!(validate_relative_path(Path::new("../secret")).is_err());
        assert!(validate_relative_path(Path::new("site/index.html")).is_ok());
    }

    #[test]
    fn normalizes_parent_components() -> anyhow::Result<()> {
        assert_eq!(
            lexical_normalize(Path::new("/srv/data/one/../two"))?,
            Path::new("/srv/data/two")
        );
        Ok(())
    }
}
