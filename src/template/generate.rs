//! 将各类 TOML 声明展开为统一 IR 与附属文件。

use super::{
    AppConfig, AppNetwork, AppServiceConfig, FORMAT_VERSION, GeneratedFile, HealthcheckConfig,
    StaticConfig, TemplateKind, TemplateOutput, TraefikConfig,
};
use crate::config::Config;
use crate::constants::PROXY_NETWORK;
use crate::spec::{
    Document, Healthcheck, Network, Route, RouteProtocol, Service, StackSpec, validate_name,
    validate_version,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

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

/// 将 Traefik 文档转换为 IR 及其所属配置文件。
pub(super) fn generate_traefik(
    input: TraefikConfig,
    config: &Config,
) -> anyhow::Result<TemplateOutput> {
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
