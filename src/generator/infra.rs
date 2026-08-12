//! Traefik 基础设施生成。

use super::compose::{self, Document, Healthcheck, Logging, Network, Service};
use super::{GeneratedFile, GeneratedStack, InfraSpec};
use std::path::PathBuf;

/// Traefik 外部网络名。
const TRAEFIK_NETWORK: &str = "nihility-traefik";

/// 生成 Traefik 项目。
pub fn generate_infrastructure(
    spec: &InfraSpec,
    data_root: &std::path::Path,
) -> anyhow::Result<Vec<GeneratedStack>> {
    validate(spec)?;
    Ok(vec![generate_traefik(spec, data_root)?])
}

/// 生成 Traefik 项目。
fn generate_traefik(
    spec: &InfraSpec,
    data_root: &std::path::Path,
) -> anyhow::Result<GeneratedStack> {
    let mut document = Document::default();
    document.networks.insert(
        String::from("traefik"),
        Network {
            name: Some(String::from(TRAEFIK_NETWORK)),
            external: None,
        },
    );
    let dashboard_host = format!("traefik.{}", spec.domain);
    document.services.insert(
        String::from("traefik"),
        Service {
            image: String::from("traefik:${TRAEFIK_VERSION}"),
            container_name: Some(String::from("nihility-traefik")),
            command: traefik_command(spec.https_port),
            restart: Some(String::from("unless-stopped")),
            networks: vec![String::from("traefik")],
            ports: vec![
                format!("{}:80", spec.http_port),
                format!("{}:443", spec.https_port),
                format!("{}:443/udp", spec.https_port),
            ],
            volumes: vec![
                String::from("/var/run/docker.sock:/var/run/docker.sock:ro"),
                String::from("./config:/etc/traefik/config:ro"),
                format!("{}:/data", data_root.join("traefik").display()),
            ],
            env_file: vec![String::from(".env")],
            labels: vec![
                String::from("traefik.enable=true"),
                String::from("traefik.docker.network=nihility-traefik"),
                String::from("traefik.http.routers.dashboard.entrypoints=https"),
                format!("traefik.http.routers.dashboard.rule=Host(`{dashboard_host}`)"),
                String::from("traefik.http.routers.dashboard.service=api@internal"),
                String::from("traefik.http.routers.dashboard.tls=true"),
                String::from("traefik.http.routers.dashboard.tls.certresolver=cloudflare"),
                format!(
                    "traefik.http.routers.dashboard.tls.domains[0].main={}",
                    spec.domain
                ),
                format!(
                    "traefik.http.routers.dashboard.tls.domains[0].sans=*.{}",
                    spec.domain
                ),
                String::from("traefik.http.routers.dashboard.middlewares=internal-only@file"),
            ],
            healthcheck: Some(Healthcheck {
                test: vec![
                    String::from("CMD"),
                    String::from("traefik"),
                    String::from("healthcheck"),
                    String::from("--ping"),
                ],
                interval: String::from("10s"),
                timeout: String::from("3s"),
                start_period: None,
                retries: 3,
            }),
            logging: Some(Logging {
                driver: String::from("json-file"),
                options: [
                    (String::from("max-size"), String::from("10m")),
                    (String::from("max-file"), String::from("3")),
                ]
                .into_iter()
                .collect(),
            }),
            ..Service::default()
        },
    );
    let env_file = format!(
        "TRAEFIK_VERSION={}\nACME_EMAIL={}\nCF_DNS_API_TOKEN={}\n",
        quote_env(&spec.traefik_version),
        quote_env(&spec.acme_email),
        quote_env(&spec.cloudflare_token)
    );
    Ok(GeneratedStack {
        name: String::from("traefik"),
        compose_yaml: compose::to_yaml(&document)?,
        env_file,
        files: middleware_files(spec.https_port),
    })
}

/// 构造 Traefik 启动参数。
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
        "--providers.docker.network=nihility-traefik",
        "--providers.file=true",
        "--providers.file.directory=/etc/traefik/config",
        "--providers.file.watch=true",
        "--entrypoints.http.address=:80",
        "--entrypoints.http.http.redirections.entrypoint.scheme=https",
        "--entrypoints.http.http.redirections.entrypoint.permanent=true",
        "--entrypoints.https.address=:443",
        "--entrypoints.https.asdefault=true",
        "--entrypoints.https.http3=true",
        "--certificatesresolvers.cloudflare.acme.email=${ACME_EMAIL}",
        "--certificatesresolvers.cloudflare.acme.storage=/data/acme.json",
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

/// 构造 Traefik 文件中间件。
fn middleware_files(https_port: u16) -> Vec<GeneratedFile> {
    [
        ("config/tls.yml", TLS_OPTIONS.as_bytes().to_vec()),
        ("config/gzip.yml", GZIP.as_bytes().to_vec()),
        (
            "config/forwarded-headers.yml",
            forwarded_headers(https_port).into_bytes(),
        ),
        (
            "config/internal-only.yml",
            INTERNAL_ONLY.as_bytes().to_vec(),
        ),
    ]
    .into_iter()
    .map(|(path, content)| GeneratedFile {
        path: PathBuf::from(path),
        content,
        mode: 0o640,
    })
    .collect()
}

/// 校验基础设施生成参数。
fn validate(spec: &InfraSpec) -> anyhow::Result<()> {
    for (label, value) in [
        ("主域名", spec.domain.as_str()),
        ("ACME 邮箱", spec.acme_email.as_str()),
        ("Cloudflare 令牌", spec.cloudflare_token.as_str()),
        ("Traefik 版本", spec.traefik_version.as_str()),
    ] {
        if value.trim().is_empty() || value.contains(['\n', '\r', '\0']) {
            anyhow::bail!("{label}不能为空或包含换行/空字符");
        }
    }
    if !spec.domain.contains('.') || spec.domain.contains(['/', '`', ' ']) {
        anyhow::bail!("主域名格式无效: {}", spec.domain);
    }
    if !crate::orchestrator::valid_image_version(&spec.traefik_version) {
        anyhow::bail!("Traefik 版本标签格式无效，且不能使用 latest");
    }
    if spec.http_port == spec.https_port {
        anyhow::bail!("Traefik HTTP 和 HTTPS 宿主机端口不能相同");
    }
    Ok(())
}

/// 生成与外部 HTTPS 端口一致的转发请求头中间件。
fn forwarded_headers(https_port: u16) -> String {
    format!(
        "http:\n  middlewares:\n    forwarded-headers:\n      headers:\n        customRequestHeaders:\n          X-Forwarded-Proto: https\n          X-Forwarded-Ssl: on\n          X-Forwarded-Port: '{https_port}'\n"
    )
}

/// 引用环境变量文件中的值。
fn quote_env(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `GZip` 中间件配置。
const GZIP: &str = "http:\n  middlewares:\n    gzip:\n      compress: {}\n";
/// 默认 TLS 选项；现代密码套件交由 Traefik 与 Go 安全默认值管理。
const TLS_OPTIONS: &str = "tls:\n  options:\n    default:\n      minVersion: VersionTLS12\n";
/// 内网地址白名单中间件配置。
const INTERNAL_ONLY: &str = "http:\n  middlewares:\n    internal-only:\n      ipAllowList:\n        sourceRange:\n          - 127.0.0.0/8\n          - 10.0.0.0/8\n          - 172.16.0.0/12\n          - 192.168.0.0/16\n";
