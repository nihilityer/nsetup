//! CLI 输出的带注释 TOML 配置骨架。

/// CLI 输出的逐字段注释应用模板。
pub(super) const APP_SKELETON: &str = r#"# 通用容器应用模板
# 配置格式版本。必填；当前只支持 1。
format = 1

# 模板选择器。省略时就是 app；通常无需取消下一行注释。
# template = "app"

# Compose 项目名。必填；小写字母开头，只能使用小写字母、数字、-、_，最长 63 字符。
name = "media"

# services 是以服务名为键的表，可按相同格式增加 web、worker 等多个服务。
# 此处 web 是 Compose 服务名，命名规则与项目名相同。
[services.web]

# 镜像仓库。必填；只写仓库名，不能携带 :标签或 @摘要。
image = "ghcr.io/example/media"

# 固定镜像标签。必填；升级应用时修改这里，不能省略或使用 latest。
version = "1.0"

# 固定容器名。可选且通常无需设置，省略时由 Compose 按项目和服务名生成。
# container_name = "media"

# 默认容器端口。Traefik 紧凑路由和未写 port 的详细路由会使用它；不会发布宿主机端口。
port = 8080

# 宿主机端口映射。仅在需要绕过 Traefik 直接访问时启用；格式为
# [HOST_IP:]HOST_PORT:CONTAINER_PORT[/tcp|udp]。
# publish = ["127.0.0.1:12780:8080/tcp"]

# bind mount。宿主机路径必须是 data_roots/stacks_root 下的绝对路径；仅支持可选 :ro。
# volumes = ["/var/lib/nsetup/data/media:/data"]

# 容器环境变量。键和值都使用字符串；密钥应优先放在权限受控的文件中。
# environment = { LOG_LEVEL = "info", TZ = "Asia/Shanghai" }

# 覆盖镜像默认命令，按参数数组传递；无需覆盖镜像 CMD 时保持注释。
# command = ["--serve", "--port", "8080"]

# Compose 重启策略。省略时不额外设置；家庭服务器常用 unless-stopped。
# restart = "unless-stopped"

# 容器环境文件。路径相对于受管项目目录且不能包含 ..；文件必须已存在于该目录。
# env_file = ["service.env"]

# 网络模式：bridge（默认）| host | external。host 不能同时配置 Traefik 容器路由。
# network = "bridge"

# 仅当 network = "external" 时填写；该 Docker 网络必须已经存在。
# external_network = "my-network"

# 额外 Docker labels，格式为 KEY=VALUE。Traefik 路由 labels 由 nsetup 自动生成。
# labels = ["com.example.owner=infra"]

# 可选 CMD-SHELL 健康检查。启用本表时 command 必填，其余字段省略则沿用镜像/Compose 行为。
# [services.web.healthcheck]
# command = "wget -qO- http://127.0.0.1:8080/health || exit 1"
# interval = "30s"       # 两次检查的间隔。
# timeout = "3s"         # 单次检查的超时时间。
# start_period = "20s"   # 容器启动后的失败宽限期。
# retries = 3             # 判定 unhealthy 前的连续失败次数，必须大于 0。

# 可选 Docker 日志配置。driver 必填；options 由所选驱动解释。
# [services.web.logging]
# driver = "json-file"
# options = { max-size = "10m", max-file = "3" }

# 可选 Traefik 紧凑路由。启用后服务自动加入 nsetup-proxy 网络并使用 HTTPS 入口。
[services.web.traefik]

# 一个路由可匹配多个主机名；短名称会拼接 nsetup 的全局 domain。
hosts = ["media"]

# 可选路径前缀，必须以 / 开头；省略时匹配整个主机。
# path_prefix = "/api"

# 可选内置中间件，按顺序执行：authelia、gzip、forwarded-headers、internal-only、tls。
# middlewares = ["authelia", "gzip"]

# Traefik 到容器的协议：http（默认）| https | h2c。
# protocol = "http"

# 是否启用负载均衡粘性 Cookie，默认 false。
# sticky_cookie = false

# 是否把原始 Host 请求头转发给后端；省略时沿用 Traefik 默认行为。
# pass_host_header = true

# 路由优先级。省略时由 Traefik 根据规则自动计算。
# priority = 100

# 需要不同端口、路径或协议的多条路由时，使用具名 routes 表。
# route.port 省略时回退到 services.web.port；route.middlewares 省略时继承上面的 middlewares。
# 通常在紧凑 hosts 与详细 routes 之间选择一种表达方式。
# api 是稳定路由名，并会进入 Traefik router/service 名称；不要使用无名称数组。
# [services.web.traefik.routes.api]
# hosts = ["media-api"]
# path_prefix = "/v1"
# port = 9090
# middlewares = ["authelia"]
# protocol = "h2c"
# sticky_cookie = true
# pass_host_header = false
# priority = 200

# 可选 Authelia OIDC client 由当前应用拥有；client_id 来自具名表键。
# 先生成客户端 ID 和客户端密钥；Random Password 配置到应用，Digest 写入 TOML：
# docker run --rm --pull=never authelia/authelia:4.39.20 authelia crypto rand --length 72 --charset rfc3986
# docker run --rm --pull=never authelia/authelia:4.39.20 authelia crypto hash generate pbkdf2 --variant sha512 --random --random.length 72 --random.charset rfc3986
# [authelia.oidc_clients.my-app]
# client_name = "My Application"
# client_secret_hash = '$pbkdf2-sha512$replace-with-generated-digest'
# authorization_policy = "two_factor"
# redirect_uris = ["https://app.example.com/oauth/callback"]
# scopes = ["openid", "profile", "email", "groups"]
# grant_types = ["authorization_code"]
# require_pkce = false
# token_endpoint_auth_method = "client_secret_basic" # 或 client_secret_post
# SPA/CLI 等公共客户端应省略 client_secret_hash，并设置 public = true、
# require_pkce = true、token_endpoint_auth_method = "none"。

# 多服务项目继续增加 [services.<名称>]；每个服务至少需要 image 与 version。
# [services.worker]
# image = "ghcr.io/example/media-worker"
# version = "1.0"
"#;

/// CLI 输出的带注释 Authelia 基础设施模板。
pub(super) const AUTHELIA_SKELETON: &str = r#"# Authelia 基础认证设施模板
# 先用以下命令交互生成 password_hash：
# docker run --rm --pull=never -it authelia/authelia:4.39.20 authelia crypto hash generate argon2
# 三个密钥分别运行一次：openssl rand -hex 32
# storage_encryption_key 在数据库首次初始化后必须保持不变；轮换时先用旧密钥执行
# authelia storage encryption change-key，不能直接在 TOML 中替换。
# 重点：[users.admin] 中的 admin 是登录用户名；修改登录名要修改表名，
# 不是只修改 display_name 或 email。应用变更后运行 nsetup restart authelia。
# 可选 OIDC provider 需要额外的 HMAC 与 RSA 私钥：
# openssl rand -hex 64
# openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:4096 -out oidc-rs256.pem
# OIDC 客户端不在此文件声明；请在对应应用中使用 [authelia.oidc_clients.<client_id>]。
format = 1
template = "authelia"
host = "auth"
version = "4.39.20"
default_redirection_url = "https://example.com"
default_policy = "one_factor"
jwt_secret = "replace-with-at-least-32-random-characters"
session_secret = "replace-with-at-least-32-random-characters"
storage_encryption_key = "replace-with-at-least-32-random-characters"

# 取消以下注释以启用 OIDC provider；私钥必须完整粘贴。
# [oidc]
# hmac_secret = "replace-with-at-least-64-random-characters"
# jwk_private_key = """
# -----BEGIN PRIVATE KEY-----
# replace-with-generated-rsa-private-key
# -----END PRIVATE KEY-----
# """

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
