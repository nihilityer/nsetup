# nsetup 端到端验收测试

在临时 daemon 与本机 Docker 上跑一遍 0.2.1 反馈清单（`../nsetup-0.2.1-feedback.md`）
里的全部验收项，覆盖 R1–R8。与 `cargo test` 的分工：单元测试覆盖解析、生成、
校验与存储语义；这里覆盖需要真实 daemon、真实文件系统权限和 `docker compose`
的端到端行为。

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

- `bash`、`docker`（CLI 与可连接的 daemon）、`mktemp`；未提供 `NSETUP_BIN` 时需要 `cargo`。
- **不需要 root**：daemon 以前台普通用户身份运行，`stacks_root`、`data_roots` 与
  socket 全部落在临时目录内，不触碰 `/var/lib/nsetup`、`/run/nsetup` 或 systemd。
  因此 daemon 会打印一条「非 root，跳过 socket 属组设置」的警告，属正常现象。
- 非 root 下 `docker compose restart` 只对已存在容器生效；本套测试的项目都不启动
  容器（`--start` 只用在需要验证钩子 `post_start` 的场景），保证离线可跑。
- 测试目录默认由 `mktemp -d` 创建并在结束时删除；`--keep` 保留。

## 检查项

| 文件 | 覆盖 | 关键断言 |
| --- | --- | --- |
| `checks/01-templates.sh` | R1、D4 | 四模板骨架可直接应用；`name` 可省略也可写、写错报错；metrics 落到启动参数与 `127.0.0.1:8081`；telemetry 落到 `configuration.yml`；`export → up` 双向 round-trip |
| `checks/02-assets.sh` | R2 | 上传目录 `0755`、文件 `0644`；`--assets-perms private` 为 `0750`/`0640`；`merge` 保留、`replace` 删除旧文件；static 模板接受 `group_add` 与 `[hooks]` |
| `checks/03-routes.sh` | R3 | `show --routes` 同时列出模板生成路由与 label 路由，带 SCHEME、PRIORITY、BACKEND、来源列；无路由项目给出明确结论 |
| `checks/04-mounts.sh` | R6 | 相对挂载源展开为项目内绝对路径且不残留 `./`；export 过滤 `--files` 注入挂载；命名卷与 `../` 被拒绝；`data_roots` 绝对路径继续可用 |
| `checks/05-hooks.sh` | R4、R5 | 钩子成功回显 stdout；失败含退出码、stdout/stderr 与容器状态且返回非零；OIDC 片段写入与 PKCE 输出；已重启时文案不出现「重启后生效」 |
| `checks/06-files-only.sh` | R7 | 项目目录 inode 不变、`compose.yaml` 不重写、新文件落盘且权限正确；未部署项目与缺资源时明确报错 |
| `checks/07-authelia.sh` | R8 | `/config` 可写且 `/secrets` 只读；OIDC 片段原地重写（inode 不变、无孤儿临时文件、权限保持）；片段在容器内可读；有本地镜像时用真实 Authelia 镜像验证无 chown 只读报错 |

## 说明

- `07` 会在结束时用一次性容器把配置目录属主改回当前用户；真实 Authelia 镜像缺失时
  该项记为 `SKIP` 而不是失败。
- 需要真机才能覆盖的两项（`doctor` 完整模式、`--restart-dependents` 对运行中
  authelia 的 `StartedAt` 影响）不在这里，按反馈文档第 6 节在测试机上手工执行。
