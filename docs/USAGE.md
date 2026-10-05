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

同一个 host 下可以按路径或协议拆分多条路由：冲突判定使用
`host + path_prefix + entrypoint + protocol` 组合，因此下面这种官方
NetBird 模板里的三段式路由是合法的，`entrypoint` 省略时为 `https`：

```toml
[services.dashboard.traefik.routes.web]
hosts = ["netbird"]

[services.dashboard.traefik.routes.api]
hosts = ["netbird"]
path_prefix = "/api"

[services.dashboard.traefik.routes.grpc]
hosts = ["netbird"]
path_prefix = "/grpc"
port = 10000
protocol = "h2c"
```

手写 `labels` 仍是逃生舱：其中的 `Host(...)` 规则不参与冲突校验，
`traefik.enable` 也不会被剥离。`middlewares` 除内置的 `authelia`、`gzip`、
`forwarded-headers`、`internal-only`、`tls` 外，还可以引用 traefik.toml 里
`[middlewares.<名称>]` 声明的自定义中间件（例如 `replacePath`、`stripPrefix`）。

应用配置并启动；更新已有项目时增加 `--force`：

```bash
nsetup up -f app.toml --start
nsetup up -f app.toml --force --start
```

### 宿主机文件与启动钩子

需要把宿主机文件交给容器读取时，不必使用 sudo 或一次性特权容器：`--files`
把路径上传到项目目录的 `files/`，并以只读方式挂到每个服务的
`/opt/nsetup/files`（用 `--files-into` 改挂载点）。目录参数的内容会**铺平**到
`files/` 根（不保留目录名），多个 `--files` 会合并，同名目标路径会被拒绝；单个
文件参数按文件名上传：

```bash
nsetup up -f app.toml --files ./netbird.yaml --start
# 容器内读取 /opt/nsetup/files/netbird.yaml
nsetup up -f observability.toml --files ./prometheus --start
# ./prometheus/* 直接落到 files/，容器内读取 /opt/nsetup/files/prometheus.yml
```

上传目录为 `0755`、文件为 `0644`（`a+rX`），容器内的非 root 进程可以直接读取；
需要收紧时用 `--assets-perms private`（目录 `0750`、文件 `0640`）。

`volumes` 的挂载源可以写绝对路径，也可以写相对项目目录的路径；相对写法让同一份
TOML 在 `stacks_root` 变更后仍然可用：

```toml
[services.prometheus]
volumes = ["files/prometheus.yml:/etc/prometheus/prometheus.yml:ro"]
```

一次性初始化动作（建库、生成注册令牌、初始化 owner）用启动钩子声明。钩子在
daemon 主机上以项目目录为工作目录、通过 `sh -c` 顺序执行，因此可以使用 shell
语法与项目内相对路径；`pre_start` 在 `compose up` 之前执行，`post_start` 只在
`--start` 时、于启动之后执行：

```toml
[services.gitea.hooks]
pre_start = ["install -d -m 0755 data"]
post_start = ["docker exec gitea gitea admin user create --username owner"]
```

钩子的运行环境：执行身份是 daemon 的运行用户（systemd 安装下为 root），可写路径
只有受管项目目录与 `data_roots`，`/tmp` 与其余系统目录在 systemd 沙箱下是**只读**
的（`ProtectSystem=strict`）。钩子的 stdout/stderr 会作为进度信息回显；失败时报错
包含退出码、完整输出、容器当前状态与补救命令。完整说明见 `nsetup up --help`
末尾的「运行环境」一节。

容器需要以固定用户（尤其 root）读取宿主机文件时使用 `user` 与 `group_add`；
static 模板同样支持这两个字段与顶层 `[hooks]`：

```toml
[services.otel-collector]
user = "0:0"
group_add = ["988"]
```

健康检查不写时不会覆盖镜像自带探针；没有 shell 的镜像必须使用 argv 形式：

```toml
[services.tuwunel.healthcheck]
command = ["/usr/bin/curl", "-f", "http://127.0.0.1:8008/health"]
```

同样的选择在 `edit` 上对应 `--healthcheck-cmd`（shell 字符串）与
`--healthcheck-exec`（argv 列表，两者互斥）。

### 基础设施与静态站点

通常先部署 Traefik；需要统一认证时再部署 Authelia，然后部署普通应用：

```bash
chmod 600 traefik.toml authelia.toml
nsetup up -f traefik.toml --start
nsetup up -f authelia.toml --start
nsetup up -f app.toml --start
```

`traefik` / `authelia` 模板的项目名固定，`name` 可以省略；`nsetup template` 的骨架与
`nsetup export` 的产出都可以直接 `nsetup up`。

Authelia 的用户、TOTP、ForwardAuth、OIDC 和密钥操作见
[Authelia 认证](AUTHELIA.md)。

静态站点的文件通过 `--assets` 上传，站点根目录是容器内的 `/opt/nsetup/site`。
默认 `--assets-mode merge` 只覆盖同名文件，因此 `--force` 不会清空站点；需要删除
已下线的旧文件时显式整体替换：

```bash
nsetup up -f static.toml --assets ./dist --start
nsetup up -f static.toml --assets ./dist --assets-mode replace --force --start
```

上传后的站点目录是 `0755`、文件 `0644`，官方 nginx 镜像的 worker（UID 101）可以
直接读取；需要收紧权限时加 `--assets-perms private`。

### 只同步资源（不重建容器）

改动只涉及 `--files` / `--assets` 的内容时，用 `--files-only` 跳过 Compose 重建：
项目目录 inode 不变，运行中容器的 bind mount 立即看到新内容：

```bash
nsetup up -f observability.toml --files ./prometheus --files-only
```

该模式只同步资源，不写 `compose.yaml` / `.env`，也不执行钩子与 OIDC 客户端同步；
声明本身有变化（镜像版本、路由、端口等）时仍要正常执行一次 `nsetup up --force`。

## 导入、编辑与导出

导入既有 Compose 项目：

```bash
nsetup import media -f compose.yaml --env-file .env --start
```

IR 不支持但常见于生产文件的字段（`version`、`depends_on`、`deploy`、`x-*` 等）
会被忽略，并在结果中列出清单；命名卷、相对 bind mount（导入路径要求可审计的绝对
路径，TOML 模板另见上文）、未固定版本镜像等影响安全的字段仍会被拒绝。局部编辑单个
服务或导出当前状态：

```bash
nsetup edit media --service web --version 1.1 --start
nsetup export media -o media.toml
nsetup export media --keep-comments -o media.toml
```

`export` 从当前 `compose.yaml` 与 `.env` 反解 TOML；原始注释无法从 Compose 状态
恢复，`--keep-comments` 会在结果前追加当前版本的带注释骨架，使产物可以直接作为
仓库交付物。目标文件已存在时不会覆盖。

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
`rm --force` 仅跳过交互确认；标准输入不是终端且未指定 `--force` 时直接报错并提示，
而不是返回 `not a terminal`。

自查域名 404 / 502 时使用 `doctor`；它比对容器上的 Traefik label 与 Traefik 实际
加载的 router，列出未被接管的服务与原因，并在发现问题时以非零状态退出。它读的是
Traefik 内置 API（`api@internal`），因此依赖 traefik 模板的 `metrics` 入口；该入口
不可达时会明确降级为仅检查容器 label。
`show --routes` 输出该项目最终生效的完整路由表：模板生成的（来源 `nsetup`）与用户
手写 label 的（来源 `labels`）一起列出，包含 HOST、PATH、ENTRYPOINT、SCHEME、
PRIORITY、BACKEND 与 MIDDLEWARES。BACKEND 通常是 `服务:端口`；指向 Traefik 内置
服务的路由（例如 traefik 项目的 dashboard）显示为 `api@internal`：

```bash
nsetup show media --routes
nsetup doctor
```

查询结果与最终操作结果写入 stdout，诊断和中间进度写入 stderr。`pull` 在终端中显示
单行进度，重定向或管道中输出稳定的制表符分隔事件；`logs --follow` 在客户端退出后
终止对应的 Compose 日志进程。

## daemon 配置与安全边界

主域名可以不重装 daemon 直接更新：

```bash
nsetup config set domain example.com
```

命令由 daemon 校验域名并原子改写 `/etc/nsetup/config.toml`，然后重启
`nsetup.service`。已部署项目中的完整域名不会被自动改写，输出会列出仍需手工调整
的项目。

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
