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
    let acme = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("acme.json"))
        .ok_or_else(|| anyhow::anyhow!("missing acme.json"))?;
    assert!(!acme.replace);
    let exported = export(&generated.spec, &config)?;
    let regenerated = apply(&exported, &config)?;
    assert_eq!(generated.spec, regenerated.spec);
    Ok(())
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
