//! 将各类 TOML 声明展开为统一 IR 与附属文件。

use super::GeneratedFile;
use super::{
    APP_OIDC_CLIENTS_KEY, AppConfig, AppNetwork, AppServiceConfig, FORMAT_VERSION,
    HealthcheckCommand, HealthcheckConfig, PRIVATE_DIRECTORY_MODE, StaticConfig, TemplateKind,
    TemplateOutput, oidc,
};
use crate::config::Config;
use crate::spec::{
    Document, Healthcheck, Network, Route, RouteProtocol, Service, StackSpec, validate_entrypoints,
    validate_group, validate_hooks, validate_middleware, validate_name, validate_user,
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
    let mut environment = BTreeMap::new();
    if let Some(authelia) = &input.authelia {
        oidc::validate_clients(&authelia.oidc_clients)?;
        environment.insert(
            String::from(APP_OIDC_CLIENTS_KEY),
            serde_json::to_string(&authelia.oidc_clients)?,
        );
    }
    if input.services.is_empty() {
        anyhow::bail!("app 模板至少需要一个服务");
    }
    let mut document = Document::default();
    let mut hooks = BTreeMap::new();
    for (service_name, service_config) in input.services {
        validate_name("服务名", &service_name)?;
        validate_version(&service_config.version)?;
        validate_repository(&service_config.image)?;
        if let Some(user) = &service_config.user {
            validate_user(user)?;
        }
        for group in &service_config.group_add {
            validate_group(group)?;
        }
        if let Some(service_hooks) = &service_config.hooks {
            validate_hooks(&format!("服务 {service_name}"), service_hooks)?;
            if !service_hooks.is_empty() {
                hooks.insert(service_name.clone(), service_hooks.clone());
            }
        }
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
            user: service_config.user,
            group_add: service_config.group_add,
            healthcheck: service_config
                .healthcheck
                .map(healthcheck_from_config)
                .transpose()?,
            logging: service_config.logging,
            ..Service::default()
        };
        service.labels.push(String::from("io.nsetup.template=app"));
        match service_config.network {
            AppNetwork::Bridge if !routes.is_empty() => {
                crate::spec::add_proxy_network(&mut document, true);
                crate::spec::add_project_network(&mut document, &input.name);
                service.networks.push(String::from("proxy"));
                service.networks.push(String::from("project"));
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
                    crate::spec::add_proxy_network(&mut document, true);
                    crate::spec::add_project_network(&mut document, &input.name);
                    service.networks.push(String::from("proxy"));
                    service.networks.push(String::from("project"));
                }
            }
        }
        service.set_routes(&input.name, &service_name, &routes)?;
        document.services.insert(service_name, service);
    }
    let mut spec = StackSpec {
        name: input.name,
        document,
        environment,
    };
    spec.set_project_hooks(&hooks)?;
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
    if let Some(user) = &input.user {
        validate_user(user)?;
    }
    for group in &input.group_add {
        validate_group(group)?;
    }
    if let Some(hooks) = &input.hooks {
        validate_hooks("static 服务 web", hooks)?;
    }
    let host = expand_host(&input.host, &config.domain)?;
    let directory = config.stacks_root.join(&input.name);
    let mut document = Document::default();
    crate::spec::add_proxy_network(&mut document, true);
    // 静态站点把整个受管项目目录以只读方式挂进容器：站点文件放在 site/ 下，用户
    // 只要把 nginx.conf 放进项目目录就同时改写服务方式，不必再借助特权容器写入。
    let mut service = Service {
        image: String::from("nginx:${NGINX_VERSION}"),
        restart: Some(String::from("unless-stopped")),
        networks: vec![String::from("proxy")],
        volumes: vec![
            format!("{}/site:/usr/share/nginx/html:ro", directory.display()),
            format!("{}:/opt/nsetup:ro", directory.display()),
        ],
        environment: BTreeMap::from([
            (
                String::from("NGINX_ENTRYPOINT_WORKER_PROCESSES_AUTOTUNE"),
                String::from("1"),
            ),
            (
                String::from("NGINX_ENTRYPOINT_QUIET_LOGS"),
                String::from("1"),
            ),
        ]),
        labels: vec![String::from("io.nsetup.template=static")],
        user: input.user,
        group_add: input.group_add,
        ..Service::default()
    };
    service.set_routes(
        &input.name,
        "web",
        &[Route {
            name: String::from("default"),
            hosts: vec![host],
            path_prefix: None,
            container_port: Some(80),
            middlewares: input.middlewares,
            protocol: RouteProtocol::Http,
            entrypoint: String::from("https"),
            sticky_cookie: false,
            pass_host_header: None,
            priority: None,
            service: None,
            tls_domains: Vec::new(),
        }],
    )?;
    document.services.insert(String::from("web"), service);
    let mut spec = StackSpec {
        name: input.name,
        document,
        environment: BTreeMap::from([(String::from("NGINX_VERSION"), input.version)]),
    };
    if let Some(service_hooks) = &input.hooks {
        let mut hooks = BTreeMap::new();
        if !service_hooks.is_empty() {
            hooks.insert(String::from("web"), service_hooks.clone());
        }
        spec.set_project_hooks(&hooks)?;
    }
    spec.validate()?;
    Ok(TemplateOutput {
        spec,
        files: static_files(),
        kind: TemplateKind::Static,
    })
}

/// 静态站点最小的 nginx 站点配置。
///
/// 镜像自带的 `default.conf` 监听 80 并以 `/usr/share/nginx/html` 为根；这里补充
/// gzip、静态资源缓存与 `/.well-known/`、`/healthz` 直达，避免用户为了生产可用的
/// 默认值自建镜像。
fn static_files() -> Vec<GeneratedFile> {
    vec![GeneratedFile {
        path: PathBuf::from("config/nginx/default.conf"),
        content: STATIC_NGINX_CONFIG.as_bytes().to_vec(),
        mode: 0o644,
        directory_mode: PRIVATE_DIRECTORY_MODE,
        replace: true,
        overwrite: true,
    }]
}

/// 默认站点配置正文；正则中的反斜杠必须原样保留，因此使用原始字符串。
const STATIC_NGINX_CONFIG: &str = r"server {
    listen 80;
    server_name _;

    root /usr/share/nginx/html;
    index index.html;

    gzip on;
    gzip_types text/plain text/css application/javascript application/json image/svg+xml;
    gzip_min_length 1024;

    location = /healthz {
        access_log off;
        add_header Content-Type text/plain;
        return 200 'ok';
    }

    location ~* ^/(\.well-known/.*|[^/]+\.(css|js|mjs|png|jpe?g|gif|svg|webp|ico|woff2?))$ {
        access_log off;
        add_header Cache-Control 'public, max-age=604800';
        try_files $uri =404;
    }

    location / {
        try_files $uri $uri/ =404;
    }
}
";

/// 将紧凑或详细的 TOML 路由展开为语义路由。
fn app_routes(service: &AppServiceConfig, config: &Config) -> anyhow::Result<Vec<Route>> {
    let Some(traefik) = &service.traefik else {
        return Ok(Vec::new());
    };
    validate_middlewares(&traefik.middlewares)?;
    validate_entrypoint(&traefik.entrypoint)?;
    let mut output = Vec::new();
    if !traefik.hosts.is_empty() {
        let port = service
            .port
            .ok_or_else(|| anyhow::anyhow!("traefik.hosts 需要服务 port"))?;
        output.push(Route {
            name: String::from("default"),
            hosts: expand_hosts(&traefik.hosts, &config.domain)?,
            path_prefix: traefik.path_prefix.clone(),
            container_port: Some(port),
            middlewares: traefik.middlewares.clone(),
            protocol: traefik.protocol.into(),
            entrypoint: traefik.entrypoint.clone(),
            sticky_cookie: traefik.sticky_cookie,
            pass_host_header: traefik.pass_host_header,
            priority: traefik.priority,
            service: None,
            tls_domains: Vec::new(),
        });
    }
    for (route_name, route) in &traefik.routes {
        validate_middlewares(&route.middlewares)?;
        validate_entrypoint(&route.entrypoint)?;
        let port = route
            .port
            .or(service.port)
            .ok_or_else(|| anyhow::anyhow!("Traefik route 需要 route.port 或服务 port"))?;
        output.push(Route {
            name: route_name.clone(),
            hosts: expand_hosts(&route.hosts, &config.domain)?,
            path_prefix: route.path_prefix.clone(),
            container_port: Some(port),
            middlewares: if route.middlewares.is_empty() {
                traefik.middlewares.clone()
            } else {
                route.middlewares.clone()
            },
            protocol: route.protocol.into(),
            entrypoint: if route.entrypoint.trim().is_empty() {
                traefik.entrypoint.clone()
            } else {
                route.entrypoint.clone()
            },
            sticky_cookie: route.sticky_cookie,
            pass_host_header: route.pass_host_header,
            priority: route.priority,
            service: None,
            tls_domains: Vec::new(),
        });
    }
    if output.is_empty() {
        anyhow::bail!("[services.*.traefik] 至少需要 hosts 或 routes");
    }
    Ok(output)
}

/// 将 TOML 健康检查转换为 Compose 测试命令。
fn healthcheck_from_config(value: HealthcheckConfig) -> anyhow::Result<Healthcheck> {
    let mut healthcheck = match &value.command {
        HealthcheckCommand::Shell(command) => Healthcheck::command(command.clone()),
        HealthcheckCommand::Exec(arguments) => Healthcheck::exec(arguments)?,
    };
    healthcheck.interval = value.interval;
    healthcheck.timeout = value.timeout;
    healthcheck.start_period = value.start_period;
    healthcheck.retries = value.retries;
    healthcheck.validate()?;
    Ok(healthcheck)
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

/// 校验路由引用的 Traefik 中间件名称。
///
/// 内置中间件由 traefik 模板生成；其它名称视为用户通过 `[traefik.middlewares]`
/// 或 `files/` 追加的自定义中间件，因此只做安全性检查而不做白名单限制。
fn validate_middlewares(values: &[String]) -> anyhow::Result<()> {
    for value in values {
        validate_middleware(value)?;
    }
    Ok(())
}

/// 校验可选的路由入口列表。
fn validate_entrypoint(value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        return Ok(());
    }
    let _entrypoints = validate_entrypoints(value)?;
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
