# nsetup

`nsetup` 是 Nihility 的 Linux 主机与 Docker Compose 管理工具。它以 systemd daemon
持有系统权限，CLI 通过本机 Unix socket 上的 gRPC 接口调用它，因此日常操作无需
`sudo`、无需复制 root token，也不会默认开放网络端口。

## 安装

构建或下载单个静态链接二进制后初始化：

```bash
chmod +x ./nsetup
sudo ./nsetup init \
  --domain example.com \
  --stacks-root /var/lib/nsetup/stacks \
  --data-root /srv/data
sudo usermod -aG nihility "$USER"
```

重新登录后检查 daemon 与 Docker：

```bash
nsetup status
```

`init --force` 可更新现有安装，未重新指定的配置项保持不变。

## 部署

TOML 是唯一的声明式配置格式。先输出带注释的配置骨架：

```bash
nsetup template app > app.toml
nsetup template authelia > authelia.toml
nsetup template traefik > traefik.toml
nsetup template static > static.toml
```

`app` 骨架逐字段说明当前支持的配置；仅保留常用的最小路由为有效配置，端口发布、
挂载、固定容器名、重启策略、健康检查、日志和高级路由等可选项均以注释示例展示。

普通容器应用支持一个项目内的多个服务：

```toml
format = 1
name = "media"

[services.web]
image = "ghcr.io/example/media"
version = "1.0"
port = 8080
publish = ["12780:8080/tcp"]
volumes = ["/srv/data/media:/data"]
environment = { LOG_LEVEL = "info" }

[services.web.traefik]
hosts = ["media"]
middlewares = ["gzip", "internal-only"]

[services.worker]
image = "ghcr.io/example/worker"
version = "1.2"
```

同一容器暴露多个子路由时使用具名表，让名称、域名和端口保持显式绑定；路由名也会
进入 Traefik router/backend 名称，不依赖声明顺序：

```toml
[services.web.traefik.routes.api]
hosts = ["s3"]
port = 9000

[services.web.traefik.routes.console]
hosts = ["s3-console"]
port = 9001
```

旧的 `[[services.*.traefik.routes]]` 顺序数组不再支持。

应用配置并启动：

```bash
nsetup up -f app.toml --start
```

镜像必须拆分为 `image` 与明确的 `version`，省略版本或使用 `latest` 会被拒绝。
短主机名会拼接全局主域名；例如 `media` 生成 `media.example.com`。

基础设施与静态站点使用相同入口。先部署 Traefik，再部署 Authelia：

```bash
chmod 600 traefik.toml authelia.toml
nsetup up -f traefik.toml --start
nsetup up -f authelia.toml --start
nsetup up -f static.toml --assets ./dist --start
```

### Authelia：基础认证设施

Authelia 模板使用文件用户库、文件型密钥和 SQLite，运行状态持久化到第一个
`data_root` 下的 `authelia/`。先交互生成密码哈希，再替换模板中的占位值：

```bash
docker run --rm --pull=never -it authelia/authelia:4.39.20 \
  authelia crypto hash generate argon2
openssl rand -hex 32   # 三个密钥分别运行一次
```

> **重点：登录用户名由 `[users.<用户名>]` 的表名决定。** 模板中的
> `[users.admin]` 表示登录用户名是 `admin`；`display_name` 只是显示名称，`email`
> 也不是默认登录别名。要改成 `nihilityer`，应把表名改为 `[users.nihilityer]`，
> 不是只修改 `display_name`。

修改用户名或密码哈希后重新应用并重启；文件用户库关闭了自动监视，当前模板也关闭
网页改密与“忘记密码”流程：

```bash
nsetup up -f authelia.toml --force
nsetup restart authelia
```

需要认证的应用只需在路由中加入 `authelia`；认证门户自身不会套用认证中间件：

```toml
[services.web.traefik]
hosts = ["admin"]
middlewares = ["authelia", "tls"]
```

整体更新已有项目需要 `--force`。静态站点再次上传 `--assets` 时会整体替换站点文件；
Traefik 的 `acme.json` 与 Authelia 的 SQLite 状态会在更新时保留。导出的基础设施
TOML 含密钥，`-o` 创建的文件权限为 `0600`。

## 导入、编辑与导出

可导入架构支持字段子集内的 Compose 文件：

```bash
nsetup import media -f compose.yaml --env-file .env --start
nsetup edit media --service web --version 1.1 --start
nsetup export media -o media.toml
```

导入遇到未知 Compose 字段、命名卷、相对 bind mount 或未固定版本镜像时直接报错。
`export` 总是从当前 `compose.yaml + .env` 反解 TOML，不依赖另存的声明副本；输出文件
已存在时不会覆盖。复杂编辑参数请查看 `nsetup edit --help`。

## 日常操作

```bash
nsetup list
nsetup show media
nsetup start media
nsetup stop media
nsetup restart media
nsetup pull media
nsetup build media
nsetup logs media --tail 300
nsetup logs media --follow
nsetup rm media
```

`pull` 在交互式终端中原地刷新单行进度条；重定向或管道中仍输出稳定的制表符分隔事件。
`up`、`import`、`edit`、`start`、`stop`、`restart`、`build`、`rm` 会立即显示排队和
执行阶段，最终结果仍单独写入 stdout。`logs --follow` 不占用变更锁，退出客户端后
daemon 会终止对应的 Compose 日志进程。
删除项目不会删除 bind mount 指向的数据。`rm --force` 仅跳过交互确认。

## 配置与安全

daemon 配置位于 `/etc/nsetup/config.toml`，使用扁平键：

```toml
domain = "example.com"
stacks_root = "/var/lib/nsetup/stacks"
data_roots = ["/srv/data"]
listen = "unix:///run/nsetup/nsetup.sock"
docker_socket = "/var/run/docker.sock"
```

bind mount 源路径只能位于 `data_roots` 或 `stacks_root`，Docker socket 只允许精确
匹配。所有路径会先归一化并解析已有符号链接。`nihility` 组能够间接控制 Docker，
应只授予可信管理员。

需要远程管理时，将 daemon 的 `listen` 设为 TCP 地址，并为任意命令同时指定：

```bash
nsetup --endpoint http://192.168.1.10:50051 \
  --token-file ./nsetup.auth.token status
```

TCP token 存放于 `/etc/nsetup/auth.token`（`0600`）。跨主机流量还应置于 VPN 或
TLS HTTP/2 代理之后。

## 开发

详细设计与约束见 [架构文档](docs/ARCHITECTURE.md)，gRPC 接口见
[proto/nsetup.proto](proto/nsetup.proto)。提交前运行：

```bash
cargo fmt --check
cargo test --quiet
cargo clippy --all-targets --quiet -- -D warnings
git diff --check
```
