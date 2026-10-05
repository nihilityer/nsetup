# nsetup 端到端验收测试

在临时 daemon 与本机 Docker 上跑一遍 0.2.1 / 0.2.2 反馈清单
（`../nsetup-0.2.1-feedback.md`、`../nsetup-0.2.2-feedback.md`）里的可离线验收项，
覆盖 R1–R13。与 `cargo test` 的分工：单元测试覆盖解析、生成、校验与存储语义；
这里覆盖需要真实 daemon、真实文件系统权限和 `docker compose` 的端到端行为。

## 运行

```bash
# 使用 cargo build 出的调试二进制
tests/e2e/run.sh

# 使用已有二进制
NSETUP_BIN=/usr/local/bin/nsetup tests/e2e/run.sh

# 保留现场便于排查
tests/e2e/run.sh --work /tmp/nsetup-e2e --keep
```

退出码为 0 表示全部检查通过；失败项以 `FAIL` 前缀打印，退出码为 1。

## 依赖与边界

- `bash`、`docker`（CLI 与可连接的 daemon）、`mktemp`；未提供 `NSETUP_BIN` 时需要
  `cargo`。`08` 的 Traefik 探针额外需要 `curl` 与本地 traefik 镜像，缺失时记为
  `SKIP`。
- **不需要 root**：daemon 以前台普通用户身份运行，`stacks_root`、`data_roots` 与
  socket 全部落在临时目录内，不触碰 `/var/lib/nsetup`、`/run/nsetup` 或 systemd。
  因此 daemon 会打印一条「非 root，跳过 socket 属组设置」的警告，属正常现象。
- 非 root 下 `docker compose restart` 只对已存在容器生效；项目自身都不启动容器
  （`--start` 只用在需要验证钩子 `post_start` 的场景），保证离线可跑。`08` 只额外
  起一个临时的 Traefik 容器来解析生成的动态配置，不部署真实栈、不申请证书。
- 测试目录默认由 `mktemp -d` 创建并在结束时删除；`--keep` 保留。

## 检查项

| 文件 | 覆盖 | 关键断言 |
| --- | --- | --- |
| `checks/01-templates.sh` | R1、D4、R9、R10、R11、R12 | 四模板骨架可直接应用；`name` 可省略也可写、写错报错；metrics 落到启动参数与 `127.0.0.1:8081`；traefik 只有一条 `api@internal` dashboard 路由（priority 1000，无容器回源端口）；healthcheck 带 `--ping`；`nsetup.yml` 里 `/metrics` → `prometheus@internal`、`/api` → `api@internal` 且保留内置 `tls` 中间件；doctor 不误报缺失/多余；authelia 的 `configuration.yml` 不含 `telemetry.metrics.path`；`export → up` 双向 round-trip |
| `checks/02-assets.sh` | R2 | 上传目录 `0755`、文件 `0644`；`--assets-perms private` 为 `0750`/`0640`；`merge` 保留、`replace` 删除旧文件；static 模板接受 `group_add` 与 `[hooks]` |
| `checks/03-routes.sh` | R3 | `show --routes` 同时列出模板生成路由与 label 路由，带 SCHEME、PRIORITY、BACKEND、来源列；无路由项目给出明确结论 |
| `checks/04-mounts.sh` | R6 | 相对挂载源展开为项目内绝对路径且不残留 `./`；export 过滤 `--files` 注入挂载；命名卷与 `../` 被拒绝；`data_roots` 绝对路径继续可用 |
| `checks/05-hooks.sh` | R4、R5 | 钩子成功回显 stdout；失败含退出码、stdout/stderr 与容器状态且返回非零；OIDC 片段写入与 PKCE 输出；已重启时文案不出现「重启后生效」 |
| `checks/06-files-only.sh` | R7 | 项目目录 inode 不变、`compose.yaml` 不重写、新文件落盘且权限正确；未部署项目与缺资源时明确报错 |
| `checks/07-authelia.sh` | R8 | `/config` 可写且 `/secrets` 只读；OIDC 片段原地重写（inode 不变、无孤儿临时文件、权限保持）；片段在容器内可读；有本地镜像时用真实 Authelia 镜像验证无 chown 只读报错 |
| `checks/08-traefik-dynamic.sh` | R13 | `config/dynamic` 不存在时 `up` 播种的 `custom.yml` 不含任何生效的 YAML（空映射会让 file provider 整体失败）；真实 Traefik 加载生成的动态配置后 `/metrics`、`/api/rawdata` 均 200、日志无 `standalone element` 与 `tls@file`；0.2.0–0.2.2 的旧骨架被自动替换，用户改过的内容逐字节保留 |

## 说明

- `07` 会在结束时用一次性容器把配置目录属主改回当前用户；真实 Authelia 镜像缺失时
  该项记为 `SKIP` 而不是失败。
- `08` 会启动一个只加载 file provider 的临时 Traefik 容器（随机宿主机端口，不占用
  `127.0.0.1:8081`），镜像优先取项目 `.env` 里声明的版本、其次任意本地 `traefik:*`；
  没有本地镜像或缺 `curl` 时记为 `SKIP`。
- 需要真机才能覆盖的项（`doctor` 完整模式、`--restart-dependents` 对运行中
  authelia 的 `StartedAt` 影响）不在这里，按反馈文档在测试机上手工执行。
- R10 的运行时断言（302、`/metrics` 与 `/api/rawdata` 返回 200、HSTS）需要可解析
  域名、ACME 与真实 Traefik 容器，在测试机上手工执行；这里断言的是决定这些结果的
  生成产物（label、`nsetup.yml`、健康检查命令）。R13 的 `/metrics` 与 `/api/rawdata`
  断言不需要域名与 ACME（两个入口都在 metrics entrypoint 上），因此由 `08` 真实执行。
