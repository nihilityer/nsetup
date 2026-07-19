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
sudo ./nsetup init
sudo usermod -aG nihility "$USER"
```

该命令会把当前可执行文件安装到 `/usr/local/bin/nsetup`，创建 `nihility` 系统组、
配置与状态目录，写入内嵌的默认配置和 systemd unit，并立即启用服务。目标文件已存在
时默认停止；确认替换单文件安装及配置时使用 `sudo ./nsetup init --force`。

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
  --domain example.com \
  --acme-email admin@example.com \
  --cloudflare-token-file ./cloudflare.token \
  --start
```

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
  --host whoami.example.com \
  --middleware gzip \
  --start
```

端口、卷、环境变量和网络可以按需声明：

```bash
nsetup app add api \
  --image ghcr.io/example/api \
  --version 1.0 \
  --container-port 8080 \
  --host api.example.com \
  --publish 12780:8080 \
  --volume /var/lib/example:/var/lib/example \
  --env LOG_LEVEL=info \
  --start
```

同一个容器暴露多个 HTTP 服务时，使用 `--route HOST:PORT` 为每个域名指定不同的
容器端口；原有 `--host` 仍使用 `--container-port` 指定的统一端口。

静态站点会递归上传普通文件到 daemon，拒绝符号链接、路径穿越、超过 10000 个文件
或总计超过 64 MiB 的输入：

```bash
nsetup app add-static docs \
  --source ./dist \
  --host docs.example.com \
  --middleware gzip \
  --start
```

复杂的多服务项目使用完整 Compose 文件创建或修改，不受单服务生成器限制：

```bash
nsetup deploy media --compose ./compose.yaml --env-file ./.env --start
nsetup app edit media --compose ./compose.yaml --env-file ./.env --start
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
