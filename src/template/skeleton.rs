//! CLI 输出的带注释 TOML 配置骨架。

/// CLI 输出的带注释应用模板。
pub(super) const APP_SKELETON: &str = r#"# 容器应用模板（可省略 template = "app"）
format = 1
name = "media"

[services.web]
image = "ghcr.io/example/media"
version = "1.0"
port = 8080
publish = ["12780:8080/tcp"]
volumes = ["/var/lib/nsetup/data/media:/data"]
environment = { LOG_LEVEL = "info" }
network = "bridge"

[services.web.traefik]
hosts = ["media"]
middlewares = ["gzip", "internal-only"]
protocol = "http"
"#;

/// CLI 输出的带注释 Traefik 模板。
pub(super) const TRAEFIK_SKELETON: &str = r#"# 反向代理基础设施模板
format = 1
template = "traefik"
domain = "example.com"
acme_email = "admin@example.com"
cloudflare_token = "replace-me"
version = "v3.8.0"
http_port = 80
https_port = 443
"#;

/// CLI 输出的带注释静态站点模板。
pub(super) const STATIC_SKELETON: &str = r#"# 静态 Nginx 站点；使用 --assets 上传文件
format = 1
template = "static"
name = "docs"
host = "docs"
version = "1.27"
middlewares = ["gzip"]
"#;
