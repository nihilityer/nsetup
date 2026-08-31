//! `template` 模块行为测试。

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

/// 应用路由可引用由基础设施提供的 Authelia 中间件。
#[test]
fn app_template_uses_authelia_middleware() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "protected"
[services.web]
image = "example/web"
version = "1.2"
port = 8080
[services.web.traefik]
hosts = ["protected"]
middlewares = ["authelia", "tls"]
"#;
    let generated = apply(input, &Config::default())?;
    let labels = &generated.spec.document.services["web"].labels;
    assert!(labels.iter().any(|label| {
        label == "traefik.http.routers.nsetup-protected-web-1.middlewares=authelia@file,tls@file"
    }));
    Ok(())
}

/// Authelia 的声明、Compose 状态、用户库和文件型密钥可稳定重建。
#[test]
fn authelia_template_round_trip() -> anyhow::Result<()> {
    let input = valid_authelia_config();
    let config = Config::default();
    let generated = apply(input, &config)?;
    assert_eq!(generated.kind, TemplateKind::Authelia);
    assert_eq!(generated.spec.name, "authelia");
    let service = &generated.spec.document.services["authelia"];
    assert_eq!(service.image, "authelia/authelia:${AUTHELIA_VERSION}");
    assert!(
        service
            .volumes
            .iter()
            .any(|mount| mount.ends_with("/authelia:/data"))
    );
    let jwt = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("JWT_SECRET"))
        .ok_or_else(|| anyhow::anyhow!("missing JWT_SECRET"))?;
    assert_eq!(jwt.mode, 0o600);
    let configuration = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("configuration.yml"))
        .ok_or_else(|| anyhow::anyhow!("missing configuration.yml"))?;
    let configuration = String::from_utf8(configuration.content.clone())?;
    let _configuration_yaml: serde_yaml::Value = serde_yaml::from_str(&configuration)?;
    assert!(configuration.contains("implementation: 'ForwardAuth'"));
    assert!(!configuration.contains("jwt-secret-value"));
    let exported = export(&generated.spec, &config)?;
    let regenerated = apply(&exported, &config)?;
    assert_eq!(generated, regenerated);
    Ok(())
}

/// Authelia 模板拒绝占位密钥和占位密码哈希。
#[test]
fn authelia_template_rejects_placeholders() {
    let result = apply(super::skeleton::AUTHELIA_SKELETON, &Config::default());
    assert!(result.is_err());
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
    let service = &generated.spec.document.services["traefik"];
    assert!(service.labels.contains(&String::from(
        "traefik.http.routers.dashboard.service=api@internal"
    )));
    assert!(service.labels.iter().all(|label| {
        !label.starts_with("traefik.http.services.dashboard.loadbalancer.server.port=")
    }));
    assert!(service.command.contains(&String::from(
        "--certificatesresolvers.cloudflare.acme.keytype=EC256"
    )));
    assert!(service.command.contains(&String::from(
        "--certificatesresolvers.cloudflare.acme.dnschallenge.propagation.delaybeforechecks=30s"
    )));
    assert_eq!(
        service.logging.as_ref().map(|value| value.driver.as_str()),
        Some("json-file")
    );
    assert!(service.healthcheck.is_some());
    let acme = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("acme.json"))
        .ok_or_else(|| anyhow::anyhow!("missing acme.json"))?;
    assert!(!acme.replace);
    let dynamic = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("dynamic.yml"))
        .ok_or_else(|| anyhow::anyhow!("missing dynamic.yml"))?;
    let dynamic = String::from_utf8(dynamic.content.clone())?;
    assert!(dynamic.contains("address: 'http://authelia:9091/api/authz/forward-auth'"));
    assert!(dynamic.contains("X-Forwarded-Port: '8443'"));
    assert!(!dynamic.contains("defaultGeneratedCert"));
    let exported = export(&generated.spec, &config)?;
    let regenerated = apply(&exported, &config)?;
    assert_eq!(generated.spec, regenerated.spec);
    Ok(())
}

/// 返回包含有效占位测试值的 Authelia TOML。
fn valid_authelia_config() -> &'static str {
    r#"
format = 1
template = "authelia"
host = "auth"
version = "4.39.20"
default_redirection_url = "https://example.com"
default_policy = "two_factor"
jwt_secret = "jwt-secret-value-0123456789-abcdef"
session_secret = "session-secret-value-0123456789-ab"
storage_encryption_key = "storage-secret-value-0123456789-a"

[users.admin]
display_name = "Administrator"
password_hash = '$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA'
email = "admin@example.com"
groups = ["admins"]
"#
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
