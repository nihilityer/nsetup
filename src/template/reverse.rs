//! 从当前 IR 状态反解规范化 TOML 声明。

use super::{
    AppAutheliaConfig, AppConfig, AppNetwork, AppServiceConfig, FORMAT_VERSION, HealthcheckCommand,
    HealthcheckConfig, StaticConfig, TemplateKind, TemplateRouteProtocol, TraefikConfig,
    TraefikMiddlewareConfig, TraefikRouteConfig, TraefikRoutesConfig,
};
use crate::config::Config;
use crate::spec::{
    Document, Healthcheck, PortProtocol, PublishedPort, Route, RouteProtocol, Service, StackSpec,
};
use std::collections::BTreeMap;

/// 从当前 IR 字段重建应用模板。
///
/// # 错误
///
/// 镜像、路由、健康检查或项目环境字段无法反解时返回错误。
pub(super) fn export_app(spec: &StackSpec) -> anyhow::Result<AppConfig> {
    let oidc_clients = super::app_oidc_clients(spec)?;
    let authelia = (!oidc_clients.is_empty()).then_some(AppAutheliaConfig { oidc_clients });
    let hooks = spec.project_hooks()?;
    let mut services = BTreeMap::new();
    for (name, source) in &spec.document.services {
        let (image, version) = source.image_version()?;
        let routes = source.routes(&spec.name, name)?;
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
        // 重新应用时 `set_routes` 会把 enable 置回 true；只有用户在原始 labels 中
        // 显式声明过 false 时才需要把该声明写回，避免导出后语义发生变化。
        if source
            .labels
            .iter()
            .any(|label| label == "traefik.enable=false")
        {
            custom.labels.push(String::from("traefik.enable=false"));
        }
        let (network, external_network) = export_network(source, &spec.document);
        services.insert(
            name.clone(),
            AppServiceConfig {
                image,
                version,
                container_name: source.container_name.clone(),
                port,
                publish: source.ports.clone(),
                volumes: source
                    .volumes
                    .iter()
                    .filter(|value| !is_files_mount(value, &spec.name))
                    .cloned()
                    .collect(),
                environment: source.environment.clone(),
                command: source.command.clone(),
                restart: source.restart.clone(),
                env_file: source.env_file.clone(),
                network,
                external_network,
                labels: custom.labels,
                user: source.user.clone(),
                group_add: source.group_add.clone(),
                hooks: hooks.get(name).cloned(),
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
        authelia,
        services,
    })
}

/// 从当前 IR 与 `.env` 字段重建 Traefik 模板。
///
/// # 错误
///
/// 必需服务、端口或项目环境字段缺失时返回错误。
pub(super) fn export_traefik(spec: &StackSpec, config: &Config) -> anyhow::Result<TraefikConfig> {
    let service = only_named_service(spec, "traefik")?;
    let routes = service.routes(&spec.name, "traefik")?;
    let dashboard_host = routes
        .first()
        .and_then(|route| route.hosts.first())
        .cloned()
        .unwrap_or_else(|| format!("traefik.{}", config.domain));
    let dashboard_authelia = service.labels.iter().any(|label| {
        label
            .strip_prefix("traefik.http.routers.dashboard.middlewares=")
            .is_some_and(|value| value.split(',').any(|name| name == "authelia@file"))
    });
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
    // 指标入口只在启动参数里声明，容器端口始终是配置的 metrics_port。
    let metrics = service
        .command
        .iter()
        .any(|argument| argument == "--metrics.prometheus=true");
    let metrics_port = service
        .command
        .iter()
        .find_map(|argument| argument.strip_prefix("--entrypoints.metrics.address=:"))
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(crate::spec::DEFAULT_METRICS_PORT);
    let middlewares: BTreeMap<String, TraefikMiddlewareConfig> =
        match spec.environment.get(super::traefik::MIDDLEWARES_KEY) {
            Some(value) => serde_json::from_str(value)?,
            None => BTreeMap::new(),
        };
    Ok(TraefikConfig {
        format: FORMAT_VERSION,
        template: String::from("traefik"),
        name: spec.name.clone(),
        domain,
        acme_email: required_env(spec, "ACME_EMAIL")?,
        cloudflare_token: required_env(spec, "CF_DNS_API_TOKEN")?,
        version: required_env(spec, "TRAEFIK_VERSION")?,
        http_port,
        https_port,
        dashboard_authelia,
        metrics,
        metrics_port,
        middlewares,
    })
}

/// 从当前 IR 字段重建静态站点模板。
///
/// # 错误
///
/// 必需服务、路由或项目环境字段缺失时返回错误。
pub(super) fn export_static(spec: &StackSpec) -> anyhow::Result<StaticConfig> {
    let service = only_named_service(spec, "web")?;
    let route = service
        .routes(&spec.name, "web")?
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
        user: service.user.clone(),
        group_add: service.group_add.clone(),
        hooks: spec.project_hooks()?.get("web").cloned(),
    })
}

/// 将单条语义路由压缩为紧凑形式，或导出多条详细路由。
fn routes_to_config(routes: &[Route]) -> Option<TraefikRoutesConfig> {
    if routes.is_empty() {
        return None;
    }
    if routes.len() == 1 && routes[0].name == "default" {
        let route = &routes[0];
        return Some(TraefikRoutesConfig {
            hosts: route.hosts.clone(),
            path_prefix: route.path_prefix.clone(),
            middlewares: route.middlewares.clone(),
            protocol: route.protocol.into(),
            entrypoint: route.entrypoint_name().to_string(),
            sticky_cookie: route.sticky_cookie,
            pass_host_header: route.pass_host_header,
            priority: route.priority,
            routes: BTreeMap::new(),
        });
    }
    Some(TraefikRoutesConfig {
        routes: routes
            .iter()
            .map(|route| {
                (
                    route.name.clone(),
                    TraefikRouteConfig {
                        hosts: route.hosts.clone(),
                        path_prefix: route.path_prefix.clone(),
                        port: Some(route.container_port),
                        middlewares: route.middlewares.clone(),
                        protocol: route.protocol.into(),
                        entrypoint: route.entrypoint_name().to_string(),
                        sticky_cookie: route.sticky_cookie,
                        pass_host_header: route.pass_host_header,
                        priority: route.priority,
                    },
                )
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
///
/// # 错误
///
/// 健康检查不是受支持的 `CMD` 或 `CMD-SHELL` 形式时返回错误。
fn healthcheck_to_config(value: &Healthcheck) -> anyhow::Result<HealthcheckConfig> {
    let command = match value.shell_command() {
        Ok(command) => HealthcheckCommand::Shell(command.to_string()),
        Err(_) => HealthcheckCommand::Exec(value.exec_arguments()?.to_vec()),
    };
    Ok(HealthcheckConfig {
        command,
        interval: value.interval.clone(),
        timeout: value.timeout.clone(),
        start_period: value.start_period.clone(),
        retries: value.retries,
    })
}

/// 从服务元数据标签推导唯一的项目模板类型。
///
/// # 错误
///
/// 项目没有任何模板标记，或标记互相冲突时返回错误。
pub fn detect_kind(spec: &StackSpec) -> anyhow::Result<TemplateKind> {
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

/// 判断挂载是否为 `--files` 注入的 `files/` 目录源。
///
/// 同一条挂载可能写成绝对路径（`/var/lib/nsetup/stacks/<项目>/files`）或相对项目
/// 目录的路径（`files`、`./files`）；生成 Compose 时相对写法已经展开为绝对路径，
/// 因此这里按目录分量比较，导出时把两种来源都过滤掉，避免重新应用时与 `--files`
/// 自动注入的挂载重复。
fn is_files_mount(value: &str, project_name: &str) -> bool {
    let Ok(mount) = crate::spec::BindMount::parse(value) else {
        return false;
    };
    let source = std::path::Path::new(mount.host_path.trim_end_matches('/'));
    let directory = crate::template::files::FILES_DIRECTORY;
    source.file_name().is_some_and(|name| name == directory)
        && source
            .parent()
            .and_then(std::path::Path::file_name)
            .is_some_and(|name| name == project_name)
}

/// 从 Compose 网络字段推导高层网络模式。
fn export_network(service: &Service, document: &Document) -> (AppNetwork, Option<String>) {
    if service.network_mode.as_deref() == Some("host") {
        return (AppNetwork::Host, None);
    }
    for key in &service.networks {
        if key == "proxy" || key == "project" {
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

#[cfg(test)]
mod tests {
    use super::is_files_mount;

    /// `--files` 注入的挂载无论写成绝对还是相对路径都要在导出时被过滤（R6）。
    #[test]
    fn filters_injected_files_mounts() {
        assert!(is_files_mount(
            "/var/lib/nsetup/stacks/observability/files:/opt/nsetup/files:ro",
            "observability"
        ));
        assert!(is_files_mount(
            "/var/lib/nsetup/stacks/observability/./files:/opt/nsetup/files:ro",
            "observability"
        ));
        assert!(!is_files_mount(
            "files/prometheus:/etc/prometheus:ro",
            "demo"
        ));
        assert!(!is_files_mount(
            "/srv/data/demo/files/x.yml:/etc/x.yml:ro",
            "demo"
        ));
    }
}
