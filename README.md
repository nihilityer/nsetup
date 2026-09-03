# nsetup

`nsetup` 是面向单机家庭服务器的 Docker Compose 管理工具。一个静态链接二进制同时
提供 systemd daemon 与 CLI：daemon 以 root 管理 Docker，日常命令通过本机 Unix
socket 调用，无需反复使用 `sudo`，也不会默认开放管理端口。

## 能做什么

- 用 TOML 声明并管理单服务、多服务和静态站点；
- 生成 Traefik 与 Authelia 基础设施；
- 导入受支持的 Compose 项目，并从当前状态导出 TOML；
- 统一执行启动、停止、升级、日志和删除等操作；
- 限制 bind mount 根目录，并支持带 token 的远程 daemon。

## 快速开始

准备 Linux、systemd 与 Docker Compose。下载或构建 `nsetup` 后初始化 daemon：

```bash
chmod +x ./nsetup
sudo ./nsetup init \
  --domain example.com \
  --stacks-root /var/lib/nsetup/stacks \
  --data-root /srv/data
sudo usermod -aG nihility "$USER"
```

重新登录，让用户组生效，然后检查运行状态：

```bash
nsetup status
```

生成带完整注释的应用配置，修改后部署：

```bash
nsetup template app > app.toml
nsetup up -f app.toml --start
nsetup list
```

TOML 中的镜像必须使用独立、明确的 `version`，不能省略或使用 `latest`。短路由主机名
会自动拼接初始化时设置的主域名。

## 文档

- [使用与运维](docs/USAGE.md)：安装、模板部署、导入导出、日常命令和远程管理；
- [Authelia 认证](docs/AUTHELIA.md)：TOTP、ForwardAuth、OIDC 和密钥生命周期；
- [架构设计](docs/ARCHITECTURE.md)：数据流、IR、RPC、磁盘布局和设计约束；
- [gRPC 接口](proto/nsetup.proto)：CLI 与 daemon 的线路协议。

命令参数以 `nsetup --help` 和 `nsetup <命令> --help` 为准；配置字段以
`nsetup template app|traefik|authelia|static` 输出的骨架为准。

## 开发

```bash
cargo fmt --check
cargo test --quiet
cargo clippy --all-targets --quiet -- -D warnings
git diff --check
```
