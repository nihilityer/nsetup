//! 版本化 TOML 模板及其与 IR 的双向转换。

use crate::config::Config;
use crate::constants::PROXY_NETWORK;
use crate::spec::{
    Document, Healthcheck, Logging, Network, PortProtocol, PublishedPort, Route, RouteProtocol,
    Service, StackSpec, validate_name, validate_version,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 当前面向用户的配置格式版本。
pub const FORMAT_VERSION: u32 = 1;

/// 与 Compose 状态一同写入的项目附属文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    /// 相对于项目目录的安全路径。
    pub path: PathBuf,
    /// 文件原始字节。
    pub content: Vec<u8>,
    /// Unix 权限模式。
    pub mode: u32,
    /// 是否替换已复制的现有附属文件。
    pub replace: bool,
}

/// 模板转换输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateOutput {
    /// 统一项目 IR。
    pub spec: StackSpec,
    /// 由模板拥有且不属于 IR 的文件。
    pub files: Vec<GeneratedFile>,
    /// 选定的模板类型。
    pub kind: TemplateKind,
}

/// 已注册的模板类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    /// 通用容器应用。
    App,
    /// Traefik 反向代理。
    Traefik,
    /// Nginx 静态站点。
    Static,
}

impl TemplateKind {
    /// 解析 CLI 或 TOML 模板名称。
    ///
    /// # 错误
    ///
    /// 模板未注册时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "app" => Ok(Self::App),
            "traefik" => Ok(Self::Traefik),
            "static" => Ok(Self::Static),
            _ => anyhow::bail!("未知模板 {value}；可用值: app, traefik, static"),
        }
    }

    /// 返回稳定的 TOML 名称。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Traefik => "traefik",
            Self::Static => "static",
        }
    }
}

/// 通用应用 TOML 文档。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// 配置模式版本。
    pub format: u32,
    /// 可选的明确模板选择器。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// 项目名。
    pub name: String,
    /// 以 Compose 服务名为键的服务映射。
    pub services: BTreeMap<String, AppServiceConfig>,
}

/// 应用模板中的单个服务。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppServiceConfig {
    /// 不含标签的镜像仓库。
    pub image: String,
    /// 明确的镜像版本。
    pub version: String,
    /// 可选容器名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    /// 路由使用的默认容器端口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// 发布端口映射。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish: Vec<String>,
    /// bind mount 列表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    /// 容器环境变量。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// 命令覆盖参数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// 重启策略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
    /// 容器环境文件路径。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_file: Vec<String>,
    /// 逻辑网络模式。
    #[serde(default)]
    pub network: AppNetwork,
    /// 外部模式使用的 Docker 网络。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_network: Option<String>,
    /// 自定义 Docker label。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// 可选健康检查。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<HealthcheckConfig>,
    /// 可选日志驱动。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<Logging>,
    /// 可选 Traefik 路由。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traefik: Option<TraefikRoutesConfig>,
}

/// 应用逻辑网络选项。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AppNetwork {
    /// Compose bridge 网络。
    #[default]
    Bridge,
    /// 宿主机网络。
    Host,
    /// 用户选择的外部 Docker 网络。
    External,
}

/// TOML 健康检查字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthcheckConfig {
    /// shell 命令。
    pub command: String,
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

/// 服务的紧凑 Traefik 配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraefikRoutesConfig {
    /// 紧凑单路由形式使用的主机名。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// 紧凑形式使用的可选路径前缀。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// 共享中间件名称。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub middlewares: Vec<String>,
    /// 后端协议。
    #[serde(default)]
    pub protocol: TemplateRouteProtocol,
    /// 粘性 Cookie 开关。
    #[serde(default)]
    pub sticky_cookie: bool,
    /// 可选的 `passHostHeader` 覆盖值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_host_header: Option<bool>,
    /// 可选路由优先级。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    /// 展开的路由条目。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<TraefikRouteConfig>,
}

/// 单个展开的应用路由。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraefikRouteConfig {
    /// 路由主机名。
    pub hosts: Vec<String>,
    /// 可选路径前缀。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// 可选的路由专用容器端口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// 路由专用中间件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub middlewares: Vec<String>,
    /// 后端协议。
    #[serde(default)]
    pub protocol: TemplateRouteProtocol,
    /// 粘性 Cookie 开关。
    #[serde(default)]
    pub sticky_cookie: bool,
    /// 可选的 `passHostHeader` 覆盖值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_host_header: Option<bool>,
    /// 可选路由优先级。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
}

/// 路由协议的 TOML 表示。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TemplateRouteProtocol {
    /// 普通 HTTP。
    #[default]
    Http,
    /// 连接后端的 HTTPS。
    Https,
    /// 明文 HTTP/2。
    H2c,
}

/// Traefik 基础设施 TOML 文档。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraefikConfig {
    /// 配置模式版本。
    pub format: u32,
    /// 必填模板选择器。
    pub template: String,
    /// 基础 DNS 域名。
    pub domain: String,
    /// ACME 联系邮箱。
    pub acme_email: String,
    /// 持久化到 `.env` 的 Cloudflare API 令牌。
    pub cloudflare_token: String,
    /// 明确的 Traefik 镜像版本。
    pub version: String,
    /// 宿主机 HTTP 端口。
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    /// 宿主机 HTTPS 和 HTTP/3 端口。
    #[serde(default = "default_https_port")]
    pub https_port: u16,
}

/// 静态站点 TOML 文档。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaticConfig {
    /// 配置模式版本。
    pub format: u32,
    /// 必填模板选择器。
    pub template: String,
    /// 项目名。
    pub name: String,
    /// 短形式或完整形式的路由主机名。
    pub host: String,
    /// 明确的 Nginx 镜像版本。
    pub version: String,
    /// 常用 Traefik 中间件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub middlewares: Vec<String>,
}

/// 解析声明并生成统一 IR。
///
/// # 错误
///
/// 当 TOML 格式错误、模板字段无效或输出不安全时返回错误。
pub fn apply(input: &str, config: &Config) -> anyhow::Result<TemplateOutput> {
    let value: toml::Value = toml::from_str(input).context("TOML 配置格式错误")?;
    let format = value
        .get("format")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| anyhow::anyhow!("TOML 配置缺少整数 format"))?;
    if format != i64::from(FORMAT_VERSION) {
        anyhow::bail!("不支持的配置 format: {format}");
    }
    let kind = TemplateKind::parse(
        value
            .get("template")
            .and_then(toml::Value::as_str)
            .unwrap_or("app"),
    )?;
    match kind {
        TemplateKind::App => generate_app(toml::from_str(input)?, config),
        TemplateKind::Traefik => generate_traefik(toml::from_str(input)?, config),
        TemplateKind::Static => generate_static(toml::from_str(input)?, config),
    }
}

/// 从当前由 Compose 承载的 IR 重建规范化 TOML。
///
/// # 错误
///
/// 无法恢复模板元数据或语义字段时返回错误。
pub fn export(spec: &StackSpec, config: &Config) -> anyhow::Result<String> {
    let kind = detect_kind(spec)?;
    let mut output = match kind {
        TemplateKind::App => toml::to_string_pretty(&export_app(spec)?)?,
        TemplateKind::Traefik => toml::to_string_pretty(&export_traefik(spec, config)?)?,
        TemplateKind::Static => toml::to_string_pretty(&export_static(spec)?)?,
    };
    if !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

/// 返回已注册模板的带注释配置骨架。
#[must_use]
pub const fn skeleton(kind: TemplateKind) -> &'static str {
    match kind {
        TemplateKind::App => APP_SKELETON,
        TemplateKind::Traefik => TRAEFIK_SKELETON,
        TemplateKind::Static => STATIC_SKELETON,
    }
}

/// 将应用文档转换为 Compose IR。
fn generate_app(input: AppConfig, config: &Config) -> anyhow::Result<TemplateOutput> {
    ensure_format(input.format)?;
    if input
        .template
        .as_deref()
        .is_some_and(|value| value != "app")
    {
        anyhow::bail!("app 配置的 template 必须为 app");
    }
    validate_name("项目名", &input.name)?;
    if input.services.is_empty() {
        anyhow::bail!("app 模板至少需要一个服务");
    }
    let mut document = Document::default();
    for (service_name, service_config) in input.services {
        validate_name("服务名", &service_name)?;
        validate_version(&service_config.version)?;
        validate_repository(&service_config.image)?;
        let routes = app_routes(&service_config, config)?;
        if !routes.is_empty() && service_config.network == AppNetwork::Host {
            anyhow::bail!("host 网络模式不能使用 Traefik 容器路由");
        }
        let mut service = Service {
            image: format!("{}:{}", service_config.image, service_config.version),
            container_name: service_config.container_name,
            command: service_config.command,
            restart: service_config.restart,
            ports: service_config.publish,
            volumes: service_config.volumes,
            environment: service_config.environment,
            env_file: service_config.env_file,
            labels: service_config.labels,
            healthcheck: service_config.healthcheck.map(healthcheck_from_config),
            logging: service_config.logging,
            ..Service::default()
        };
        service.labels.push(String::from("io.nsetup.template=app"));
        match service_config.network {
            AppNetwork::Bridge if !routes.is_empty() => {
                add_proxy_network(&mut document, true);
                service.networks.push(String::from("proxy"));
            }
            AppNetwork::Bridge => {}
            AppNetwork::Host => service.network_mode = Some(String::from("host")),
            AppNetwork::External => {
                let external = service_config.external_network.ok_or_else(|| {
                    anyhow::anyhow!("服务 {service_name} 的 external 网络缺少 external_network")
                })?;
                validate_docker_name("外部网络名", &external)?;
                let key = format!("external-{service_name}");
                document.networks.insert(
                    key.clone(),
                    Network {
                        external: true,
                        name: Some(external),
                    },
                );
                service.networks.push(key);
                if !routes.is_empty() {
                    add_proxy_network(&mut document, true);
                    service.networks.push(String::from("proxy"));
                }
            }
        }
        service.set_routes(&input.name, &service_name, &routes)?;
        document.services.insert(service_name, service);
    }
    let spec = StackSpec {
        name: input.name,
        document,
        environment: BTreeMap::new(),
    };
    spec.validate()?;
    Ok(TemplateOutput {
        spec,
        files: Vec::new(),
        kind: TemplateKind::App,
    })
}

/// 将 Traefik 文档转换为 IR 及其所属配置文件。
fn generate_traefik(input: TraefikConfig, config: &Config) -> anyhow::Result<TemplateOutput> {
    ensure_format(input.format)?;
    ensure_template(&input.template, TemplateKind::Traefik)?;
    crate::config::validate_domain(&input.domain)?;
    validate_version(&input.version)?;
    validate_email(&input.acme_email)?;
    if input.cloudflare_token.trim().is_empty() {
        anyhow::bail!("cloudflare_token 不能为空");
    }
    let name = String::from("traefik");
    let directory = config.stacks_root.join(&name);
    let mut document = Document::default();
    add_proxy_network(&mut document, false);
    let mut service = Service {
        image: String::from("traefik:${TRAEFIK_VERSION}"),
        container_name: Some(String::from("traefik")),
        command: vec![String::from("--configFile=/etc/traefik/traefik.yml")],
        restart: Some(String::from("unless-stopped")),
        networks: vec![String::from("proxy")],
        ports: vec![
            format!("{}:80/tcp", input.http_port),
            format!("{}:443/tcp", input.https_port),
            format!("{}:443/udp", input.https_port),
        ],
        volumes: vec![
            format!("{}:/var/run/docker.sock:ro", config.docker_socket.display()),
            format!(
                "{}/config/traefik.yml:/etc/traefik/traefik.yml:ro",
                directory.display()
            ),
            format!(
                "{}/config/dynamic.yml:/etc/traefik/dynamic.yml:ro",
                directory.display()
            ),
            format!("{}/config/acme.json:/acme.json", directory.display()),
        ],
        environment: BTreeMap::from([
            (
                String::from("CF_DNS_API_TOKEN"),
                String::from("${CF_DNS_API_TOKEN}"),
            ),
            (String::from("ACME_EMAIL"), String::from("${ACME_EMAIL}")),
        ]),
        labels: vec![String::from("io.nsetup.template=traefik")],
        ..Service::default()
    };
    service.set_routes(
        &name,
        "traefik",
        &[Route {
            hosts: vec![format!("traefik.{}", input.domain)],
            path_prefix: None,
            container_port: 8080,
            middlewares: vec![String::from("internal-only")],
            protocol: RouteProtocol::Http,
            sticky_cookie: false,
            pass_host_header: None,
            priority: None,
        }],
    )?;
    document.services.insert(String::from("traefik"), service);
    let spec = StackSpec {
        name,
        document,
        environment: BTreeMap::from([
            (String::from("TRAEFIK_VERSION"), input.version),
            (String::from("ACME_EMAIL"), input.acme_email.clone()),
            (String::from("CF_DNS_API_TOKEN"), input.cloudflare_token),
        ]),
    };
    spec.validate()?;
    let files = vec![
        GeneratedFile {
            path: PathBuf::from("config/traefik.yml"),
            content: traefik_static_config(&input.acme_email).into_bytes(),
            mode: 0o640,
            replace: true,
        },
        GeneratedFile {
            path: PathBuf::from("config/dynamic.yml"),
            content: traefik_dynamic_config(&input.domain).into_bytes(),
            mode: 0o640,
            replace: true,
        },
        GeneratedFile {
            path: PathBuf::from("config/acme.json"),
            content: b"{}\n".to_vec(),
            mode: 0o600,
            replace: false,
        },
    ];
    Ok(TemplateOutput {
        spec,
        files,
        kind: TemplateKind::Traefik,
    })
}

/// 将静态站点文档转换为 Compose IR。
fn generate_static(input: StaticConfig, config: &Config) -> anyhow::Result<TemplateOutput> {
    ensure_format(input.format)?;
    ensure_template(&input.template, TemplateKind::Static)?;
    validate_name("项目名", &input.name)?;
    validate_version(&input.version)?;
    validate_middlewares(&input.middlewares)?;
    let host = expand_host(&input.host, &config.domain)?;
    let directory = config.stacks_root.join(&input.name);
    let mut document = Document::default();
    add_proxy_network(&mut document, true);
    let mut service = Service {
        image: String::from("nginx:${NGINX_VERSION}"),
        restart: Some(String::from("unless-stopped")),
        networks: vec![String::from("proxy")],
        volumes: vec![format!(
            "{}/site:/usr/share/nginx/html:ro",
            directory.display()
        )],
        labels: vec![String::from("io.nsetup.template=static")],
        ..Service::default()
    };
    service.set_routes(
        &input.name,
        "web",
        &[Route {
            hosts: vec![host],
            path_prefix: None,
            container_port: 80,
            middlewares: input.middlewares,
            protocol: RouteProtocol::Http,
            sticky_cookie: false,
            pass_host_header: None,
            priority: None,
        }],
    )?;
    document.services.insert(String::from("web"), service);
    let spec = StackSpec {
        name: input.name,
        document,
        environment: BTreeMap::from([(String::from("NGINX_VERSION"), input.version)]),
    };
    spec.validate()?;
    Ok(TemplateOutput {
        spec,
        files: Vec::new(),
        kind: TemplateKind::Static,
    })
}

/// 从当前 IR 字段重建应用模板。
fn export_app(spec: &StackSpec) -> anyhow::Result<AppConfig> {
    let mut services = BTreeMap::new();
    for (name, source) in &spec.document.services {
        let (image, version) = source.image_version()?;
        let routes = source.routes()?;
        let port = routes
            .first()
            .map(|route| route.container_port)
            .or_else(|| {
                source
                    .ports
                    .first()
                    .and_then(|value| PublishedPort::parse(value).ok())
                    .map(|value| value.container_port)
            });
        let traefik = routes_to_config(&routes);
        let mut custom = source.clone();
        custom.set_routes(&spec.name, name, &[])?;
        custom
            .labels
            .retain(|label| label != "io.nsetup.template=app");
        let (network, external_network) = export_network(source, &spec.document);
        services.insert(
            name.clone(),
            AppServiceConfig {
                image,
                version,
                container_name: source.container_name.clone(),
                port,
                publish: source.ports.clone(),
                volumes: source.volumes.clone(),
                environment: source.environment.clone(),
                command: source.command.clone(),
                restart: source.restart.clone(),
                env_file: source.env_file.clone(),
                network,
                external_network,
                labels: custom.labels,
                healthcheck: source
                    .healthcheck
                    .as_ref()
                    .map(healthcheck_to_config)
                    .transpose()?,
                logging: source.logging.clone(),
                traefik,
            },
        );
    }
    Ok(AppConfig {
        format: FORMAT_VERSION,
        template: None,
        name: spec.name.clone(),
        services,
    })
}

/// 从当前 IR 与 `.env` 字段重建 Traefik 模板。
fn export_traefik(spec: &StackSpec, config: &Config) -> anyhow::Result<TraefikConfig> {
    let service = only_named_service(spec, "traefik")?;
    let routes = service.routes()?;
    let dashboard_host = routes
        .first()
        .and_then(|route| route.hosts.first())
        .cloned()
        .unwrap_or_else(|| format!("traefik.{}", config.domain));
    let domain = dashboard_host
        .strip_prefix("traefik.")
        .unwrap_or(&dashboard_host)
        .to_string();
    let ports: Vec<PublishedPort> = service
        .ports
        .iter()
        .map(|value| PublishedPort::parse(value))
        .collect::<anyhow::Result<_>>()?;
    let http_port = ports
        .iter()
        .find(|port| port.container_port == 80 && port.protocol == PortProtocol::Tcp)
        .map_or(80, |port| port.host_port);
    let https_port = ports
        .iter()
        .find(|port| port.container_port == 443 && port.protocol == PortProtocol::Tcp)
        .map_or(443, |port| port.host_port);
    Ok(TraefikConfig {
        format: FORMAT_VERSION,
        template: String::from("traefik"),
        domain,
        acme_email: required_env(spec, "ACME_EMAIL")?,
        cloudflare_token: required_env(spec, "CF_DNS_API_TOKEN")?,
        version: required_env(spec, "TRAEFIK_VERSION")?,
        http_port,
        https_port,
    })
}

/// 从当前 IR 字段重建静态站点模板。
fn export_static(spec: &StackSpec) -> anyhow::Result<StaticConfig> {
    let service = only_named_service(spec, "web")?;
    let route = service
        .routes()?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("static 项目缺少 Traefik 路由"))?;
    let host = route
        .hosts
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("static 项目路由缺少 host"))?;
    Ok(StaticConfig {
        format: FORMAT_VERSION,
        template: String::from("static"),
        name: spec.name.clone(),
        host,
        version: required_env(spec, "NGINX_VERSION")?,
        middlewares: route.middlewares,
    })
}

/// 将紧凑或详细的 TOML 路由展开为语义路由。
fn app_routes(service: &AppServiceConfig, config: &Config) -> anyhow::Result<Vec<Route>> {
    let Some(traefik) = &service.traefik else {
        return Ok(Vec::new());
    };
    validate_middlewares(&traefik.middlewares)?;
    let mut output = Vec::new();
    if !traefik.hosts.is_empty() {
        let port = service
            .port
            .ok_or_else(|| anyhow::anyhow!("traefik.hosts 需要服务 port"))?;
        output.push(Route {
            hosts: expand_hosts(&traefik.hosts, &config.domain)?,
            path_prefix: traefik.path_prefix.clone(),
            container_port: port,
            middlewares: traefik.middlewares.clone(),
            protocol: traefik.protocol.into(),
            sticky_cookie: traefik.sticky_cookie,
            pass_host_header: traefik.pass_host_header,
            priority: traefik.priority,
        });
    }
    for route in &traefik.routes {
        validate_middlewares(&route.middlewares)?;
        let port = route
            .port
            .or(service.port)
            .ok_or_else(|| anyhow::anyhow!("Traefik route 需要 route.port 或服务 port"))?;
        output.push(Route {
            hosts: expand_hosts(&route.hosts, &config.domain)?,
            path_prefix: route.path_prefix.clone(),
            container_port: port,
            middlewares: if route.middlewares.is_empty() {
                traefik.middlewares.clone()
            } else {
                route.middlewares.clone()
            },
            protocol: route.protocol.into(),
            sticky_cookie: route.sticky_cookie,
            pass_host_header: route.pass_host_header,
            priority: route.priority,
        });
    }
    if output.is_empty() {
        anyhow::bail!("[services.*.traefik] 至少需要 hosts 或 routes");
    }
    Ok(output)
}

/// 将单条语义路由压缩为紧凑形式，或导出多条详细路由。
fn routes_to_config(routes: &[Route]) -> Option<TraefikRoutesConfig> {
    if routes.is_empty() {
        return None;
    }
    if routes.len() == 1 {
        let route = &routes[0];
        return Some(TraefikRoutesConfig {
            hosts: route.hosts.clone(),
            path_prefix: route.path_prefix.clone(),
            middlewares: route.middlewares.clone(),
            protocol: route.protocol.into(),
            sticky_cookie: route.sticky_cookie,
            pass_host_header: route.pass_host_header,
            priority: route.priority,
            routes: Vec::new(),
        });
    }
    Some(TraefikRoutesConfig {
        routes: routes
            .iter()
            .map(|route| TraefikRouteConfig {
                hosts: route.hosts.clone(),
                path_prefix: route.path_prefix.clone(),
                port: Some(route.container_port),
                middlewares: route.middlewares.clone(),
                protocol: route.protocol.into(),
                sticky_cookie: route.sticky_cookie,
                pass_host_header: route.pass_host_header,
                priority: route.priority,
            })
            .collect(),
        ..TraefikRoutesConfig::default()
    })
}

impl From<TemplateRouteProtocol> for RouteProtocol {
    fn from(value: TemplateRouteProtocol) -> Self {
        match value {
            TemplateRouteProtocol::Http => Self::Http,
            TemplateRouteProtocol::Https => Self::Https,
            TemplateRouteProtocol::H2c => Self::H2c,
        }
    }
}

impl From<RouteProtocol> for TemplateRouteProtocol {
    fn from(value: RouteProtocol) -> Self {
        match value {
            RouteProtocol::Http => Self::Http,
            RouteProtocol::Https => Self::Https,
            RouteProtocol::H2c => Self::H2c,
        }
    }
}

/// 将 TOML 健康检查转换为 Compose `CMD-SHELL` 形式。
fn healthcheck_from_config(value: HealthcheckConfig) -> Healthcheck {
    let mut healthcheck = Healthcheck::command(value.command);
    healthcheck.interval = value.interval;
    healthcheck.timeout = value.timeout;
    healthcheck.start_period = value.start_period;
    healthcheck.retries = value.retries;
    healthcheck
}

/// 将受支持的 Compose 健康检查转换回 TOML。
fn healthcheck_to_config(value: &Healthcheck) -> anyhow::Result<HealthcheckConfig> {
    Ok(HealthcheckConfig {
        command: value.shell_command()?.to_string(),
        interval: value.interval.clone(),
        timeout: value.timeout.clone(),
        start_period: value.start_period.clone(),
        retries: value.retries,
    })
}

/// 从服务元数据标签推导唯一的项目模板类型。
fn detect_kind(spec: &StackSpec) -> anyhow::Result<TemplateKind> {
    let mut kind = None;
    for service in spec.document.services.values() {
        for label in &service.labels {
            if let Some(value) = label.strip_prefix("io.nsetup.template=") {
                let current = TemplateKind::parse(value)?;
                if kind.is_some_and(|previous| previous != current) {
                    anyhow::bail!("项目包含冲突的模板标记");
                }
                kind = Some(current);
            }
        }
    }
    kind.ok_or_else(|| {
        anyhow::anyhow!("导入的 Compose 未包含 io.nsetup.template 标签，无法导出 TOML 模板")
    })
}

/// 从 Compose 网络字段推导高层网络模式。
fn export_network(service: &Service, document: &Document) -> (AppNetwork, Option<String>) {
    if service.network_mode.as_deref() == Some("host") {
        return (AppNetwork::Host, None);
    }
    for key in &service.networks {
        if key == "proxy" {
            continue;
        }
        if let Some(network) = document.networks.get(key)
            && network.external
        {
            return (
                AppNetwork::External,
                Some(network.name.clone().unwrap_or_else(|| key.clone())),
            );
        }
    }
    (AppNetwork::Bridge, None)
}

/// 插入固定名称的反向代理网络定义。
fn add_proxy_network(document: &mut Document, external: bool) {
    document.networks.insert(
        String::from("proxy"),
        Network {
            external,
            name: Some(String::from(PROXY_NETWORK)),
        },
    );
}

/// 按稳定名称获取模板的必需服务。
fn only_named_service<'a>(spec: &'a StackSpec, name: &str) -> anyhow::Result<&'a Service> {
    spec.document
        .services
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("{} 模板缺少服务 {name}", spec.name))
}

/// 读取必需的项目环境变量值。
fn required_env(spec: &StackSpec, key: &str) -> anyhow::Result<String> {
    spec.environment
        .get(key)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("项目 .env 缺少 {key}"))
}

/// 要求使用当前 TOML 模式版本。
fn ensure_format(format: u32) -> anyhow::Result<()> {
    if format != FORMAT_VERSION {
        anyhow::bail!("不支持的配置 format: {format}");
    }
    Ok(())
}

/// 要求明确的模板选择器与解析后的文档匹配。
fn ensure_template(value: &str, expected: TemplateKind) -> anyhow::Result<()> {
    if TemplateKind::parse(value)? != expected {
        anyhow::bail!("模板字段必须为 {}", expected.as_str());
    }
    Ok(())
}

/// 校验不得包含标签或摘要的镜像仓库名。
fn validate_repository(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.contains('@')
        || value.chars().any(char::is_whitespace)
        || value
            .rsplit('/')
            .next()
            .is_some_and(|component| component.contains(':'))
    {
        anyhow::bail!("image 必须是不含标签或摘要的镜像仓库: {value}");
    }
    Ok(())
}

/// 按保守规则校验 Docker 对象名。
fn validate_docker_name(label: &str, value: &str) -> anyhow::Result<()> {
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

/// 根据模板内置中间件注册表检查名称。
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

/// 展开并校验主机名列表。
fn expand_hosts(values: &[String], domain: &str) -> anyhow::Result<Vec<String>> {
    values
        .iter()
        .map(|value| expand_host(value, domain))
        .collect()
}

/// 将配置的域名追加到短路由主机名。
fn expand_host(value: &str, domain: &str) -> anyhow::Result<String> {
    let output = if value.contains('.') {
        value.to_string()
    } else {
        format!("{value}.{domain}")
    };
    crate::config::validate_domain(&output)?;
    Ok(output)
}

/// 对 ACME 邮箱地址执行保守的结构校验。
fn validate_email(value: &str) -> anyhow::Result<()> {
    if value.len() > 254 || value.chars().any(char::is_whitespace) {
        anyhow::bail!("ACME 邮箱无效: {value}");
    }
    let (local, domain) = value
        .rsplit_once('@')
        .ok_or_else(|| anyhow::anyhow!("ACME 邮箱无效: {value}"))?;
    if local.is_empty()
        || !local
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        anyhow::bail!("ACME 邮箱无效: {value}");
    }
    crate::config::validate_domain(domain)
}

/// 返回 Traefik 默认 HTTP 端口。
const fn default_http_port() -> u16 {
    80
}

/// 返回 Traefik 默认 HTTPS 端口。
const fn default_https_port() -> u16 {
    443
}

/// 返回 Traefik 守护进程静态配置。
fn traefik_static_config(acme_email: &str) -> String {
    r#"api:
  dashboard: true
entryPoints:
  web:
    address: ":80"
    http:
      redirections:
        entryPoint:
          to: websecure
          scheme: https
  websecure:
    address: ":443"
    http3: {}
providers:
  docker:
    exposedByDefault: false
    network: nsetup-proxy
  file:
    filename: /etc/traefik/dynamic.yml
certificatesResolvers:
  cloudflare:
    acme:
      email: "__ACME_EMAIL__"
      storage: /acme.json
      dnsChallenge:
        provider: cloudflare
"#
    .replace("__ACME_EMAIL__", acme_email)
}

/// 为域名渲染共享中间件与证书默认配置。
fn traefik_dynamic_config(domain: &str) -> String {
    r#"http:
  middlewares:
    gzip:
      compress: {}
    forwarded-headers:
      headers:
        customRequestHeaders:
          X-Forwarded-Proto: https
    internal-only:
      ipAllowList:
        sourceRange:
          - 10.0.0.0/8
          - 172.16.0.0/12
          - 192.168.0.0/16
          - fc00::/7
    tls:
      headers:
        stsSeconds: 31536000
        stsIncludeSubdomains: true
tls:
  options:
    default:
      minVersion: VersionTLS12
  stores:
    default:
      defaultGeneratedCert:
        resolver: cloudflare
        domain:
          main: "__DOMAIN__"
          sans:
            - "*.__DOMAIN__"
"#
    .replace("__DOMAIN__", domain)
}

/// CLI 输出的带注释应用模板。
const APP_SKELETON: &str = r#"# 容器应用模板（可省略 template = "app"）
format = 1
name = "media"

[services.web]
image = "ghcr.io/example/media"
version = "1.0"
port = 8080
publish = ["12780:8080/tcp"]
volumes = ["/var/lib/nsetup/data/media:/data"]
environment = { LOG_LEVEL = "info" }
network = "bridge"

[services.web.traefik]
hosts = ["media"]
middlewares = ["gzip", "internal-only"]
protocol = "http"
"#;

/// CLI 输出的带注释 Traefik 模板。
const TRAEFIK_SKELETON: &str = r#"# 反向代理基础设施模板
format = 1
template = "traefik"
domain = "example.com"
acme_email = "admin@example.com"
cloudflare_token = "replace-me"
version = "v3.8.0"
http_port = 80
https_port = 443
"#;

/// CLI 输出的带注释静态站点模板。
const STATIC_SKELETON: &str = r#"# 静态 Nginx 站点；使用 --assets 上传文件
format = 1
template = "static"
name = "docs"
host = "docs"
version = "1.27"
middlewares = ["gzip"]
"#;

#[cfg(test)]
mod tests {
    use super::{TemplateKind, apply, export};
    use crate::config::Config;

    #[test]
    fn app_template_round_trip() -> anyhow::Result<()> {
        let input = r#"
format = 1
name = "demo"
[services.web]
image = "example/web"
version = "1.2"
port = 8080
[services.web.traefik]
hosts = ["demo"]
middlewares = ["gzip"]
"#;
        let config = Config::default();
        let generated = apply(input, &config)?;
        assert_eq!(generated.kind, TemplateKind::App);
        let exported = export(&generated.spec, &config)?;
        let regenerated = apply(&exported, &config)?;
        assert_eq!(generated.spec, regenerated.spec);
        Ok(())
    }

    /// Traefik 状态、密钥与模板类型在 IR 导出后保持不变。
    #[test]
    fn traefik_template_round_trip() -> anyhow::Result<()> {
        let input = r#"
format = 1
template = "traefik"
domain = "example.com"
acme_email = "admin@example.com"
cloudflare_token = "secret"
version = "v3.8.0"
http_port = 8080
https_port = 8443
"#;
        let config = Config::default();
        let generated = apply(input, &config)?;
        assert_eq!(generated.kind, TemplateKind::Traefik);
        let acme = generated
            .files
            .iter()
            .find(|file| file.path.ends_with("acme.json"))
            .ok_or_else(|| anyhow::anyhow!("missing acme.json"))?;
        assert!(!acme.replace);
        let exported = export(&generated.spec, &config)?;
        let regenerated = apply(&exported, &config)?;
        assert_eq!(generated.spec, regenerated.spec);
        Ok(())
    }

    /// 静态模板保留镜像版本、主机名与中间件语义。
    #[test]
    fn static_template_round_trip() -> anyhow::Result<()> {
        let input = r#"
format = 1
template = "static"
name = "docs"
host = "docs"
version = "1.27"
middlewares = ["gzip"]
"#;
        let config = Config::default();
        let generated = apply(input, &config)?;
        let exported = export(&generated.spec, &config)?;
        let regenerated = apply(&exported, &config)?;
        assert_eq!(generated.spec, regenerated.spec);
        Ok(())
    }
}
