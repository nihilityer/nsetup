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

# bind mount。宿主机路径必须是 data_roots/stacks_root 或本项目目录内的绝对路径；
# 仅支持可选 :ro。日常部署优先用 nsetup up -f app.toml --files <路径>。
# volumes = ["/var/lib/nsetup/data/media:/data"]

# 容器运行用户。需要以固定用户（尤其 root）读取宿主机文件时使用 UID[:GID]。
# user = "0:0"

# 除主用户组外额外加入的补充组；可用组名或数字 GID。
# group_add = ["988"]

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

# 可选健康检查。不写本表时不会覆盖镜像自带的 HEALTHCHECK。
# command 是 shell 字符串时为 CMD-SHELL；写成数组时是 CMD（argv），
# 没有 shell 的镜像（如 tuwunel）只能用 argv 形式。
# [services.web.healthcheck]
# command = "wget -qO- http://127.0.0.1:8080/health || exit 1"
# command = ["/usr/bin/curl", "-f", "http://127.0.0.1:8080/health"]
# interval = "30s"       # 两次检查的间隔。
# timeout = "3s"         # 单次检查的超时时间。
# start_period = "20s"   # 容器启动后的失败宽限期。
# retries = 3             # 判定 unhealthy 前的连续失败次数，必须大于 0。

# 可选启动钩子。在 daemon 上以本项目目录为工作目录、通过 sh -c 顺序执行；
# pre_start 在 compose up 之前、post_start 之后执行，post_start 仅在 --start 时运行。
# [services.web.hooks]
# pre_start = ["install -d -m 0755 data", "docker run --rm -v $PWD/data:/d alpine chown -R 1000 /d"]
# post_start = ["docker exec web app init"]

# 可选 Docker 日志配置。driver 必填；options 由所选驱动解释。
# [services.web.logging]
# driver = "json-file"
# options = { max-size = "10m", max-file = "3" }

# 可选 Traefik 紧凑路由。启用后服务同时加入 nsetup-proxy 与项目默认网络：
# 前者供 Traefik 回源，后者保证仍能访问同项目其它服务；入口固定为 HTTPS。
[services.web.traefik]

# 一个路由可匹配多个主机名；短名称会拼接 nsetup 的全局 domain。
hosts = ["media"]

# 可选路径前缀，必须以 / 开头；省略时匹配整个主机。
# path_prefix = "/api"

# 中间件按顺序执行。内置名称：authelia、gzip、forwarded-headers、internal-only、tls；
# 其它名称引用 traefik.toml 的 [middlewares.<名称>] 或 files/ 里的自定义中间件。
# middlewares = ["authelia", "gzip"]

# Traefik 到容器的协议：http（默认）| https | h2c。
# protocol = "http"

# 路由监听的 entrypoint；省略时为 https。需要 http 明文访问时显式写 http。
# entrypoint = "https"

# 是否启用负载均衡粘性 Cookie，默认 false。
# sticky_cookie = false

# 是否把原始 Host 请求头转发给后端；省略时沿用 Traefik 默认行为。
# pass_host_header = true

# 路由优先级。省略时由 Traefik 根据规则自动计算。
# priority = 100

# 同一 host 下可以按 path_prefix 或协议拆分多条路由，nsetup 只拒绝
# host + path_prefix + entrypoint + protocol 完全相同的组合。
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
# entrypoint = "https"
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
# 引用 authelia.toml 中 [oidc.claims_policies.<名称>] 声明的策略；Grafana 等不请求
# UserInfo 的应用必须配置此项，否则 ID Token 里没有 groups 等 claim。
# claims_policy = "my-app"
# SPA/CLI 等公共客户端应省略 client_secret_hash，并设置 public = true、
# require_pkce = true、token_endpoint_auth_method = "none"。

# 需要把宿主机文件交给容器读取时，用 nsetup up -f app.toml --files ./config 上传：
# 目录内容会铺到项目目录的 files/ 并以只读方式挂到 /opt/nsetup/files
# （可用 --files-into 改挂载点），无需 sudo 或一次性特权容器；上传目录为 0755、
# 文件为 0644，容器内非 root 进程可以直接读取。
# volumes 的挂载源可以写相对项目目录的路径，例如 files/config.yaml，这样更换
# stacks_root 后 TOML 无需修改。
# 只改了 files/ 内容、不需要重建容器时用 nsetup up -f app.toml --files ./config
# --files-only：它只同步资源，不写 compose.yaml/.env，也不执行钩子。

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
# 可选 claims_policies 用于把 ID Token 之外的 claim 直接写进 ID Token；Grafana 等
# 不请求 UserInfo 的应用必须为客户端配置 claims_policy 才能拿到 email/name/groups。
format = 1
template = "authelia"
# 项目名固定为 authelia；这里显式写出只为与 app 模板保持一致，也可以省略。
name = "authelia"
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
# 可选具名 claims policy，键名由引用它的客户端 claims_policy 指定。
# 每个 policy 至少声明 id_token 或 access_token 之一，claim 名区分大小写。
# [oidc.claims_policies.default]
# id_token = ["groups", "email", "email_verified", "preferred_username", "name"]
#
# 客户端只在 require_pkce = true 时才输出 pkce_challenge_method: S256；
# Authelia 把该字段视为对该客户端强制 PKCE，会拒绝不发送 code_challenge 的客户端。

# 可选自身遥测，默认不暴露指标也不导出 trace。
# [telemetry]
# metrics_address = "tcp://0.0.0.0:9959"
# metrics_path = "/metrics"
# tracing_address = "udp://otel-collector:4318"
# tracing_sample_rate = 0.5

# 生成的 Compose 把 config/ 以可写方式挂到 /config、secrets/ 只读挂到 /secrets。
# 官方镜像的 entrypoint 会按 PUID/PGID（镜像默认 0:0）执行 chown -R /config，
# 只读挂载会因此持续往容器日志写 `chown: ... Read-only file system`。
# 需要以非 root 运行 Authelia 时，同时设置 PUID/PGID 与 user，并确保 secrets/
# 中的只读密钥对该 UID 可读。

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
# 项目名固定为 traefik；这里显式写出只为与 app 模板保持一致，也可以省略。
name = "traefik"
domain = "example.com"
acme_email = "admin@example.com"
cloudflare_token = "replace-me"
version = "v3.8.0"
http_port = 80
https_port = 443
# 通过 Authelia ForwardAuth 保护 dashboard；仍保留内网来源限制。
# 请先准备 Authelia 配置，再开启此项。
dashboard_authelia = true
# 暴露 Traefik 自身的 Prometheus 指标，默认开启。指标入口只在内网监听，
# 同时以 127.0.0.1:<metrics_port> 绑定到宿主机回环地址，供本机采集器抓取。
# metrics = true
# metrics_port = 8081

# 追加自定义中间件，供应用路由的 middlewares 引用。
# [middlewares.replace-path]
# kind = "replacePath"
# args = { path = "/status" }
#
# [middlewares.strip-api]
# kind = "stripPrefix"
# args = { prefixes = ["/api"], forceSlash = true }

# 生成的内置中间件写入 config/dynamic/nsetup.yml；同目录的 custom.yml 由用户拥有，
# 不会被 nsetup up 覆盖，可以在其中追加路由与中间件。
"#;

/// CLI 输出的带注释静态站点模板。
pub(super) const STATIC_SKELETON: &str = r#"# 静态 Nginx 站点；使用 --assets 上传站点文件
#
# 上传目录整体位于项目目录的 site/，挂载到 /usr/share/nginx/html。容器同时以
# 只读方式获得整个项目目录（/opt/nsetup），放入 nginx.conf 即可改写服务方式；
# 默认站点配置由 config/nginx/default.conf 提供。
# --assets-mode merge（默认）只覆盖同名文件，需要删除已下线文件时用 replace。
# 上传后的站点目录为 0755、文件为 0644（a+rX），因此 nginx worker（UID 101）等
# 非 root 进程可以直接读取，不再需要 chmod 兜底脚本；需要收紧权限时用
# nsetup up --assets-perms private 上传（目录 0750、文件 0640）。
format = 1
template = "static"
name = "docs"
host = "docs"
version = "1.27"
middlewares = ["gzip"]

# 容器运行用户与补充组。默认沿用镜像的 root 主进程 + nginx 用户 worker；需要让
# 站点目录整体由固定用户读取时可以显式指定 UID[:GID]。
# user = "101:101"
# group_add = ["988"]

# 可选启动钩子。在 daemon 上以项目目录为工作目录、通过 sh -c 执行；pre_start 在
# compose up 之前、post_start 之后，post_start 仅在 --start 时运行。
# 钩子用于站点侧修正属主/权限，例如把上传目录交给容器内的固定 UID：
# [hooks]
# pre_start = ["chmod -R a+rX site"]
# post_start = ["docker exec docs-web-1 nginx -s reload"]
"#;
