# nsetup 使用与运维

本文说明从安装到日常运维的常用流程。完整命令参数请直接查看
`nsetup <命令> --help`；所有 TOML 字段及示例由 `nsetup template <类型>` 输出。

## 安装

`nsetup` 需要 Linux、systemd 与 Docker Compose。准备静态链接二进制后执行：

```bash
chmod +x ./nsetup
sudo ./nsetup init \
  --domain example.com \
  --stacks-root /var/lib/nsetup/stacks \
  --data-root /srv/data
sudo usermod -aG nihility "$USER"
```

重新登录后运行 `nsetup status`。`init --force` 可替换现有安装；未重新指定的配置项
保持原值。

## 使用 TOML 部署

TOML 是唯一的声明式配置格式。先生成当前版本对应的带注释骨架：

```bash
nsetup template app > app.toml
nsetup template traefik > traefik.toml
nsetup template authelia > authelia.toml
nsetup template static > static.toml
```

### 容器应用

最小的应用配置可以只包含项目、服务、镜像和版本：

```toml
format = 1
name = "media"

[services.web]
image = "ghcr.io/example/media"
version = "1.0"
port = 8080

[services.web.traefik]
hosts = ["media"]
```

镜像仓库与版本必须分开声明；版本不能省略或使用 `latest`。短主机名会拼接 daemon
配置中的 `domain`，例如 `media` 会生成 `media.example.com`。

同一服务需要多个不同端口或域名的路由时，使用具名表明确绑定关系：

```toml
[services.web.traefik.routes.api]
hosts = ["s3"]
port = 9000

[services.web.traefik.routes.console]
hosts = ["s3-console"]
port = 9001
```

路由名会进入 Traefik router/backend 名称。旧的
`[[services.*.traefik.routes]]` 顺序数组不受支持。

应用配置并启动；更新已有项目时增加 `--force`：

```bash
nsetup up -f app.toml --start
nsetup up -f app.toml --force --start
```

### 基础设施与静态站点

通常先部署 Traefik；需要统一认证时再部署 Authelia，然后部署普通应用：

```bash
chmod 600 traefik.toml authelia.toml
nsetup up -f traefik.toml --start
nsetup up -f authelia.toml --start
nsetup up -f app.toml --start
```

Authelia 的用户、TOTP、ForwardAuth、OIDC 和密钥操作见
[Authelia 认证](AUTHELIA.md)。

静态站点的文件通过 `--assets` 上传。再次上传会整体替换已有站点文件：

```bash
nsetup up -f static.toml --assets ./dist --start
```

## 导入、编辑与导出

导入受支持字段子集内的 Compose 项目：

```bash
nsetup import media -f compose.yaml --env-file .env --start
```

未知 Compose 字段、命名卷、相对 bind mount 或未固定版本镜像会被拒绝。局部编辑
单个服务或导出当前状态：

```bash
nsetup edit media --service web --version 1.1 --start
nsetup export media -o media.toml
```

`export` 从当前 `compose.yaml` 与 `.env` 反解 TOML。目标文件已存在时不会覆盖。

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

删除项目会停止容器并移除受管项目目录，但不会删除 bind mount 指向的数据。
`rm --force` 仅跳过交互确认。

查询结果与最终操作结果写入 stdout，诊断和中间进度写入 stderr。`pull` 在终端中显示
单行进度，重定向或管道中输出稳定的制表符分隔事件；`logs --follow` 在客户端退出后
终止对应的 Compose 日志进程。

## daemon 配置与安全边界

daemon 配置位于 `/etc/nsetup/config.toml`：

```toml
domain = "example.com"
stacks_root = "/var/lib/nsetup/stacks"
data_roots = ["/srv/data"]
listen = "unix:///run/nsetup/nsetup.sock"
docker_socket = "/var/run/docker.sock"
```

bind mount 源路径必须是 `data_roots` 或 `stacks_root` 下的绝对路径；Docker socket
只允许精确匹配。路径在使用前会归一化并解析已有符号链接。`nihility` 组成员能够通过
daemon 间接控制 Docker，因此只应授予可信管理员。

远程管理时，将 `listen` 改为 TCP 地址，并在每次调用中同时提供端点与 token：

```bash
nsetup --endpoint http://192.168.1.10:50051 \
  --token-file ./nsetup.auth.token status
```

daemon 的 TCP token 位于 `/etc/nsetup/auth.token`，权限为 `0600`。token 只负责应用层
认证；跨主机流量还应置于 VPN 或 TLS HTTP/2 代理之后。
