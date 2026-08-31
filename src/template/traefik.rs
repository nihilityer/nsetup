//! Traefik 基础设施模板，默认值继承 `main` 分支的成熟配置。

use super::{FORMAT_VERSION, GeneratedFile, TemplateKind, TemplateOutput, TraefikConfig};
use crate::config::Config;
use crate::constants::PROXY_NETWORK;
use crate::spec::{Document, Healthcheck, Logging, Network, Service, StackSpec, validate_version};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 导出时持久化 Traefik 主域名的项目环境键。
pub(super) const DOMAIN_KEY: &str = "NSETUP_TRAEFIK_DOMAIN";

/// 将 Traefik TOML 转换为 IR 与附属文件。
pub(super) fn generate(input: TraefikConfig, config: &Config) -> anyhow::Result<TemplateOutput> {
    validate(&input)?;
    let directory = config.stacks_root.join("traefik");
    let dashboard_host = format!("traefik.{}", input.domain);
    let mut document = Document::default();
    document.networks.insert(
        String::from("proxy"),
        Network {
            external: false,
            name: Some(String::from(PROXY_NETWORK)),
        },
    );
    let mut healthcheck = Healthcheck::command(String::from("traefik healthcheck --ping"));
    healthcheck.interval = Some(String::from("10s"));
    healthcheck.timeout = Some(String::from("3s"));
    healthcheck.retries = Some(3);
    let service = Service {
        image: String::from("traefik:${TRAEFIK_VERSION}"),
        container_name: Some(String::from("traefik")),
        command: traefik_command(input.https_port),
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
        labels: dashboard_labels(&dashboard_host, &input.domain),
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
    document.services.insert(String::from("traefik"), service);
    let spec = StackSpec {
        name: String::from("traefik"),
        document,
        environment: BTreeMap::from([
            (String::from("TRAEFIK_VERSION"), input.version),
            (String::from("ACME_EMAIL"), input.acme_email),
            (String::from("CF_DNS_API_TOKEN"), input.cloudflare_token),
            (String::from(DOMAIN_KEY), input.domain),
        ]),
    };
    spec.validate()?;
    Ok(TemplateOutput {
        spec,
        files: vec![
            GeneratedFile {
                path: PathBuf::from("config/dynamic.yml"),
                content: dynamic_config(input.https_port).into_bytes(),
                mode: 0o640,
                replace: true,
            },
            GeneratedFile {
                path: PathBuf::from("config/acme.json"),
                content: b"{}\n".to_vec(),
                mode: 0o600,
                replace: false,
            },
        ],
        kind: TemplateKind::Traefik,
    })
}

/// 构造沿用 main 分支默认行为的 Traefik 启动参数。
fn traefik_command(https_port: u16) -> Vec<String> {
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
        "--metrics.prometheus=false",
        "--tracing=false",
        "--providers.docker=true",
        "--providers.docker.endpoint=unix:///var/run/docker.sock",
        "--providers.docker.watch=true",
        "--providers.docker.exposedbydefault=false",
        "--providers.docker.usebindportip=false",
        "--providers.docker.network=nsetup-proxy",
        "--providers.file=true",
        "--providers.file.filename=/etc/traefik/dynamic.yml",
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
        format!("--entrypoints.http.http.redirections.entrypoint.to=:{https_port}"),
        format!("--entrypoints.https.http3.advertisedport={https_port}"),
    ]);
    command
}

/// 构造 dashboard 到 `api@internal` 的官方推荐路由标签。
fn dashboard_labels(host: &str, domain: &str) -> Vec<String> {
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
        String::from("traefik.http.routers.dashboard.middlewares=internal-only@file"),
    ]
}

/// 构造 main 分支中间件默认值，并保留当前架构新增的 Authelia 与 TLS 响应头。
fn dynamic_config(https_port: u16) -> String {
    format!(
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
          X-Forwarded-Port: '{https_port}'
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
tls:
  options:
    default:
      minVersion: VersionTLS12
"#,
    )
}

/// 校验 Traefik TOML 的格式、凭据与端口。
fn validate(input: &TraefikConfig) -> anyhow::Result<()> {
    if input.format != FORMAT_VERSION {
        anyhow::bail!("不支持的配置 format: {}", input.format);
    }
    if input.template != "traefik" {
        anyhow::bail!("Traefik 配置的 template 必须为 traefik");
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
