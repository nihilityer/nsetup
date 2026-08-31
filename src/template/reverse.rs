//! 从当前 IR 状态反解规范化 TOML 声明。

use super::{
    AppConfig, AppNetwork, AppServiceConfig, FORMAT_VERSION, HealthcheckConfig, StaticConfig,
    TemplateKind, TemplateRouteProtocol, TraefikConfig, TraefikRouteConfig, TraefikRoutesConfig,
};
use crate::config::Config;
use crate::spec::{
    Document, Healthcheck, PortProtocol, PublishedPort, Route, RouteProtocol, Service, StackSpec,
};
use std::collections::BTreeMap;

/// 从当前 IR 字段重建应用模板。
pub(super) fn export_app(spec: &StackSpec) -> anyhow::Result<AppConfig> {
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
pub(super) fn export_traefik(spec: &StackSpec, config: &Config) -> anyhow::Result<TraefikConfig> {
    let service = only_named_service(spec, "traefik")?;
    let routes = service.routes()?;
    let dashboard_host = routes
        .first()
        .and_then(|route| route.hosts.first())
        .cloned()
        .unwrap_or_else(|| format!("traefik.{}", config.domain));
    let domain = spec
        .environment
        .get(super::traefik::DOMAIN_KEY)
        .cloned()
        .unwrap_or_else(|| {
            dashboard_host
                .strip_prefix("traefik.")
                .unwrap_or(&dashboard_host)
                .to_string()
        });
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
pub(super) fn export_static(spec: &StackSpec) -> anyhow::Result<StaticConfig> {
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
pub(super) fn detect_kind(spec: &StackSpec) -> anyhow::Result<TemplateKind> {
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
