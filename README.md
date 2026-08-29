# nsetup

`nsetup` 是 Nihility 的 Linux 主机与 Docker Compose 管理工具。它以 systemd daemon
持有系统权限，CLI 通过本机 Unix socket 上的 gRPC 接口调用它，因此日常操作无需
`sudo`、无需复制 root token，也不会默认开放网络端口。

## 快速开始

下载或构建单个 `nsetup` 可执行文件，然后初始化系统服务：

```bash
chmod +x ./nsetup
sudo ./nsetup init \
  --domain example.com \
  --stacks-root /mnt/persistent/nsetup/stacks
sudo usermod -aG nihility "$USER"
```

重新登录以刷新组权限。初始化会安装二进制、配置文件和 systemd unit，并立即启动
daemon。目标已存在时命令会停止；需要覆盖时使用 `init --force`，未重新指定的配置值
会保留。

检查安装：

```bash
nsetup status
nsetup service status
nsetup list
```

## 初始化反向代理

`infra init` 创建由 daemon 管理的 Traefik 项目。Cloudflare token 从文件读取，避免
进入 shell 历史：

```bash
install -m 600 /dev/null ./cloudflare.token

nsetup infra init \
  --acme-email admin@example.com \
  --cloudflare-token-file ./cloudflare.token \
  --start
```

默认域名来自 `/etc/nsetup/config.toml`。可用 `--domain` 临时覆盖，或用
`--http-port`、`--https-port` 修改宿主机入口端口。重新生成已有配置需要 `--force`。

## 部署应用

镜像必须使用明确版本，省略标签或使用 `latest` 会被拒绝。

推荐使用 nsetup 原生 TOML 配置，无需编写或维护 Compose 文件：

```toml
format = 1
name = "api"
image = "ghcr.io/example/api:1.0"
port = 8080
publish = ["12780:8080/tcp"]
volumes = ["/var/lib/example:/var/lib/example"]
named_volumes = ["api-cache:/var/cache/api"]

[environment]
LOG_LEVEL = "info"

[traefik]
hosts = ["api"]
middlewares = ["gzip"]
protocol = "http"
sticky_cookie = true
pass_host_header = true
priority = 100

[healthcheck]
command = "curl -fsS http://localhost:8080/health"
interval = "30s"
timeout = "3s"
retries = 3
```

读取配置并部署，或导出当前保存的简化配置：

```bash
nsetup app import --config ./api.toml --start
nsetup app export api --output ./api.toml --force
```

`traefik.hosts` 接受完整域名或短子域名；`middlewares` 支持 `gzip`、
`forwarded-headers`、`internal-only`；`protocol` 支持 `http`、`https`、`h2c`。
还可以用 `[[traefik.routes]]` 为不同域名指定独立的 `port` 和 `path_prefix`。
`sticky_cookie`、`pass_host_header` 和 `priority` 会生成对应的常用 Traefik labels，
其余高级场景仍可通过顶层 `labels` 数组补充。

简化配置由 daemon 以 `0600` 权限保存。若应用随后被 `app edit`、`upgrade` 或原始
Compose 部署修改，旧配置会失效；此时导出会拒绝返回可能过期的内容。

也可以直接通过命令参数创建单服务应用：

```bash
nsetup app add whoami \
  --image traefik/whoami \
  --version v1.11 \
  --container-port 80 \
  --host whoami \
  --middleware gzip \
  --start
```

短主机名会自动拼接全局域名，例如 `whoami` 会变为 `whoami.example.com`。端口、卷、
环境变量和健康检查等可按需添加：

```bash
nsetup app add api \
  --image ghcr.io/example/api \
  --version 1.0 \
  --container-port 8080 \
  --host api \
  --publish 12780:8080 \
  --volume /var/lib/example:/var/lib/example \
  --env LOG_LEVEL=info \
  --healthcheck-cmd "curl -fsS http://localhost:8080/health" \
  --start
```

部署静态站点：

```bash
nsetup app add-static docs \
  --source ./dist \
  --host docs \
  --middleware gzip \
  --start
```

部署完整 Compose 项目：

```bash
nsetup deploy media --compose ./compose.yaml --env-file ./.env --start
```

向已有项目追加服务使用 `--join`；修改现有服务使用 `app edit`：

```bash
nsetup app add media --join \
  --service worker \
  --image ghcr.io/example/worker \
  --version 1.2 \
  --start

nsetup app edit media --service worker --version 1.3 --start
```

`app edit` 只修改传入的参数，其余 Compose 内容保持不变。它也可以通过
`--compose` 和 `--env-file` 整体替换项目。完整参数见：

```bash
nsetup app add --help
nsetup app edit --help
```

## 日常运维

```bash
nsetup show media
nsetup start media
nsetup stop media
nsetup restart media
nsetup logs media --tail 300
nsetup logs media -f
nsetup pull media
nsetup build media
nsetup upgrade media --service api --version 2.4.0
nsetup remove media --force
```

单服务项目升级时可省略 `--service`。`remove --purge` 还会删除 Compose 命名卷，
但不会删除宿主机绑定挂载的数据。

## 配置与目录

默认配置位于 `/etc/nsetup/config.toml`：

```toml
[paths]
stacks_root = "/var/lib/nsetup/stacks"
data_root = "/var/lib/nsetup/data"

[home]
domain = "example.com"

[grpc]
listen = "unix:///run/nsetup/nsetup.sock"
```

修改后重启服务：

```bash
sudo systemctl restart nsetup
```

| 内容 | 默认路径 |
| --- | --- |
| 二进制 | `/usr/local/bin/nsetup` |
| systemd unit | `/etc/systemd/system/nsetup.service` |
| 配置 | `/etc/nsetup/config.toml` |
| Compose 项目 | `/var/lib/nsetup/stacks` |
| 容器数据 | `/var/lib/nsetup/data` |
| gRPC socket | `/run/nsetup/nsetup.sock` |
| TCP 认证 token | `/etc/nsetup/auth.token` |

`stacks_root` 与 `data_root` 必须是不同的绝对路径。需要持久化时，应将它们配置到不会
被系统清理的磁盘或挂载点。

## 远程访问与安全

本机 socket 权限为 `root:nihility`、`0660`。该组成员可以通过 daemon 控制 Docker，
应视为高权限管理员。

远程开发时可将 `grpc.listen` 改为 TCP 地址。客户端必须显式提供 endpoint 和 token；
跨主机连接还应放在 VPN 或 TLS HTTP/2 代理之后：

```bash
nsetup rpc \
  --endpoint http://192.168.7.107:50051 \
  --token-file ./server.auth.token \
  health
```

gRPC 协议见 [`proto/nsetup.proto`](proto/nsetup.proto)。

## 开发

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

发布产物是 musl 静态链接的单个 `nsetup` 可执行文件。
