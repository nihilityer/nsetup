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
nsetup template traefik > traefik.toml
nsetup template static > static.toml
```

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

应用配置并启动：

```bash
nsetup up -f app.toml --start
```

镜像必须拆分为 `image` 与明确的 `version`，省略版本或使用 `latest` 会被拒绝。
短主机名会拼接全局主域名；例如 `media` 生成 `media.example.com`。

Traefik 与静态站点使用相同入口：

```bash
chmod 600 traefik.toml       # 文件中含 Cloudflare token
nsetup up -f traefik.toml --start
nsetup up -f static.toml --assets ./dist --start
```

整体更新已有项目需要 `--force`。静态站点再次上传 `--assets` 时会整体替换站点文件；
Traefik 的 `acme.json` 会在更新时保留。

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
