//! 将各类 TOML 声明展开为统一 IR 与附属文件。

use super::{
    AppConfig, AppNetwork, AppServiceConfig, FORMAT_VERSION, HealthcheckConfig, StaticConfig,
    TemplateKind, TemplateOutput,
};
use crate::config::Config;
use crate::constants::PROXY_NETWORK;
use crate::spec::{
    Document, Healthcheck, Network, Route, RouteProtocol, Service, StackSpec, validate_name,
    validate_version,
};
use std::collections::BTreeMap;

/// 将应用文档转换为 Compose IR。
pub(super) fn generate_app(input: AppConfig, config: &Config) -> anyhow::Result<TemplateOutput> {
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

/// 将静态站点文档转换为 Compose IR。
pub(super) fn generate_static(
    input: StaticConfig,
    config: &Config,
) -> anyhow::Result<TemplateOutput> {
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

/// 将 TOML 健康检查转换为 Compose `CMD-SHELL` 形式。
fn healthcheck_from_config(value: HealthcheckConfig) -> Healthcheck {
    let mut healthcheck = Healthcheck::command(value.command);
    healthcheck.interval = value.interval;
    healthcheck.timeout = value.timeout;
    healthcheck.start_period = value.start_period;
    healthcheck.retries = value.retries;
    healthcheck
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
            "authelia" | "gzip" | "forwarded-headers" | "internal-only" | "tls"
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
