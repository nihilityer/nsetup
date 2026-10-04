# Authelia 认证

`nsetup` 的 Authelia 模板提供文件用户库、SQLite、TOTP、Traefik ForwardAuth 和可选
OIDC provider。运行状态保存在第一个 `data_root` 的 `authelia/` 下，删除项目不会
删除这些状态。

生成的 Compose 把项目里的 `config/` **可写**挂到 `/config`、`secrets/` 只读挂到
`/secrets`、`data_root` 挂到 `/data`。`/config` 必须可写：官方镜像的 entrypoint 在
`PUID`/`PGID` 为 0（镜像默认值）时执行 `chown -R 0:0 /config`，只读挂载会让它每次
启动都往容器日志写 `chown: /config/...: Read-only file system`，淹没真实错误。需要
以非 root 运行 Authelia 时，同时设置 `PUID`/`PGID` 与 `user`，并确认 `secrets/` 中的
只读密钥对该 UID 可读。

OIDC 客户端片段（`config/oidc-clients/<项目>.yml`）在更新时**原地重写**，不替换
inode，因此容器 entrypoint 改过的属主不会被下一次 `up` 悄悄改回；写入权限只提不降。

## 初始化

先生成模板和密码哈希，再为三个基础密钥分别生成随机值：

```bash
nsetup template authelia > authelia.toml
docker run --rm --pull=never -it authelia/authelia:4.39.20 \
  authelia crypto hash generate argon2
openssl rand -hex 32  # 分别为三个密钥运行一次
```

将密码摘要写入 `password_hash`，三次 `openssl` 输出分别写入 `jwt_secret`、
`session_secret` 和 `storage_encryption_key`。配置包含密钥，应保持 `0600`：

```bash
chmod 600 authelia.toml
nsetup up -f authelia.toml --start
```

模板明确启用 TOTP 并将其设为唯一的二次验证方式，同时禁用 WebAuthn；模板不配置
Duo。普通 ForwardAuth 路由是否要求二次验证由 `default_policy` 决定，OIDC 客户端
则使用各自的 `authorization_policy`。

## 用户名与持久化密钥

`[users.<名称>]` 的表名就是登录用户名。默认的 `[users.admin]` 表示用户名为
`admin`；`display_name` 只是显示名称，`email` 也不是默认登录别名。修改用户名必须
修改表名，例如改为 `[users.nihilityer]`。

文件用户库不自动监视变更，模板也关闭了网页改密和忘记密码流程。修改用户名或密码
摘要后需要重新应用并重启：

```bash
nsetup up -f authelia.toml --force
nsetup restart authelia
```

`storage_encryption_key` 加密 SQLite 中的敏感字段。数据库初始化后必须保留原值，普通
`up --force` 不能重新生成它。需要轮换时，先停止 Authelia，使用旧密钥执行
`authelia storage encryption change-key`，再更新 TOML。丢失旧密钥后无法解密已有的
TOTP、WebAuthn 和 OIDC 状态。

## Traefik ForwardAuth

Traefik 模板会生成 `authelia@file` 中间件。普通应用在路由中引用它即可启用统一认证：

```toml
[services.web.traefik]
hosts = ["admin"]
middlewares = ["authelia", "tls"]
```

认证门户自身不能引用该中间件，否则会形成重定向循环。

Traefik 模板的 `dashboard_authelia = true` 会在固定的 `internal-only@file` 之后追加
`authelia@file`，使控制台同时受内网来源限制与 Authelia 登录保护。关闭或省略时只
保留内网限制；启用前应确保 Authelia 已启动。

ForwardAuth 与应用原生 OIDC 登录是两种独立集成方式。应用只使用自身的 OIDC 登录
时，通常无需再为同一路由增加 `authelia` 中间件。

## OIDC provider

OIDC provider 是 Authelia 的全局能力。在 `authelia.toml` 中配置 HMAC 和 RS256
私钥：

```bash
openssl rand -hex 64
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:4096 -out oidc-rs256.pem
```

```toml
[oidc]
hmac_secret = "...openssl rand 的输出..."
jwk_private_key = """
-----BEGIN PRIVATE KEY-----
...oidc-rs256.pem 的完整内容...
-----END PRIVATE KEY-----
"""
```

## OIDC claims policy

Authelia 默认只把协议必需的 claim 放进 ID Token，其余 claim 按规范通过 UserInfo
端点交付。部分客户端（典型是 Grafana）不会主动请求 UserInfo，结果是能登录、却没有
邮箱、显示名或 groups。这类客户端需要在 provider 侧用 claims policy 把 claim 直接
写进 ID Token：

```toml
[oidc.claims_policies.default]
id_token = ["groups", "email", "email_verified", "preferred_username", "name"]
```

policy 名称由引用它的客户端 `claims_policy = "default"` 指定；每个 policy 至少声明
`id_token` 或 `access_token` 之一。引用了未声明的 policy 时 Authelia 启动即报错，
不会静默降级。

## OIDC 客户端

每个客户端由实际使用它的应用 TOML 管理，具名表键就是 `client_id`。生成客户端明文
密钥及其 PBKDF2 摘要：

```bash
docker run --rm --pull=never authelia/authelia:4.39.20 \
  authelia crypto hash generate pbkdf2 --variant sha512 --random \
  --random.length 72 --random.charset rfc3986
```

将命令输出的 `Random Password` 配置到客户端应用，将 `Digest` 写入对应应用 TOML：

```toml
[authelia.oidc_clients.gitea]
client_name = "Gitea"
client_secret_hash = '$pbkdf2-sha512$...'
authorization_policy = "two_factor"
redirect_uris = ["https://git.example.com/user/oauth2/authelia/callback"]
scopes = ["openid", "profile", "email", "groups"]
grant_types = ["authorization_code", "refresh_token"]
require_pkce = false
token_endpoint_auth_method = "client_secret_basic"
claims_policy = "default"   # 可选；引用 Authelia 项目的 claims policy
```

回调 URI 区分大小写，必须精确一致。公共 SPA 或 CLI 客户端省略
`client_secret_hash`，并设置 `public = true`、`require_pkce = true` 和
`token_endpoint_auth_method = "none"`。

`pkce_challenge_method` 只在 `require_pkce = true` 时输出。Authelia 把该字段视为
对该客户端强制 PKCE，即使同一片段里写着 `require_pkce = false` 也依然强制，因此
不发送 `code_challenge` 的客户端（例如 Gitea）会收到
`registered in a way that enforces PKCE` 而被拒绝。discovery 地址为：

```text
https://<认证门户域名>/.well-known/openid-configuration
```

应用执行 `nsetup up -f <应用>.toml --force` 后，客户端配置会同步到 Authelia 项目；
片段确实发生变化时会自动重启 authelia 使其生效（可用 `--restart-dependents` 显式
要求），删除声明或整个应用时对应片段也会删除。所有应用的 `client_id` 必须全局唯一。

## 自身遥测

Authelia 默认不暴露指标也不导出 trace；需要时在 authelia.toml 中声明：

```toml
[telemetry]
metrics_address = "tcp://0.0.0.0:9959"
metrics_path = "/metrics"
tracing_address = "udp://otel-collector:4318"
tracing_sample_rate = 0.5
```

`metrics_address` 与 `tracing_address` 使用 `host:port` 形式，允许
`tcp://`、`udp://` 传输前缀。

导出的 Authelia TOML 含密钥，使用 `-o` 写出的文件权限为 `0600`。Traefik 的
`acme.json` 与 Authelia 的 SQLite 状态会在 `up --force` 时保留。
