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

/// CLI 输出的带注释 Authelia 基础设施模板。
pub(super) const AUTHELIA_SKELETON: &str = r#"# Authelia 基础认证设施模板
# 先用以下命令交互生成 password_hash：
# docker run --rm --pull=never -it authelia/authelia:4.39.20 authelia crypto hash generate argon2
# 三个密钥分别运行一次：openssl rand -hex 32
# 重点：[users.admin] 中的 admin 是登录用户名；修改登录名要修改表名，
# 不是只修改 display_name 或 email。应用变更后运行 nsetup restart authelia。
format = 1
template = "authelia"
host = "auth"
version = "4.39.20"
default_redirection_url = "https://example.com"
default_policy = "one_factor"
jwt_secret = "replace-with-at-least-32-random-characters"
session_secret = "replace-with-at-least-32-random-characters"
storage_encryption_key = "replace-with-at-least-32-random-characters"

[users.admin]
display_name = "Administrator"
password_hash = '$argon2id$replace-with-generated-password-hash'
email = "admin@example.com"
groups = ["admins"]
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
