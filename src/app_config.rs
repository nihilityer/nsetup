//! nsetup 原生的单服务应用简化配置。

use crate::constants::APP_CONFIG_FILE;
use crate::generator::{
    self, AppSpec, GeneratedFile, GeneratedStack, HealthcheckSpec, Middleware, NamedVolume,
    NetworkMode, PortProtocol, PublishedPort, Route, Volume,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 简化配置允许的最大字节数。
const MAX_CONFIG_SIZE: usize = 1024 * 1024;

/// 当前简化配置格式版本。
const CONFIG_VERSION: u8 = 1;

/// 已生成的简化应用及其冲突检测输入。
#[derive(Debug)]
pub struct GeneratedApplicationConfig {
    /// 可直接持久化的 Compose 项目。
    pub stack: GeneratedStack,
    /// 应用声明的 HTTP 路由。
    pub routes: Vec<Route>,
    /// 应用发布的宿主机端口。
    pub published_ports: Vec<PublishedPort>,
}

/// nsetup 原生的单服务应用配置。
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ApplicationConfig {
    /// 配置格式版本。
    #[serde(default = "default_format")]
    format: u8,
    /// Compose 项目名。
    name: String,
    /// Compose 服务名。
    #[serde(
        default = "default_service",
        skip_serializing_if = "is_default_service"
    )]
    service: String,
    /// 带有明确版本标签的完整镜像引用。
    image: String,
    /// 覆盖镜像默认命令的参数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,
    /// 应用主要容器端口。
    #[serde(default = "default_port", skip_serializing_if = "is_default_port")]
    port: u16,
    /// 宿主机端口映射。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    publish: Vec<String>,
    /// 绑定卷挂载。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<String>,
    /// Docker 命名卷挂载。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    named_volumes: Vec<String>,
    /// 容器环境变量。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    environment: BTreeMap<String, String>,
    /// 容器网络模式。
    #[serde(default, skip_serializing_if = "Network::is_bridge")]
    network: Network,
    /// external 网络模式使用的网络名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_network: Option<String>,
    /// 常用 Traefik 配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    traefik: Option<TraefikConfig>,
    /// 无法由高层选项表达的附加 Docker 标签。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    labels: Vec<String>,
    /// 容器健康检查。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    healthcheck: Option<HealthcheckConfig>,
}

/// 简化配置中的容器网络模式。
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Network {
    /// Compose 默认桥接网络。
    #[default]
    Bridge,
    /// 宿主机网络。
    Host,
    /// 指定名称的外部网络。
    External,
}

impl Network {
    /// 判断是否为默认桥接网络。
    const fn is_bridge(value: &Self) -> bool {
        matches!(value, Self::Bridge)
    }
}

/// 常用 Traefik 高层配置。
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TraefikConfig {
    /// 使用应用主要端口的域名或短子域名。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hosts: Vec<String>,
    /// 使用独立端口或路径的路由。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    routes: Vec<TraefikRoute>,
    /// 应用于全部路由的路径前缀。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path_prefix: Option<String>,
    /// 引用基础设施内置的文件中间件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    middlewares: Vec<BuiltinMiddleware>,
    /// Traefik 连接后端时使用的协议。
    #[serde(default, skip_serializing_if = "BackendProtocol::is_http")]
    protocol: BackendProtocol,
    /// 是否启用基于 cookie 的粘性会话。
    #[serde(default, skip_serializing_if = "is_false")]
    sticky_cookie: bool,
    /// 是否把原始 Host 请求头传递给后端。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pass_host_header: Option<bool>,
    /// 全部路由使用的显式优先级。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<u32>,
}

/// 简化配置中的单条 Traefik 路由。
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TraefikRoute {
    /// 完整域名或短子域名。
    host: String,
    /// 路由连接的容器端口；省略时使用应用主要端口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    /// 此路由独立使用的路径前缀。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path_prefix: Option<String>,
}

/// 基础设施提供的常用 Traefik 中间件。
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum BuiltinMiddleware {
    /// Gzip 响应压缩。
    Gzip,
    /// 注入 HTTPS 转发头。
    ForwardedHeaders,
    /// 仅允许内网地址访问。
    InternalOnly,
}

/// Traefik 连接应用后端的协议。
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum BackendProtocol {
    /// 普通 HTTP。
    #[default]
    Http,
    /// TLS HTTP。
    Https,
    /// 明文 HTTP/2，常用于 gRPC。
    H2c,
}

impl BackendProtocol {
    /// 判断是否为 Traefik 默认的 HTTP 后端。
    const fn is_http(value: &Self) -> bool {
        matches!(value, Self::Http)
    }

    /// 返回 Traefik label 使用的协议名。
    const fn label(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::H2c => "h2c",
        }
    }
}

/// 简化配置中的健康检查。
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HealthcheckConfig {
    /// `CMD-SHELL` 检查命令。
    command: String,
    /// 检查间隔。
    #[serde(
        default = "default_health_interval",
        skip_serializing_if = "is_default_health_interval"
    )]
    interval: String,
    /// 单次检查超时。
    #[serde(
        default = "default_health_timeout",
        skip_serializing_if = "is_default_health_timeout"
    )]
    timeout: String,
    /// 启动宽限期。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_period: Option<String>,
    /// 失败重试次数。
    #[serde(
        default = "default_health_retries",
        skip_serializing_if = "is_default_health_retries"
    )]
    retries: u32,
}

/// 读取、校验并生成一个原生简化配置应用。
pub fn generate(content: &str, domain: &str) -> anyhow::Result<GeneratedApplicationConfig> {
    if content.len() > MAX_CONFIG_SIZE {
        anyhow::bail!("应用简化配置不能超过 1 MiB");
    }
    let config: ApplicationConfig = toml::from_str(content)
        .map_err(|error| anyhow::anyhow!("应用简化配置 TOML 格式错误: {error}"))?;
    if config.format != CONFIG_VERSION {
        anyhow::bail!(
            "不支持应用简化配置版本 {}，当前仅支持 {}",
            config.format,
            CONFIG_VERSION
        );
    }
    let normalized = toml::to_string_pretty(&config).context("无法序列化应用简化配置")?;
    let spec = config.into_spec(domain)?;
    let routes = spec.routes.clone();
    let published_ports = spec.published_ports.clone();
    let mut generated = generator::generate_application(&spec)?;
    generated.files.push(GeneratedFile {
        path: PathBuf::from(APP_CONFIG_FILE),
        content: normalized.into_bytes(),
        mode: 0o600,
    });
    Ok(GeneratedApplicationConfig {
        stack: generated,
        routes,
        published_ports,
    })
}

impl ApplicationConfig {
    /// 将简化配置转换为应用生成器参数。
    fn into_spec(self, domain: &str) -> anyhow::Result<AppSpec> {
        let (image, version) = split_image(&self.image)?;
        let published_ports = self
            .publish
            .iter()
            .map(|mapping| parse_published_port(mapping))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let volumes = self
            .volumes
            .iter()
            .map(|mapping| parse_volume(mapping))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let named_volumes = self
            .named_volumes
            .iter()
            .map(|mapping| parse_named_volume(mapping))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let network_mode = match (self.network, self.external_network) {
            (Network::Bridge, None) => NetworkMode::Bridge,
            (Network::Host, None) => NetworkMode::Host,
            (Network::External, Some(name)) if !name.is_empty() => NetworkMode::External(name),
            (Network::External, None) => {
                anyhow::bail!("network: external 必须同时指定 external_network")
            }
            (_, Some(_)) => anyhow::bail!("external_network 只能与 network: external 一起使用"),
        };
        let (routes, middlewares, mut generated_labels) = match self.traefik {
            Some(traefik) => traefik.into_parts(&self.name, self.port, domain)?,
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        generated_labels.extend(self.labels);
        let healthcheck = self.healthcheck.map(|healthcheck| HealthcheckSpec {
            command: healthcheck.command,
            interval: healthcheck.interval,
            timeout: healthcheck.timeout,
            start_period: healthcheck.start_period,
            retries: healthcheck.retries,
        });
        Ok(AppSpec {
            name: self.name,
            service: self.service,
            image,
            version,
            command: self.command,
            container_port: self.port,
            routes,
            published_ports,
            volumes,
            environment: self.environment,
            network_mode,
            middlewares,
            labels: generated_labels,
            named_volumes,
            healthcheck,
        })
    }
}

impl TraefikConfig {
    /// 转换路由、中间件和高层选项生成的 labels。
    fn into_parts(
        self,
        project: &str,
        default_port: u16,
        domain: &str,
    ) -> anyhow::Result<(Vec<Route>, Vec<Middleware>, Vec<String>)> {
        let mut routes = self
            .hosts
            .into_iter()
            .map(|host| {
                Ok(Route {
                    host: resolve_host(&host, domain)?,
                    path_prefix: self.path_prefix.clone(),
                    container_port: default_port,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        routes.extend(
            self.routes
                .into_iter()
                .map(|route| {
                    Ok(Route {
                        host: resolve_host(&route.host, domain)?,
                        path_prefix: route.path_prefix.or_else(|| self.path_prefix.clone()),
                        container_port: route.port.unwrap_or(default_port),
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
        );
        if routes.is_empty() {
            anyhow::bail!("traefik 至少需要一个 hosts 或 routes 条目");
        }
        let middlewares = self
            .middlewares
            .into_iter()
            .map(|middleware| match middleware {
                BuiltinMiddleware::Gzip => Middleware::Gzip,
                BuiltinMiddleware::ForwardedHeaders => Middleware::ForwardedHeaders,
                BuiltinMiddleware::InternalOnly => Middleware::InternalOnly,
            })
            .collect();
        let mut labels = Vec::new();
        for index in 0..routes.len() {
            let router = format!("{project}-{index}");
            if self.protocol != BackendProtocol::Http {
                labels.push(format!(
                    "traefik.http.services.{router}.loadbalancer.server.scheme={}",
                    self.protocol.label()
                ));
            }
            if self.sticky_cookie {
                labels.push(format!(
                    "traefik.http.services.{router}.loadbalancer.sticky.cookie=true"
                ));
            }
            if let Some(pass_host_header) = self.pass_host_header {
                labels.push(format!(
                    "traefik.http.services.{router}.loadbalancer.passhostheader={pass_host_header}"
                ));
            }
            if let Some(priority) = self.priority {
                labels.push(format!("traefik.http.routers.{router}.priority={priority}"));
            }
        }
        Ok((routes, middlewares, labels))
    }
}

/// 拆分带有明确标签的镜像引用。
fn split_image(reference: &str) -> anyhow::Result<(String, String)> {
    if reference.contains('@') {
        anyhow::bail!("image 必须使用版本标签，不能使用摘要");
    }
    let slash = reference.rfind('/');
    let colon = reference
        .rfind(':')
        .filter(|colon| slash.is_none_or(|slash| *colon > slash))
        .ok_or_else(|| anyhow::anyhow!("image 必须包含明确版本标签，例如 nginx:1.27"))?;
    let repository = &reference[..colon];
    let version = &reference[colon + 1..];
    if repository.is_empty() || !crate::orchestrator::valid_image_version(version) {
        anyhow::bail!("image 版本标签无效，且不能使用 latest");
    }
    Ok((repository.to_string(), version.to_string()))
}

/// 解析 `HOST:CONTAINER[/PROTOCOL]` 端口映射。
fn parse_published_port(value: &str) -> anyhow::Result<PublishedPort> {
    let (mapping, protocol) = match value.rsplit_once('/') {
        Some((mapping, "tcp")) => (mapping, PortProtocol::Tcp),
        Some((mapping, "udp")) => (mapping, PortProtocol::Udp),
        Some((_, protocol)) => anyhow::bail!("不支持端口协议 {protocol}，只能使用 tcp 或 udp"),
        None => (value, PortProtocol::Tcp),
    };
    let (host, container) = mapping
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("端口映射必须为 HOST:CONTAINER[/PROTOCOL]"))?;
    Ok(PublishedPort {
        host_port: parse_port(host)?,
        container_port: parse_port(container)?,
        protocol,
    })
}

/// 解析 `HOST:CONTAINER[:ro]` 绑定卷。
fn parse_volume(value: &str) -> anyhow::Result<Volume> {
    let (mapping, read_only) = value
        .strip_suffix(":ro")
        .map_or((value, false), |mapping| (mapping, true));
    let (host_path, container_path) = mapping
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("卷挂载必须为 HOST:CONTAINER[:ro]"))?;
    Ok(Volume {
        host_path: host_path.to_string(),
        container_path: container_path.to_string(),
        read_only,
    })
}

/// 解析 `NAME:CONTAINER` 命名卷。
fn parse_named_volume(value: &str) -> anyhow::Result<NamedVolume> {
    let (name, container_path) = value
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("命名卷必须为 NAME:CONTAINER"))?;
    Ok(NamedVolume {
        name: name.to_string(),
        container_path: container_path.to_string(),
    })
}

/// 解析非零端口。
fn parse_port(value: &str) -> anyhow::Result<u16> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| anyhow::anyhow!("端口必须在 1..=65535 范围内"))
}

/// 将短子域名解析为完整域名。
fn resolve_host(host: &str, domain: &str) -> anyhow::Result<String> {
    if host.contains('.') {
        return Ok(host.to_string());
    }
    let valid = host.len() <= 63
        && host
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && host
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && host
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric);
    if !valid {
        anyhow::bail!("短子域名格式无效: {host}");
    }
    Ok(format!("{host}.{domain}"))
}

/// 默认配置格式版本。
const fn default_format() -> u8 {
    CONFIG_VERSION
}

/// 默认 Compose 服务名。
fn default_service() -> String {
    String::from("app")
}

/// 判断服务名是否为默认值。
fn is_default_service(value: &String) -> bool {
    value == "app"
}

/// 默认主要容器端口。
const fn default_port() -> u16 {
    80
}

/// 判断主要容器端口是否为默认值。
const fn is_default_port(value: &u16) -> bool {
    *value == 80
}

/// 默认健康检查间隔。
fn default_health_interval() -> String {
    String::from("30s")
}

/// 判断健康检查间隔是否为默认值。
fn is_default_health_interval(value: &String) -> bool {
    value == "30s"
}

/// 默认健康检查超时。
fn default_health_timeout() -> String {
    String::from("30s")
}

/// 判断健康检查超时是否为默认值。
fn is_default_health_timeout(value: &String) -> bool {
    value == "30s"
}

/// 默认健康检查重试次数。
const fn default_health_retries() -> u32 {
    3
}

/// 判断健康检查重试次数是否为默认值。
const fn is_default_health_retries(value: &u32) -> bool {
    *value == 3
}

/// 供 serde 跳过 false 值。
const fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 简化配置应生成默认 HTTPS 路由及常用 Traefik labels。
    #[test]
    fn generates_application_and_traefik_presets() -> anyhow::Result<()> {
        let generated = generate(
            r#"
format = 1
name = "grpc-api"
image = "ghcr.io/example/api:1.2.3"
port = 8080

[environment]
LOG_LEVEL = "info"

[traefik]
hosts = ["api"]
middlewares = ["gzip", "internal-only"]
protocol = "h2c"
sticky_cookie = true
pass_host_header = false
priority = 100
"#,
            "example.com",
        )?;
        assert!(
            generated
                .stack
                .compose_yaml
                .contains("Host(`api.example.com`)")
        );
        assert!(generated.stack.compose_yaml.contains("server.scheme=h2c"));
        assert!(generated.stack.compose_yaml.contains("sticky.cookie=true"));
        assert!(
            generated
                .stack
                .compose_yaml
                .contains("passhostheader=false")
        );
        assert!(generated.stack.compose_yaml.contains("priority=100"));
        assert!(
            generated
                .stack
                .compose_yaml
                .contains("gzip@file,internal-only@file")
        );
        assert_eq!(
            generated.stack.files[0].path,
            PathBuf::from(APP_CONFIG_FILE)
        );
        assert_eq!(generated.stack.files[0].mode, 0o600);
        let exported = std::str::from_utf8(&generated.stack.files[0].content)?;
        assert!(exported.contains("format = 1"));
        assert!(exported.contains("[traefik]"));
        Ok(())
    }

    /// 简化配置必须拒绝未固定版本的镜像。
    #[test]
    fn rejects_unpinned_image() -> anyhow::Result<()> {
        let Err(error) = generate("name = \"demo\"\nimage = \"nginx:latest\"\n", "example.com")
        else {
            anyhow::bail!("latest 镜像未被拒绝");
        };
        let error = error.to_string();
        assert!(error.contains("latest"));
        Ok(())
    }

    /// 简化配置必须拒绝未知字段，避免拼写错误静默失效。
    #[test]
    fn rejects_unknown_fields() -> anyhow::Result<()> {
        let Err(error) = generate(
            "name = \"demo\"\nimage = \"nginx:1.27\"\nunknown = true\n",
            "example.com",
        ) else {
            anyhow::bail!("未知配置字段未被拒绝");
        };
        let error = error.to_string();
        assert!(error.contains("unknown"));
        Ok(())
    }
}
