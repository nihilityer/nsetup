//! 强类型 Compose 中间表示与语义视图。

use crate::config::validate_domain;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// 完整的受管项目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackSpec {
    /// Compose 项目名。
    pub name: String,
    /// 类型化 Compose 文档。
    pub document: Document,
    /// 持久化到 `.env` 的变量。
    pub environment: BTreeMap<String, String>,
}

/// 受支持的顶层 Compose 文档。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Document {
    /// 以 Compose 服务名为键的服务映射。
    pub services: BTreeMap<String, Service>,
    /// 以逻辑名称为键的项目网络映射。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub networks: BTreeMap<String, Network>,
}

/// 受支持的 Compose 服务字段。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// 包含完整标签的镜像引用。
    pub image: String,
    /// 可选的固定容器名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    /// 替换镜像默认命令的参数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Compose 重启策略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
    /// 宿主机或其他直接 Docker 网络模式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_mode: Option<String>,
    /// 服务加入的具名 Compose 网络。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<String>,
    /// 短语法发布端口映射。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    /// 短语法 bind mount。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    /// 映射形式的容器环境变量。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// 容器读取的环境变量文件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_file: Vec<String>,
    /// 列表形式的 Docker label。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// 可选容器健康检查。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<Healthcheck>,
    /// 可选 Docker 日志配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<Logging>,
}

/// 受支持的 Compose 网络定义。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// 是否要求 Docker 中已存在该网络。
    #[serde(default, skip_serializing_if = "is_false")]
    pub external: bool,
    /// 可选的固定 Docker 网络名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 受支持的 Compose 健康检查定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Healthcheck {
    /// Compose 健康检查测试，通常为 `CMD-SHELL` 加一条命令。
    pub test: Vec<String>,
    /// 检查间隔。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// 超时时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// 启动宽限期。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_period: Option<String>,
    /// 重试次数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
}

/// 受支持的 Compose 日志定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Logging {
    /// Docker 日志驱动。
    pub driver: String,
    /// 驱动专用选项。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
}

/// 从 label 派生的 Traefik 语义路由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// 此路由器处理的 DNS 主机名。
    pub hosts: Vec<String>,
    /// 可选 URL 路径前缀。
    pub path_prefix: Option<String>,
    /// 目标容器端口。
    pub container_port: u16,
    /// 不含提供者后缀的中间件名称。
    pub middlewares: Vec<String>,
    /// 后端协议。
    pub protocol: RouteProtocol,
    /// 是否启用负载均衡粘性 Cookie。
    pub sticky_cookie: bool,
    /// 可选的 `passHostHeader` 覆盖值。
    pub pass_host_header: Option<bool>,
    /// 可选路由优先级。
    pub priority: Option<u32>,
}

/// 受支持的 Traefik 后端协议。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RouteProtocol {
    /// 普通 HTTP。
    #[default]
    Http,
    /// 连接后端的 HTTPS。
    Https,
    /// 明文 HTTP/2。
    H2c,
}

impl RouteProtocol {
    /// 返回 Traefik scheme 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::H2c => "h2c",
        }
    }

    /// 解析受支持的协议。
    ///
    /// # 错误
    ///
    /// 值不受支持时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            "h2c" => Ok(Self::H2c),
            _ => anyhow::bail!("不支持的路由协议: {value}"),
        }
    }
}

/// 用于校验和编辑的已解析发布端口。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublishedPort {
    /// 可选的宿主机 IP 绑定。
    pub host_ip: Option<String>,
    /// 宿主机端口。
    pub host_port: u16,
    /// 容器端口。
    pub container_port: u16,
    /// 传输协议。
    pub protocol: PortProtocol,
}

/// 受支持的发布端口协议。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PortProtocol {
    /// 传输控制协议（TCP）。
    #[default]
    Tcp,
    /// 用户数据报协议（UDP）。
    Udp,
}

impl PortProtocol {
    /// 返回 Compose 后缀。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// 已解析的 bind mount。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMount {
    /// 宿主机源绝对路径。
    pub host_path: String,
    /// 容器目标绝对路径。
    pub container_path: String,
    /// 只读标志。
    pub read_only: bool,
}

impl StackSpec {
    /// 将 Compose YAML 和可选环境文件内容解析为 IR。
    ///
    /// # 错误
    ///
    /// 字段不受支持或状态无效时返回包含上下文的错误。
    pub fn parse(name: &str, compose_yaml: &str, env_file: &str) -> anyhow::Result<Self> {
        validate_name("项目名", name)?;
        let document: Document =
            serde_yaml::from_str(compose_yaml).context("Compose YAML 含不支持的字段或值")?;
        let spec = Self {
            name: name.to_string(),
            document,
            environment: parse_env_file(env_file)?,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// 从项目目录加载 `compose.yaml` 和 `.env`。
    ///
    /// # 错误
    ///
    /// 文件缺失、无法读取或内容无效时返回错误。
    pub fn load(directory: &Path) -> anyhow::Result<Self> {
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("无法从目录取得项目名: {}", directory.display()))?;
        let compose = std::fs::read_to_string(directory.join(COMPOSE_FILE))
            .with_context(|| format!("无法读取 {COMPOSE_FILE}: {}", directory.display()))?;
        let env = match std::fs::read_to_string(directory.join(ENV_FILE)) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        Self::parse(name, &compose, &env)
    }

    /// 校验 IR 表示的名称、镜像、端口、label 和挂载。
    ///
    /// # 错误
    ///
    /// 状态无法安全管理和重新生成时返回错误。
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_name("项目名", &self.name)?;
        if self.document.services.is_empty() {
            anyhow::bail!("Compose 至少需要一个服务");
        }
        for (name, network) in &self.document.networks {
            validate_docker_object_name("Compose 网络名", name)?;
            if let Some(docker_name) = &network.name {
                validate_docker_object_name("Docker 网络名", docker_name)?;
            }
        }
        for (name, service) in &self.document.services {
            validate_name("服务名", name)?;
            if let Some(container_name) = &service.container_name {
                validate_docker_object_name("container_name", container_name)?;
            }
            let resolved = interpolate_image(&service.image, &self.environment)?;
            validate_tagged_image(&resolved)?;
            if service
                .network_mode
                .as_deref()
                .is_some_and(|mode| mode != "host")
            {
                anyhow::bail!("服务 {name} 使用了不支持的 network_mode");
            }
            for network in &service.networks {
                if !self.document.networks.contains_key(network) {
                    anyhow::bail!("服务 {name} 引用了未定义网络 {network}");
                }
            }
            let mut ports = BTreeSet::new();
            for value in &service.ports {
                let port = PublishedPort::parse(value)?;
                if !ports.insert((port.host_ip.clone(), port.host_port, port.protocol)) {
                    anyhow::bail!("服务 {name} 重复发布宿主机端口 {value}");
                }
            }
            for value in &service.volumes {
                let _mount = BindMount::parse(value)?;
            }
            for env_file in &service.env_file {
                validate_relative_compose_path(env_file)?;
            }
            validate_labels(&service.labels)?;
            if let Some(healthcheck) = &service.healthcheck {
                healthcheck.validate()?;
            }
            if let Some(logging) = &service.logging
                && logging.driver.trim().is_empty()
            {
                anyhow::bail!("服务 {name} 的 logging.driver 不能为空");
            }
            for route in service.routes()? {
                route.validate()?;
            }
        }
        let _route_hosts = self.route_hosts()?;
        Ok(())
    }

    /// 返回仅由 IR 生成的规范化 Compose YAML。
    ///
    /// # 错误
    ///
    /// 序列化意外失败时返回错误。
    pub fn compose_yaml(&self) -> anyhow::Result<String> {
        let mut output = serde_yaml::to_string(&self.document)?;
        if !output.ends_with('\n') {
            output.push('\n');
        }
        Ok(output)
    }

    /// 返回确定顺序的 `.env` 表示。
    #[must_use]
    pub fn env_file(&self) -> String {
        serialize_env_file(&self.environment)
    }

    /// 返回全部已发布宿主机端口。
    ///
    /// # 错误
    ///
    /// 存储的映射格式错误时返回错误。
    pub fn host_ports(&self) -> anyhow::Result<Vec<PublishedPort>> {
        self.document
            .services
            .values()
            .flat_map(|service| service.ports.iter())
            .map(|value| PublishedPort::parse(value))
            .collect()
    }

    /// 返回 Traefik label 声明的全部主机名。
    ///
    /// # 错误
    ///
    /// 路由 label 不一致时返回错误。
    pub fn route_hosts(&self) -> anyhow::Result<Vec<String>> {
        let mut hosts = Vec::new();
        for service in self.document.services.values() {
            for (key, value) in label_map(&service.labels)? {
                if key.starts_with("traefik.http.routers.")
                    && key.ends_with(".rule")
                    && value.contains("Host(")
                {
                    for host in parse_hosts(&value)? {
                        validate_domain(&host)?;
                        hosts.push(host);
                    }
                }
            }
        }
        Ok(hosts)
    }
}

impl Service {
    /// 返回镜像仓库和明确版本标签。
    ///
    /// # 错误
    ///
    /// 镜像引用由变量支撑或格式无效时返回错误。
    pub fn image_version(&self) -> anyhow::Result<(String, String)> {
        if self.image.contains("${") {
            anyhow::bail!("变量镜像不能直接反解版本: {}", self.image);
        }
        split_tagged_image(&self.image)
    }

    /// 替换镜像仓库和/或版本标签。
    ///
    /// # 错误
    ///
    /// 镜像由变量支撑或新值无效时返回错误。
    pub fn set_image_version(
        &mut self,
        repository: Option<&str>,
        version: Option<&str>,
    ) -> anyhow::Result<()> {
        let (current_repository, current_version) = self.image_version()?;
        let repository = repository.unwrap_or(&current_repository);
        if repository.contains(':')
            && repository
                .rsplit('/')
                .next()
                .is_some_and(|part| part.contains(':'))
        {
            anyhow::bail!("--image 必须是不含标签的镜像仓库: {repository}");
        }
        let version = version.unwrap_or(&current_version);
        validate_version(version)?;
        let image = format!("{repository}:{version}");
        validate_tagged_image(&image)?;
        self.image = image;
        Ok(())
    }

    /// 从 Traefik label 派生生成的路由。
    ///
    /// # 错误
    ///
    /// 生成的 label 含无效值时返回错误。
    pub fn routes(&self) -> anyhow::Result<Vec<Route>> {
        let labels = label_map(&self.labels)?;
        let mut routers = BTreeSet::new();
        for key in labels.keys() {
            if let Some(rest) = key.strip_prefix("traefik.http.routers.")
                && let Some(router) = rest.strip_suffix(".rule")
                && router.starts_with("nsetup-")
            {
                routers.insert(router.to_string());
            }
        }
        let mut routes = Vec::new();
        for router in routers {
            let prefix = format!("traefik.http.routers.{router}");
            let rule = labels
                .get(&format!("{prefix}.rule"))
                .ok_or_else(|| anyhow::anyhow!("路由 {router} 缺少 rule"))?;
            let hosts = parse_hosts(rule)?;
            let path_prefix = parse_path_prefix(rule);
            let backend = labels
                .get(&format!("{prefix}.service"))
                .cloned()
                .unwrap_or_else(|| router.clone());
            let service_prefix = format!("traefik.http.services.{backend}.loadbalancer");
            let container_port = labels
                .get(&format!("{service_prefix}.server.port"))
                .ok_or_else(|| anyhow::anyhow!("路由 {router} 缺少容器端口"))?
                .parse::<u16>()
                .with_context(|| format!("路由 {router} 容器端口无效"))?;
            let protocol = labels
                .get(&format!("{service_prefix}.server.scheme"))
                .map_or(Ok(RouteProtocol::Http), |value| RouteProtocol::parse(value))?;
            let middlewares = labels
                .get(&format!("{prefix}.middlewares"))
                .map(|value| {
                    value
                        .split(',')
                        .filter(|item| !item.is_empty())
                        .map(|item| item.strip_suffix("@file").unwrap_or(item).to_string())
                        .collect()
                })
                .unwrap_or_default();
            let sticky_cookie =
                parse_optional_bool(labels.get(&format!("{service_prefix}.sticky.cookie")))?
                    .unwrap_or(false);
            let pass_host_header =
                parse_optional_bool(labels.get(&format!("{service_prefix}.passhostheader")))?;
            let priority = labels
                .get(&format!("{prefix}.priority"))
                .map(|value| value.parse::<u32>().context("Traefik priority 无效"))
                .transpose()?;
            routes.push(Route {
                hosts,
                path_prefix,
                container_port,
                middlewares,
                protocol,
                sticky_cookie,
                pass_host_header,
                priority,
            });
        }
        Ok(routes)
    }

    /// 替换为此项目服务生成的全部 label。
    ///
    /// # 错误
    ///
    /// 路由无效或自定义 label 键重复时返回错误。
    pub fn set_routes(
        &mut self,
        stack_name: &str,
        service_name: &str,
        routes: &[Route],
    ) -> anyhow::Result<()> {
        let generated_prefix = format!("nsetup-{stack_name}-{service_name}-");
        let mut labels = label_map(&self.labels)?;
        labels.retain(|key, _| !is_generated_traefik_key(key) && key != "traefik.enable");
        if !routes.is_empty() {
            labels.insert(String::from("traefik.enable"), String::from("true"));
        }
        for (index, route) in routes.iter().enumerate() {
            route.validate()?;
            let name = format!("{generated_prefix}{}", index + 1);
            let router = format!("traefik.http.routers.{name}");
            let backend = format!("traefik.http.services.{name}.loadbalancer");
            let mut rule = format!(
                "Host({})",
                route
                    .hosts
                    .iter()
                    .map(|host| format!("`{host}`"))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            if let Some(path) = &route.path_prefix {
                rule.push_str(&format!(" && PathPrefix(`{path}`)"));
            }
            labels.insert(format!("{router}.rule"), rule);
            labels.insert(format!("{router}.entrypoints"), String::from("websecure"));
            labels.insert(format!("{router}.tls"), String::from("true"));
            labels.insert(format!("{router}.service"), name.clone());
            if !route.middlewares.is_empty() {
                labels.insert(
                    format!("{router}.middlewares"),
                    route
                        .middlewares
                        .iter()
                        .map(|value| format!("{value}@file"))
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
            if let Some(priority) = route.priority {
                labels.insert(format!("{router}.priority"), priority.to_string());
            }
            labels.insert(
                format!("{backend}.server.port"),
                route.container_port.to_string(),
            );
            if route.protocol != RouteProtocol::Http {
                labels.insert(
                    format!("{backend}.server.scheme"),
                    route.protocol.as_str().to_string(),
                );
            }
            if route.sticky_cookie {
                labels.insert(format!("{backend}.sticky.cookie"), String::from("true"));
            }
            if let Some(pass_host_header) = route.pass_host_header {
                labels.insert(
                    format!("{backend}.passhostheader"),
                    pass_host_header.to_string(),
                );
            }
        }
        self.labels = labels
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        Ok(())
    }
}

impl Route {
    /// 校验语义路由。
    ///
    /// # 错误
    ///
    /// 缺少主机名，或 DNS 名称、路径、端口无效时返回错误。
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.hosts.is_empty() {
            anyhow::bail!("Traefik 路由至少需要一个 host");
        }
        for host in &self.hosts {
            validate_domain(host)?;
        }
        if let Some(path) = &self.path_prefix
            && (!path.starts_with('/') || path.contains('`'))
        {
            anyhow::bail!("path_prefix 必须以 / 开头且不能含反引号: {path}");
        }
        if self.container_port == 0 {
            anyhow::bail!("路由容器端口必须在 1..=65535 范围内");
        }
        Ok(())
    }
}

impl PublishedPort {
    /// 解析 Docker Compose 短端口语法。
    ///
    /// # 错误
    ///
    /// 映射不受支持或超出范围时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let (mapping, protocol) = match value.rsplit_once('/') {
            Some((mapping, "tcp")) => (mapping, PortProtocol::Tcp),
            Some((mapping, "udp")) => (mapping, PortProtocol::Udp),
            Some((_mapping, protocol)) => anyhow::bail!("不支持的端口协议: {protocol}"),
            None => (value, PortProtocol::Tcp),
        };
        let parts: Vec<&str> = mapping.split(':').collect();
        let (host_ip, host, container) = match parts.as_slice() {
            [host, container] => (None, *host, *container),
            [host_ip, host, container] => (Some((*host_ip).to_string()), *host, *container),
            _ => anyhow::bail!("端口映射必须是 HOST:CONTAINER[/tcp|udp]: {value}"),
        };
        let host_port = parse_port(host, "宿主机端口")?;
        let container_port = parse_port(container, "容器端口")?;
        Ok(Self {
            host_ip,
            host_port,
            container_port,
            protocol,
        })
    }

    /// 将映射序列化为规范短语法。
    #[must_use]
    pub fn compose_value(&self) -> String {
        let address = self
            .host_ip
            .as_ref()
            .map(|value| format!("{value}:"))
            .unwrap_or_default();
        format!(
            "{address}{}:{}/{}",
            self.host_port,
            self.container_port,
            self.protocol.as_str()
        )
    }
}

impl BindMount {
    /// 解析 bind mount 并拒绝命名卷。
    ///
    /// # 错误
    ///
    /// 遇到命名卷、相对路径或无效目标时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = value.split(':').collect();
        let (host_path, container_path, read_only) = match parts.as_slice() {
            [host, container] => (*host, *container, false),
            [host, container, "ro"] => (*host, *container, true),
            [_, _, mode] => anyhow::bail!("卷挂载模式仅支持 ro: {mode}"),
            _ => anyhow::bail!("卷挂载格式必须是 HOST:CONTAINER[:ro]: {value}"),
        };
        if !Path::new(host_path).is_absolute() {
            anyhow::bail!("禁止命名卷或相对 bind mount: {host_path}");
        }
        if !Path::new(container_path).is_absolute() {
            anyhow::bail!("容器挂载目标必须是绝对路径: {container_path}");
        }
        Ok(Self {
            host_path: host_path.to_string(),
            container_path: container_path.to_string(),
            read_only,
        })
    }

    /// 将 bind mount 序列化为规范短语法。
    #[must_use]
    pub fn compose_value(&self) -> String {
        format!(
            "{}:{}{}",
            self.host_path,
            self.container_path,
            if self.read_only { ":ro" } else { "" }
        )
    }
}

impl Healthcheck {
    /// 构造 `CMD-SHELL` 健康检查。
    #[must_use]
    pub fn command(command: String) -> Self {
        Self {
            test: vec![String::from("CMD-SHELL"), command],
            interval: None,
            timeout: None,
            start_period: None,
            retries: None,
        }
    }

    /// 当前健康检查受支持时返回 shell 命令。
    ///
    /// # 错误
    ///
    /// 健康检查不是 `CMD-SHELL` 形式时返回错误。
    pub fn shell_command(&self) -> anyhow::Result<&str> {
        match self.test.as_slice() {
            [kind, command] if kind == "CMD-SHELL" && !command.is_empty() => Ok(command),
            _ => anyhow::bail!("healthcheck.test 只支持 [CMD-SHELL, command]"),
        }
    }

    /// 校验受支持的健康检查表示。
    fn validate(&self) -> anyhow::Result<()> {
        let _command = self.shell_command()?;
        if self.retries == Some(0) {
            anyhow::bail!("healthcheck.retries 必须大于 0");
        }
        Ok(())
    }
}

/// 校验受管项目名或服务名。
///
/// # 错误
///
/// 违反保守命名规则时返回错误。
pub fn validate_name(label: &str, value: &str) -> anyhow::Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 63
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte)
        });
    if !valid {
        anyhow::bail!("{label}无效: {value}（需小写字母开头，仅含小写字母、数字、-、_，最长 63）");
    }
    Ok(())
}

/// 校验明确的镜像版本。
///
/// # 错误
///
/// 标签为 `latest`、为空、过长或格式无效时返回错误。
pub fn validate_version(version: &str) -> anyhow::Result<()> {
    let valid = !version.is_empty()
        && version != "latest"
        && version.len() <= 128
        && (version.as_bytes()[0].is_ascii_alphanumeric() || version.as_bytes()[0] == b'_')
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        anyhow::bail!("镜像版本无效或未钉定: {version}");
    }
    Ok(())
}

/// 拆分包含完整标签的镜像引用。
///
/// # 错误
///
/// 缺少明确标签或标签无效时返回错误。
pub fn split_tagged_image(image: &str) -> anyhow::Result<(String, String)> {
    if image.contains('@') {
        anyhow::bail!("镜像必须使用显式版本标签，不能使用摘要: {image}");
    }
    let slash = image.rfind('/');
    let colon = image
        .rfind(':')
        .filter(|index| slash.is_none_or(|slash_index| *index > slash_index))
        .ok_or_else(|| anyhow::anyhow!("镜像缺少显式版本标签: {image}"))?;
    let repository = &image[..colon];
    let version = &image[colon + 1..];
    if repository.is_empty() || repository.chars().any(char::is_whitespace) {
        anyhow::bail!("镜像仓库无效: {repository}");
    }
    validate_version(version)?;
    Ok((repository.to_string(), version.to_string()))
}

/// 校验包含完整标签的镜像，并丢弃拆分结果。
fn validate_tagged_image(image: &str) -> anyhow::Result<()> {
    let _parts = split_tagged_image(image)?;
    Ok(())
}

/// 使用项目环境状态解析镜像中的 `${KEY}` 片段。
fn interpolate_image(
    image: &str,
    environment: &BTreeMap<String, String>,
) -> anyhow::Result<String> {
    let mut result = String::new();
    let mut remaining = image;
    while let Some(start) = remaining.find("${") {
        result.push_str(&remaining[..start]);
        let after = &remaining[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("镜像变量缺少 }}: {image}"))?;
        let key = &after[..end];
        let value = environment
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("镜像变量 {key} 未在 .env 中定义"))?;
        result.push_str(value);
        remaining = &after[end + 1..];
    }
    result.push_str(remaining);
    Ok(result)
}

/// 解析 nsetup 管理的保守 `.env` 子集。
fn parse_env_file(input: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    for (index, original) in input.lines().enumerate() {
        let line = original.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, raw_value) = line
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!(".env 第 {} 行缺少 =", index + 1))?;
        validate_env_key(key)?;
        let value = unquote_env(raw_value)?;
        if output.insert(key.to_string(), value).is_some() {
            anyhow::bail!(".env 重复定义变量 {key}");
        }
    }
    Ok(output)
}

/// 使用转义双引号，以确定顺序序列化项目变量。
fn serialize_env_file(environment: &BTreeMap<String, String>) -> String {
    let mut output = String::new();
    for (key, value) in environment {
        let escaped = value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        output.push_str(&format!("{key}=\"{escaped}\"\n"));
    }
    output
}

/// 移除受支持的 `.env` 引号和转义序列。
fn unquote_env(value: &str) -> anyhow::Result<String> {
    if value.starts_with('"') {
        if !value.ends_with('"') || value.len() < 2 {
            anyhow::bail!(".env 双引号未闭合");
        }
        let mut output = String::new();
        let mut escaped = false;
        for character in value[1..value.len() - 1].chars() {
            if escaped {
                output.push(match character {
                    'n' => '\n',
                    'r' => '\r',
                    other => other,
                });
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else {
                output.push(character);
            }
        }
        if escaped {
            anyhow::bail!(".env 转义不完整");
        }
        Ok(output)
    } else if value.starts_with('\'') {
        if !value.ends_with('\'') || value.len() < 2 {
            anyhow::bail!(".env 单引号未闭合");
        }
        Ok(value[1..value.len() - 1].to_string())
    } else {
        Ok(value.trim().to_string())
    }
}

/// 校验可移植的环境变量键。
fn validate_env_key(key: &str) -> anyhow::Result<()> {
    let mut bytes = key.bytes();
    let first = bytes
        .next()
        .ok_or_else(|| anyhow::anyhow!("环境变量名不能为空"))?;
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        anyhow::bail!("环境变量名无效: {key}");
    }
    Ok(())
}

/// 校验列表形式的 Docker label 和重复键。
fn validate_labels(labels: &[String]) -> anyhow::Result<()> {
    let _labels = label_map(labels)?;
    Ok(())
}

/// 将列表形式的 Docker label 解析为确定顺序的映射。
fn label_map(labels: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    for label in labels {
        let (key, value) = label
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Docker label 必须是 KEY=VALUE: {label}"))?;
        if key.is_empty() {
            anyhow::bail!("Docker label 键不能为空");
        }
        if output.insert(key.to_string(), value.to_string()).is_some() {
            anyhow::bail!("Docker label 重复: {key}");
        }
    }
    Ok(output)
}

/// 从 Traefik 规则提取主机名参数。
fn parse_hosts(rule: &str) -> anyhow::Result<Vec<String>> {
    let mut hosts = Vec::new();
    let mut remaining = rule;
    while let Some(offset) = remaining.find("Host(") {
        let arguments = &remaining[offset + 5..];
        let end = arguments
            .find(')')
            .ok_or_else(|| anyhow::anyhow!("Traefik Host() 未闭合: {rule}"))?;
        hosts.extend(
            arguments[..end]
                .split(',')
                .map(|value| value.trim().trim_matches('`').to_string())
                .filter(|value| !value.is_empty()),
        );
        remaining = &arguments[end + 1..];
    }
    if hosts.is_empty() {
        anyhow::bail!("Traefik rule 缺少非空 Host(): {rule}");
    }
    Ok(hosts)
}

/// 识别由 nsetup 管理的路由器和服务 label。
fn is_generated_traefik_key(key: &str) -> bool {
    ["traefik.http.routers.", "traefik.http.services."]
        .iter()
        .any(|prefix| {
            key.strip_prefix(prefix)
                .and_then(|rest| rest.split('.').next())
                .is_some_and(|name| name.starts_with("nsetup-"))
        })
}

/// 从生成的 Traefik 规则提取可选路径前缀。
fn parse_path_prefix(rule: &str) -> Option<String> {
    let start = rule.find("PathPrefix(`")? + 12;
    let end = rule[start..].find("`)")? + start;
    Some(rule[start..end].to_string())
}

/// 解析可选布尔 label 值。
fn parse_optional_bool(value: Option<&String>) -> anyhow::Result<Option<bool>> {
    value
        .map(|value| value.parse::<bool>().context("Traefik bool label 无效"))
        .transpose()
}

/// 解析非零 TCP 或 UDP 端口号并检查范围。
fn parse_port(value: &str, label: &str) -> anyhow::Result<u16> {
    let port = value
        .parse::<u16>()
        .with_context(|| format!("{label}无效: {value}"))?;
    if port == 0 {
        anyhow::bail!("{label}不能为 0");
    }
    Ok(port)
}

/// 用于省略 `false` 值的 Serde 辅助函数。
fn is_false(value: &bool) -> bool {
    !value
}

/// 要求环境文件路径保持在项目目录内。
fn validate_relative_compose_path(value: &str) -> anyhow::Result<()> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        anyhow::bail!("env_file 必须是项目内的安全相对路径: {value}");
    }
    Ok(())
}

/// 校验直接交给 Docker、而非使用项目命名规则的名称。
fn validate_docker_object_name(label: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 255
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        anyhow::bail!("{label}无效: {value}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Document, Route, RouteProtocol, Service, StackSpec, parse_env_file, serialize_env_file,
    };
    use std::collections::BTreeMap;

    #[test]
    fn compose_unknown_field_is_rejected() {
        let result = StackSpec::parse(
            "demo",
            "services:\n  app:\n    image: example/app:1\n    depends_on: [db]\n",
            "",
        );
        assert!(result.is_err());
    }

    #[test]
    fn routes_round_trip_through_labels() -> anyhow::Result<()> {
        let mut service = Service {
            image: String::from("example/app:1"),
            ..Service::default()
        };
        let expected = Route {
            hosts: vec![String::from("app.example.com")],
            path_prefix: Some(String::from("/api")),
            container_port: 8080,
            middlewares: vec![String::from("gzip")],
            protocol: RouteProtocol::H2c,
            sticky_cookie: true,
            pass_host_header: Some(false),
            priority: Some(100),
        };
        service.set_routes("demo", "web", std::slice::from_ref(&expected))?;
        assert_eq!(service.routes()?, vec![expected]);
        let spec = StackSpec {
            name: String::from("demo"),
            document: Document {
                services: BTreeMap::from([(String::from("web"), service)]),
                networks: BTreeMap::new(),
            },
            environment: BTreeMap::new(),
        };
        spec.validate()?;
        Ok(())
    }

    /// 镜像钉版本同时拒绝缺少标签和可变的 `latest` 标签。
    #[test]
    fn rejects_unpinned_images() {
        for image in ["example/app", "example/app:latest"] {
            let compose = format!("services:\n  app:\n    image: {image}\n");
            assert!(StackSpec::parse("demo", &compose, "").is_err());
        }
    }

    /// 容器环境文件不能越出受管项目目录。
    #[test]
    fn rejects_escaping_env_file() {
        let compose = "services:\n  app:\n    image: example/app:1\n    env_file: [../secret]\n";
        assert!(StackSpec::parse("demo", compose, "").is_err());
    }

    /// 受管 `.env` 转义应保留空格、引号和换行符。
    #[test]
    fn env_file_round_trip() -> anyhow::Result<()> {
        let parsed = parse_env_file("A=plain\nB=\"two words\"\nC=\"line\\nnext\"\n")?;
        let normalized = serialize_env_file(&parsed);
        assert_eq!(parse_env_file(&normalized)?, parsed);
        Ok(())
    }

    /// 由变量支撑的模板镜像不能按字面镜像标签编辑。
    #[test]
    fn rejects_version_edit_for_variable_image() {
        let mut service = Service {
            image: String::from("nginx:${NGINX_VERSION}"),
            ..Service::default()
        };
        assert!(service.set_image_version(None, Some("1.28")).is_err());
    }
}
