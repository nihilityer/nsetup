# nsetup

`nsetup` 是 Nihility 项目的机器守护进程管理 CLI 和发行版可执行文件。后台 daemon
持有 Docker 及系统数据目录权限，CLI 默认通过本机 Unix domain socket 上的 gRPC API
调用它，无需 `sudo`、无需复制 root token，也不开放网络端口。

## Linux 目录布局

两种安装方式均遵循 FHS 和 systemd 的目录约定：

| 内容                    | 路径                                   | 管理者            |
|-----------------------|--------------------------------------|----------------|
| 单文件安装的二进制             | `/usr/local/bin/nsetup`              | `nsetup init`  |
| 单文件安装的 systemd unit   | `/etc/systemd/system/nsetup.service` | `nsetup init`  |
| Debian 包二进制           | `/usr/bin/nsetup`                    | dpkg           |
| Debian 包 systemd unit | `/lib/systemd/system/nsetup.service` | dpkg           |
| 配置                    | `/etc/nsetup/config.toml`            | 管理员；升级时保留      |
| Compose 项目            | `/var/lib/nsetup/stacks`             | daemon         |
| 容器数据默认根目录             | `/var/lib/nsetup/data`               | daemon         |
| 本机 gRPC socket        | `/run/nsetup/nsetup.sock`            | systemd/daemon |
| 远程 TCP token          | `/etc/nsetup/auth.token`             | daemon         |

运行目录随重启清理，配置和持久数据保存在系统目录，不写入 `~/.nsetup`。

## 单文件初始化

下载或构建一个 `nsetup` 可执行文件后，以 root 身份初始化：

```bash
chmod +x ./nsetup
sudo ./nsetup init \
  --domain example.com \
  --stacks-root /mnt/persistent/nsetup/stacks
sudo usermod -aG nihility "$USER"
```

该命令会把当前可执行文件安装到 `/usr/local/bin/nsetup`，创建 `nihility` 系统组、
配置与状态目录，写入配置和 systemd unit，并立即启用服务。`--domain` 会成为基础设施
及应用的默认主域名；`--stacks-root` 可将 Compose 项目放到不会随 `/var` 清理的持久化
挂载点。目标文件已存在时默认停止；确认替换单文件安装及配置时使用
`sudo ./nsetup init --force`，未重复指定的配置值会保留。

单文件初始化会拒绝与 Debian/RPM 风格的安装共存。使用包管理器安装后，升级和卸载
也必须继续通过包管理器完成。

## 安装 Debian 包

发布产物中的 `.deb` 会创建 `nihility` 系统组、安装并启动 Nihility 机器守护进程：

```bash
sudo apt install ./nsetup_0.1.0-1_amd64.deb
sudo usermod -aG nihility "$USER"
```

重新登录以刷新组成员关系，然后直接使用 CLI：

```bash
nsetup status
nsetup list
nsetup deploy assistant-api --compose ./compose.yaml --env-file ./.env --start
nsetup restart assistant-api
nsetup logs assistant-api --tail 300
nsetup logs assistant-api -f
nsetup remove assistant-api --force
```

`nsetup service status|start|stop|restart` 用于控制已安装的 unit。通过 Debian 包安装
后，安装、升级和卸载必须继续使用 `apt`/`dpkg`。

## 初始化基础设施

`nsetup init` 安装机器守护进程；`nsetup infra init` 则生成由守护进程管理的
Traefik Compose 项目。Cloudflare 令牌通过文件读取，避免出现在 shell 历史中：

```bash
install -m 600 /dev/null ./cloudflare.token

nsetup infra init \
  --acme-email admin@example.com \
  --cloudflare-token-file ./cloudflare.token \
  --start
```

未指定 `--domain` 时使用 `/etc/nsetup/config.toml` 中的 `home.domain`；需要临时生成到
其他主域名时仍可显式传入 `--domain`。

需要规避防火墙的标准端口限制时，可以修改宿主机入口端口。容器内仍监听 80/443，
HTTPS 重定向和 HTTP/3 广播端口会自动同步：

```bash
nsetup infra init \
  --domain example.com \
  --acme-email admin@example.com \
  --cloudflare-token-file ./cloudflare.token \
  --http-port 8080 \
  --https-port 8443 \
  --force \
  --start
```

该命令生成 `traefik` 项目、独立的 Docker 网络以及 Traefik 文件中间件。已有项目
默认不会覆盖；重新生成时需要显式添加 `--force`。

默认使用 Traefik `v3.7.8`，关闭匿名使用统计、版本检查和访问日志，并启用 HTTP/3、
Cloudflare DNS-01、TLS 1.2 最低版本、容器健康检查及日志轮转。

## 添加应用

常规单服务应用可以直接从镜像生成。有限选项使用枚举，当前中间件包括
`gzip`、`forwarded-headers` 和 `internal-only`：

所有容器镜像都必须指定明确版本；`latest` 和省略标签的镜像引用会被拒绝。

```bash
nsetup app add whoami \
  --image traefik/whoami \
  --version v1.11 \
  --container-port 80 \
  --host whoami \
  --middleware gzip \
  --start
```

端口、卷、环境变量、网络、自定义 Docker 标签和命名卷可以按需声明：

```bash
nsetup app add api \
  --image ghcr.io/example/api \
  --version 1.0 \
  --container-port 8080 \
  --host api \
  --publish 12780:8080 \
  --volume /var/lib/example:/var/lib/example \
  --env LOG_LEVEL=info \
  --start
```

同一个容器暴露多个 HTTP 服务时，使用 `--route HOST:PORT` 为每个域名指定不同的
容器端口；原有 `--host` 仍使用 `--container-port` 指定的统一端口。`HOST` 可以是
`whoami`、`s3` 这样的短子域名，daemon 会拼接配置的主域名；传入完整域名时保持不变。

静态站点会递归上传普通文件到 daemon，拒绝符号链接、路径穿越、超过 10000 个文件
或总计超过 64 MiB 的输入：

```bash
nsetup app add-static docs \
  --source ./dist \
  --host docs \
  --middleware gzip \
  --start
```

需要多个服务的应用先用普通 `app add` 创建项目，再通过 `--join` 追加服务；
后续用 `app edit --service` 分别修改、用 `nsetup upgrade` 分别升级。也可以
直接使用完整 Compose 文件创建或修改，不受单服务生成器限制：

```bash
nsetup app add netbird --join --service netbird-server --image ... --version ...
nsetup deploy media --compose ./compose.yaml --env-file ./.env --start
nsetup app edit media --compose ./compose.yaml --env-file ./.env --start
```

## 参数化修改已有应用

`app edit` 与 `app add` 使用同一套参数；不指定 `--compose` 时，daemon 会解析
项目现有的 Compose 文件，只修改传入的参数，其余内容保持不变。多服务项目用
`--service` 选择要修改的服务：

```bash
# 升级镜像版本
nsetup app edit whoami --version v1.12

# 合并环境变量并替换发布端口
nsetup app edit api --service api --env LOG_LEVEL=debug --publish 12781:8081

# 重建路由（--host/--route/--path-prefix 会整体重建该服务的 Traefik 路由标签）
nsetup app edit netbird --host netbird --container-port 8080

# 更新健康检查或移除健康检查
nsetup app edit api --healthcheck-cmd "curl -fsS http://localhost:8080/health" \
  --healthcheck-interval 30s --healthcheck-timeout 3s --healthcheck-retries 5
nsetup app edit api --no-healthcheck
```

修改语义：`--env` 合并进现有环境变量（同名覆盖）；`--publish`/`--publish-udp`
整体替换端口映射；`--volume`/`--named-volume`/`--label` 追加（同名标签覆盖）；
`--host`/`--route`/`--path-prefix` 重建路由标签；镜像只传 `--version` 时保留
当前仓库，只传 `--image` 时保留当前标签。

向已有项目追加服务使用 `app add --join`（项目名不变，`--service` 命名新服务，
`--force` 可替换同名服务）。追加服务的环境变量写入该服务自身的 `environment`，
路由序号自动接续，避免多服务之间 Traefik 路由器重名：

```bash
nsetup app add netbird --join \
  --service netbird-server \
  --image netbirdio/netbird-server \
  --version 0.76.3 \
  --start
```

## 高级参数：自定义标签、命名卷与健康检查

单服务生成器自动为每个路由创建 `Host(...)` 路由器，并追加你通过 `--label`
传入的任意 Docker/Traefik 标签。需要自定义路由器（路径前缀、优先级、h2c
后端、中间件等）时直接覆盖或补充同名标签即可；`--label` 在自动生成的路由
标签之后生效。`--command` 每次传入一个命令参数，并允许以 `-` 开头的值
（如 `--config`）；需要多个参数时重复指定。
命名卷通过 `--named-volume NAME:CONTAINER` 声明，会写入 Compose 的
`volumes` 段；健康检查通过 `--healthcheck-cmd` 启用（`CMD-SHELL` 形式），
`--healthcheck-interval`、`--healthcheck-timeout`、`--healthcheck-start-period`
和 `--healthcheck-retries` 可选，默认间隔与超时 30s、重试 3 次：

```bash
nsetup app add api \
  --image ghcr.io/example/api \
  --version 1.0 \
  --container-port 8080 \
  --host api \
  --healthcheck-cmd "curl -fsS http://localhost:8080/health || exit 1" \
  --healthcheck-start-period 10s \
  --start
```

```bash
nsetup app add worker \
  --image ghcr.io/example/worker \
  --version 1.2 \
  --command --config \
  --command /etc/worker/config.yaml \
  --publish-udp 9000:9000 \
  --named-volume worker-data:/var/lib/worker \
  --read-only-volume /etc/nsetup/worker-config.yaml:/etc/worker/config.yaml \
  --label "traefik.http.routers.worker-grpc.rule=Host(\`worker.example.com\`) && (PathPrefix(\`/grpc\`))" \
  --label "traefik.http.routers.worker-grpc.service=worker-h2c" \
  --label "traefik.http.services.worker-h2c.loadbalancer.server.scheme=h2c" \
  --start
```

### 示例：自托管 NetBird

新版 NetBird 自托管部署（`netbirdio/netbird-server` 合并管理、信号、中继与
内嵌 STUN 的单一容器）通过上面的参数即可表达为同一个项目下的两个服务。
`config.yaml` 是服务端统一配置，替代旧版 `management.json` 与 `relay.env`，请参考
[官方配置参考](https://docs.netbird.io/selfhosted/maintenance/configuration-files)
编写后挂载进容器；`dashboard.env` 中的变量改用 `--env` 传给 Dashboard：

```bash
# 1. 编写 config.yaml（含 authSecret、加密密钥等），保存到 /etc/nsetup/
# 2. 创建 netbird 项目并添加管理面板（自动创建 Host(netbird.example.com) 路由）
nsetup app add netbird \
  --image netbirdio/dashboard \
  --version v2.90.10 \
  --container-port 80 \
  --host netbird \
  --env NETBIRD_MGMT_API_ENDPOINT=https://netbird.example.com \
  --env NETBIRD_MGMT_GRPC_API_ENDPOINT=https://netbird.example.com \
  --env AUTH_AUDIENCE=netbird-dashboard \
  --env AUTH_CLIENT_ID=netbird-dashboard \
  --env AUTH_AUTHORITY=https://netbird.example.com/oauth2 \
  --env "AUTH_SUPPORTED_SCOPES=openid profile email groups" \
  --env AUTH_REDIRECT_URI=/nb-auth \
  --env AUTH_SILENT_REDIRECT_URI=/nb-silent-auth \
  --label "traefik.http.routers.netbird-0.priority=1" \
  --start

# 3. 向同一项目追加合并服务器（gRPC/后端双路由用 --label 声明）
nsetup app add netbird --join \
  --service netbird-server \
  --image netbirdio/netbird-server \
  --version 0.76.3 \
  --command --config \
  --command /etc/netbird/config.yaml \
  --publish-udp 3478:3478 \
  --named-volume netbird-data:/var/lib/netbird \
  --read-only-volume /etc/nsetup/netbird-config.yaml:/etc/netbird/config.yaml \
  --label "traefik.http.routers.netbird-grpc.rule=Host(\`netbird.example.com\`) && (PathPrefix(\`/signalexchange.SignalExchange/\`) || PathPrefix(\`/management.ManagementService/\`) || PathPrefix(\`/management.ProxyService/\`))" \
  --label "traefik.http.routers.netbird-grpc.entrypoints=https" \
  --label "traefik.http.routers.netbird-grpc.tls=true" \
  --label "traefik.http.routers.netbird-grpc.tls.certresolver=cloudflare" \
  --label "traefik.http.routers.netbird-grpc.service=netbird-server-h2c" \
  --label "traefik.http.routers.netbird-grpc.priority=100" \
  --label "traefik.http.routers.netbird-backend.rule=Host(\`netbird.example.com\`) && (PathPrefix(\`/relay\`) || PathPrefix(\`/ws-proxy/\`) || PathPrefix(\`/api\`) || PathPrefix(\`/oauth2\`))" \
  --label "traefik.http.routers.netbird-backend.entrypoints=https" \
  --label "traefik.http.routers.netbird-backend.tls=true" \
  --label "traefik.http.routers.netbird-backend.tls.certresolver=cloudflare" \
  --label "traefik.http.routers.netbird-backend.service=netbird-server" \
  --label "traefik.http.routers.netbird-backend.priority=100" \
  --label "traefik.http.services.netbird-server.loadbalancer.server.port=80" \
  --label "traefik.http.services.netbird-server-h2c.loadbalancer.server.port=80" \
  --label "traefik.http.services.netbird-server-h2c.loadbalancer.server.scheme=h2c" \
  --start

# 4. 确认 3478/udp 已对公网开放（STUN 无法通过 HTTP 反向代理转发）
```

升级新版时分别升级项目内的两个服务；`config.yaml` 与数据卷会保留：

```bash
nsetup upgrade netbird --service netbird-server --version 0.77.0
nsetup upgrade netbird --service app --version v2.91.0
```

升级时分别指定应用、服务和版本。单服务应用可以省略 `--service`；多服务应用必须指定，
命令会修改保存的 Compose 镜像标签、拉取新镜像并重新创建目标服务：

```bash
nsetup upgrade whoami --version v1.12
nsetup upgrade media --service api --version 2.4.0
```

## 配置

包提供的默认配置为：

```toml
[paths]
stacks_root = "/var/lib/nsetup/stacks"
data_root = "/var/lib/nsetup/data"

[home]
domain = "example.com"

[grpc]
listen = "unix:///run/nsetup/nsetup.sock"
```

修改 `/etc/nsetup/config.toml` 后执行：

```bash
sudo systemctl restart nsetup
```

`stacks_root` 和 `data_root` 必须是不同的绝对路径。若系统会清理 `/var`，应将需要
保留的路径设置到持久化磁盘或挂载点；daemon 启动及 `nsetup status` 会显示实际使用的
Compose 项目目录和主域名。

## gRPC 与远程开发

协议位于 [`proto/nsetup.proto`](proto/nsetup.proto)。service、RPC、
message 和 field 都有中文注释，测试会逐条确认这些注释进入 Prost 生成的 `.rs`
文档。接口覆盖健康检查、查询、部署、删除、生命周期、构建和日志。

本机 Unix socket 依靠 `root:nihility` 和 `0660` 权限控制访问。成员能够通过
服务控制 Docker，等同于高权限管理能力，只应把可信管理员加入该组。

远程开发需要显式把 `grpc.listen` 改成 TCP 地址，例如 `127.0.0.1:50051`。daemon
会创建 `/etc/nsetup/auth.token`，客户端必须显式指定端点；跨主机连接还应
置于 VPN 或 TLS HTTP/2 代理之后：

```bash
nsetup rpc \
  --endpoint http://192.168.7.107:50051 \
  --token-file ./server.auth.token \
  health
```

## 构建与打包

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
cargo install cargo-deb --locked
cargo deb
```

Debian 包输出到 `target/debian/`。打包输入位于 `packaging/`，包维护脚本负责创建
系统组、状态目录以及启用服务；配置文件被声明为 conffile，升级不会静默覆盖修改。
