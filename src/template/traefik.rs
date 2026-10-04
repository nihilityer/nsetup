//! Traefik 基础设施模板，默认值继承 `main` 分支的成熟配置。

use super::{
    FORMAT_VERSION, GeneratedFile, TemplateKind, TemplateOutput, TraefikConfig,
    TraefikMiddlewareValue,
};
use crate::config::Config;
use crate::constants::PROXY_NETWORK;
use crate::spec::{
    Document, Healthcheck, Logging, METRICS_ENTRYPOINT, Network, Service, StackSpec,
    validate_entrypoints, validate_version,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 导出时持久化 Traefik 主域名的项目环境键。
pub(super) const DOMAIN_KEY: &str = "NSETUP_TRAEFIK_DOMAIN";
/// 导出时持久化自定义中间件的项目环境键。
pub(super) const MIDDLEWARES_KEY: &str = "NSETUP_TRAEFIK_MIDDLEWARES_JSON";
/// Traefik 模板生成的动态配置在容器内的目录。
const DYNAMIC_DIRECTORY: &str = "/etc/traefik/dynamic";
/// 动态配置目录中由 nsetup 拥有的文件名。
const DYNAMIC_FILE: &str = "nsetup.yml";

/// 将 Traefik TOML 转换为 IR 与附属文件。
pub(super) fn generate(input: &TraefikConfig, config: &Config) -> anyhow::Result<TemplateOutput> {
    validate(input)?;
    let project = input.name.as_str();
    let directory = config.stacks_root.join(project);
    let dashboard_host = format!("traefik.{}", input.domain);
    let mut document = Document::default();
    document.networks.insert(
        String::from("proxy"),
        Network {
            external: false,
            name: Some(String::from(PROXY_NETWORK)),
        },
    );
    // `traefik healthcheck` 自带 ping 客户端，不需要镜像里存在 shell。
    let healthcheck = Healthcheck::exec(&[String::from("traefik"), String::from("healthcheck")])?;
    let mut service = Service {
        image: String::from("traefik:${TRAEFIK_VERSION}"),
        container_name: Some(String::from("traefik")),
        command: traefik_command(input),
        restart: Some(String::from("unless-stopped")),
        networks: vec![String::from("proxy")],
        ports: traefik_ports(input),
        volumes: vec![
            format!("{}:/var/run/docker.sock:ro", config.docker_socket.display()),
            format!(
                "{}/config/dynamic:{DYNAMIC_DIRECTORY}:ro",
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
        labels: dashboard_labels(&dashboard_host, &input.domain, input.dashboard_authelia),
        healthcheck: Some(healthcheck),
        logging: Some(Logging {
            driver: String::from("json-file"),
            options: BTreeMap::from([
                (String::from("max-size"), String::from("10m")),
                (String::from("max-file"), String::from("3")),
            ]),
        }),
        ..Service::default()
    };
    service.set_routes(
        project,
        project,
        &[crate::spec::Route {
            name: String::from("dashboard"),
            hosts: vec![dashboard_host],
            path_prefix: None,
            container_port: 8080,
            middlewares: Vec::new(),
            protocol: crate::spec::RouteProtocol::Http,
            entrypoint: String::from("https"),
            sticky_cookie: false,
            pass_host_header: None,
            priority: None,
        }],
    )?;
    document.services.insert(String::from("traefik"), service);
    let mut environment = BTreeMap::from([
        (String::from("TRAEFIK_VERSION"), input.version.clone()),
        (String::from("ACME_EMAIL"), input.acme_email.clone()),
        (
            String::from("CF_DNS_API_TOKEN"),
            input.cloudflare_token.clone(),
        ),
        (String::from(DOMAIN_KEY), input.domain.clone()),
    ]);
    if !input.middlewares.is_empty() {
        environment.insert(
            String::from(MIDDLEWARES_KEY),
            serde_json::to_string(&input.middlewares)?,
        );
    }
    let spec = StackSpec {
        name: project.to_string(),
        document,
        environment,
    };
    spec.validate()?;
    Ok(TemplateOutput {
        spec,
        files: vec![
            GeneratedFile {
                path: PathBuf::from("config/dynamic").join(DYNAMIC_FILE),
                content: dynamic_config(input).into_bytes(),
                mode: 0o640,
                directory_mode: crate::template::PRIVATE_DIRECTORY_MODE,
                replace: true,
                overwrite: true,
            },
            // 用户可以在同一目录里追加自己的动态配置文件，traefik 的 file
            // provider 会加载目录中的全部 `*.yml`，因此 `nsetup up` 重写
            // nsetup.yml 时不会清除手工追加的路由与中间件。
            GeneratedFile {
                path: PathBuf::from("config/dynamic/custom.yml"),
                content: USER_DYNAMIC_TEMPLATE.as_bytes().to_vec(),
                mode: 0o640,
                directory_mode: crate::template::PRIVATE_DIRECTORY_MODE,
                replace: false,
                // 用户拥有的文件：只做首次生成，之后不再覆盖。
                overwrite: false,
            },
            GeneratedFile {
                path: PathBuf::from("config/acme.json"),
                content: b"{}\n".to_vec(),
                mode: 0o600,
                directory_mode: crate::template::PRIVATE_DIRECTORY_MODE,
                replace: false,
                // 已签发的 ACME 证书不能被覆盖，否则每次应用都会重新申请。
                overwrite: false,
            },
        ],
        kind: TemplateKind::Traefik,
    })
}

/// 用户可以自由编辑的附加动态配置模板。
const USER_DYNAMIC_TEMPLATE: &str = "\
# 手工追加的 Traefik 动态配置。
#
# 本文件与 nsetup.yml 位于同一个目录，traefik 的 file provider 会加载目录中所有
# `*.yml`，因此这里的路由与中间件不会被 `nsetup up -f traefik.toml` 清除。
# 也可以在同目录新增其它 `*.yml` 文件，效果相同。
http:
  routers: {}
  services: {}
  middlewares: {}
";

/// 构造沿用 main 分支默认行为的 Traefik 启动参数。
fn traefik_command(input: &TraefikConfig) -> Vec<String> {
    let mut command = [
        "--global.sendanonymoususage=false",
        "--global.checknewversion=false",
        "--api=true",
        "--api.dashboard=true",
        "--api.debug=false",
        "--api.disabledashboardad=true",
        "--api.insecure=false",
        "--ping=true",
        "--log.level=INFO",
        "--log.format=common",
        "--log.nocolor=true",
        "--accesslog=false",
        "--tracing=false",
        "--providers.docker=true",
        "--providers.docker.endpoint=unix:///var/run/docker.sock",
        "--providers.docker.watch=true",
        "--providers.docker.exposedbydefault=false",
        "--providers.docker.usebindportip=false",
        "--providers.docker.network=nsetup-proxy",
        "--providers.file=true",
        "--providers.file.directory=/etc/traefik/dynamic",
        "--providers.file.watch=true",
        "--entrypoints.http.address=:80",
        "--entrypoints.http.http.redirections.entrypoint.scheme=https",
        "--entrypoints.http.http.redirections.entrypoint.permanent=true",
        "--entrypoints.https.address=:443",
        "--entrypoints.https.asdefault=true",
        "--entrypoints.https.http3=true",
        "--certificatesresolvers.cloudflare.acme.email=${ACME_EMAIL}",
        "--certificatesresolvers.cloudflare.acme.storage=/acme.json",
        "--certificatesresolvers.cloudflare.acme.keytype=EC256",
        "--certificatesresolvers.cloudflare.acme.dnschallenge=true",
        "--certificatesresolvers.cloudflare.acme.dnschallenge.provider=cloudflare",
        "--certificatesresolvers.cloudflare.acme.dnschallenge.resolvers=1.1.1.1:53,8.8.8.8:53",
        "--certificatesresolvers.cloudflare.acme.dnschallenge.propagation.delaybeforechecks=30s",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    command.extend([
        format!(
            "--entrypoints.http.http.redirections.entrypoint.to=:{}",
            input.https_port
        ),
        format!(
            "--entrypoints.https.http3.advertisedport={}",
            input.https_port
        ),
    ]);
    if input.metrics {
        // 指标只在内网入口暴露，宿主机端口固定绑定到回环地址，避免暴露到 LAN。
        command.extend([
            format!(
                "--entrypoints.{METRICS_ENTRYPOINT}.address=:{}",
                input.metrics_port
            ),
            "--metrics.prometheus=true".to_string(),
            "--metrics.prometheus.addentrypointslabels=true".to_string(),
            "--metrics.prometheus.addserviceslabels=true".to_string(),
        ]);
    }
    command
}

/// 返回 Traefik 的发布端口，指标开启时额外绑定到宿主机回环地址。
fn traefik_ports(input: &TraefikConfig) -> Vec<String> {
    let mut ports = vec![
        format!("{}:80/tcp", input.http_port),
        format!("{}:443/tcp", input.https_port),
        format!("{}:443/udp", input.https_port),
    ];
    if input.metrics {
        ports.push(format!(
            "127.0.0.1:{}:{}/tcp",
            input.metrics_port, input.metrics_port
        ));
    }
    ports
}

/// 构造 dashboard 到 `api@internal` 的官方推荐路由标签。
fn dashboard_labels(host: &str, domain: &str, authelia: bool) -> Vec<String> {
    let middlewares = if authelia {
        "internal-only@file,authelia@file"
    } else {
        "internal-only@file"
    };
    vec![
        String::from("io.nsetup.template=traefik"),
        String::from("traefik.enable=true"),
        format!("traefik.docker.network={PROXY_NETWORK}"),
        String::from("traefik.http.routers.dashboard.entrypoints=https"),
        format!("traefik.http.routers.dashboard.rule=Host(`{host}`)"),
        String::from("traefik.http.routers.dashboard.service=api@internal"),
        String::from("traefik.http.routers.dashboard.tls=true"),
        String::from("traefik.http.routers.dashboard.tls.certresolver=cloudflare"),
        format!("traefik.http.routers.dashboard.tls.domains[0].main={domain}"),
        format!("traefik.http.routers.dashboard.tls.domains[0].sans=*.{domain}"),
        format!("traefik.http.routers.dashboard.middlewares={middlewares}"),
    ]
}

/// 构造内置中间件与用户自定义中间件共用的动态配置。
fn dynamic_config(input: &TraefikConfig) -> String {
    let mut output = format!(
        r#"http:
  middlewares:
    authelia:
      forwardAuth:
        address: 'http://authelia:9091/api/authz/forward-auth'
        trustForwardHeader: true
        maxResponseBodySize: 8192
        authResponseHeaders:
          - Remote-User
          - Remote-Groups
          - Remote-Email
          - Remote-Name
    gzip:
      compress: {{}}
    forwarded-headers:
      headers:
        customRequestHeaders:
          X-Forwarded-Proto: https
          X-Forwarded-Ssl: on
          X-Forwarded-Port: '{}'
    internal-only:
      ipAllowList:
        sourceRange:
          - 127.0.0.0/8
          - 10.0.0.0/8
          - 172.16.0.0/12
          - 192.168.0.0/16
    tls:
      headers:
        stsSeconds: 31536000
        stsIncludeSubdomains: true
"#,
        input.https_port
    );
    for (name, middleware) in &input.middlewares {
        output.push_str(&middleware_yaml(name, middleware));
    }
    output.push_str(
        r#"tls:
  options:
    default:
      minVersion: VersionTLS12
"#,
    );
    output
}

/// 将单个自定义中间件渲染为动态配置片段。
fn middleware_yaml(name: &str, middleware: &super::TraefikMiddlewareConfig) -> String {
    let mut output = format!("    {name}:\n      {}:\n", middleware.kind);
    for (key, value) in &middleware.args {
        match value {
            TraefikMiddlewareValue::Scalar(value) => {
                output.push_str(&format!("        {key}: {}\n", yaml_scalar(value)));
            }
            TraefikMiddlewareValue::Flag(value) => {
                output.push_str(&format!("        {key}: {value}\n"));
            }
            TraefikMiddlewareValue::List(values) => {
                output.push_str(&format!("        {key}:\n"));
                for value in values {
                    output.push_str(&format!("          - {}\n", yaml_scalar(value)));
                }
            }
        }
    }
    output
}

/// 按需为 YAML 标量加引号，避免 `:`、`#` 等字符改变语义。
fn yaml_scalar(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value
            .chars()
            .any(|character| character.is_whitespace() || ":#'\"{}[],&*?|>%@`".contains(character))
        || value.parse::<f64>().is_ok()
        || matches!(value, "true" | "false" | "null" | "yes" | "no");
    if needs_quotes {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        value.to_string()
    }
}

/// 校验 Traefik TOML 的格式、凭据、端口与自定义中间件。
fn validate(input: &TraefikConfig) -> anyhow::Result<()> {
    if input.format != FORMAT_VERSION {
        anyhow::bail!("不支持的配置 format: {}", input.format);
    }
    if input.template != "traefik" {
        anyhow::bail!("Traefik 配置的 template 必须为 traefik");
    }
    if input.name != "traefik" {
        anyhow::bail!(
            "Traefik 模板的项目名固定为 traefik，不能声明 name = \"{}\"",
            input.name
        );
    }
    crate::config::validate_domain(&input.domain)?;
    validate_version(&input.version)?;
    validate_email(&input.acme_email)?;
    if input.cloudflare_token.trim().is_empty()
        || input.cloudflare_token.contains(['\n', '\r', '\0'])
    {
        anyhow::bail!("cloudflare_token 不能为空或包含换行/空字符");
    }
    if input.http_port == input.https_port {
        anyhow::bail!("Traefik HTTP 和 HTTPS 宿主机端口不能相同");
    }
    if input.metrics
        && (input.metrics_port == input.http_port || input.metrics_port == input.https_port)
    {
        anyhow::bail!("metrics_port 不能与 HTTP 或 HTTPS 端口相同");
    }
    if input.metrics_port == 0 {
        anyhow::bail!("metrics_port 必须大于 0");
    }
    // 指标入口在容器内监听该端口，不能占用 Traefik 自身的 HTTP/HTTPS 端口。
    if input.metrics && matches!(input.metrics_port, 80 | 443) {
        anyhow::bail!("metrics_port 不能占用 Traefik 自身的 80/443 容器端口");
    }
    let _entrypoints = validate_entrypoints("https,http")?;
    for (name, middleware) in &input.middlewares {
        crate::spec::validate_middleware(name)?;
        if middleware.kind.trim().is_empty()
            || !middleware
                .kind
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            anyhow::bail!(
                "traefik.middlewares.{name} 的 kind 无效: {}",
                middleware.kind
            );
        }
        for key in middleware.args.keys() {
            if key.trim().is_empty()
                || key.len() > 128
                || key.contains(char::is_whitespace)
                || key.contains([':', '#'])
            {
                anyhow::bail!("traefik.middlewares.{name} 的参数名无效: {key}");
            }
        }
    }
    if input.metrics_port == 0 {
        anyhow::bail!("metrics_port 必须大于 0");
    }
    Ok(())
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
