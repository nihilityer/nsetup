#!/usr/bin/env bash
# nsetup 端到端验收测试公共函数库。
#
# 被 run.sh source，不单独执行。约定：
#   * 每个 tests/e2e/checks/*.sh 只依赖本文件提供的 BIN/E/check/expect_* 辅助；
#   * 所有状态都落在 $WORK 目录内，测试不触碰 /var/lib/nsetup 或系统 systemd；
#   * daemon 以前台普通用户身份运行，失败时日志在 $WORK/daemon.log。

set -u

# ---- 断言与统计 -------------------------------------------------------------

PASS=0
FAIL=0
SKIP=0

# 记录一项被跳过的检查（环境不满足，例如缺少本地镜像）。
skip() {
  printf 'SKIP  %s\n' "$1"
  SKIP=$((SKIP + 1))
}

# 断言一个命令成功执行。
check() {
  local label=$1
  shift
  if "$@" >/dev/null 2>&1; then
    printf 'PASS  %s\n' "$label"
    PASS=$((PASS + 1))
  else
    printf 'FAIL  %s\n' "$label"
    FAIL=$((FAIL + 1))
  fi
}

# 断言命令失败（用于反例）。
expect_fail() {
  local label=$1
  shift
  if "$@" >/dev/null 2>&1; then
    printf 'FAIL  %s（应当失败却成功了）\n' "$label"
    FAIL=$((FAIL + 1))
  else
    printf 'PASS  %s\n' "$label"
    PASS=$((PASS + 1))
  fi
}

# 断言输出包含子串：expect_contains <描述> <子串> <输出>
expect_contains() {
  local label=$1
  local needle=$2
  local haystack=$3
  if grep -qF -- "$needle" <<<"$haystack"; then
    printf 'PASS  %s\n' "$label"
    PASS=$((PASS + 1))
  else
    printf 'FAIL  %s（未找到 %s）\n' "$label" "$needle"
    echo "----- 实际输出 -----"
    echo "$haystack"
    echo "--------------------"
    FAIL=$((FAIL + 1))
  fi
}

# 断言权限位：expect_mode <描述> <路径> <八进制权限>
expect_mode() {
  local label=$1
  local path=$2
  local expected=$3
  local actual
  actual=$(file_mode "$path")
  if [ "$actual" = "$expected" ]; then
    printf 'PASS  %s\n' "$label"
    PASS=$((PASS + 1))
  else
    printf 'FAIL  %s（期望 %s，实际 %s）\n' "$label" "$expected" "$actual"
    FAIL=$((FAIL + 1))
  fi
}

# 返回路径的八进制权限位（GNU 与 BSD stat 都支持）。
file_mode() {
  if stat -c '%a' "$1" >/dev/null 2>&1; then
    stat -c '%a' "$1"
  else
    stat -f '%Lp' "$1"
  fi
}

# 返回路径的 inode。
file_inode() {
  if stat -c '%i' "$1" >/dev/null 2>&1; then
    stat -c '%i' "$1"
  else
    stat -f '%i' "$1"
  fi
}

# ---- 环境准备 ---------------------------------------------------------------

# 校验必需的外部命令。
require_commands() {
  local missing=0
  for command in docker mktemp; do
    if ! command -v "$command" >/dev/null 2>&1; then
      echo "缺少必需命令: $command" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    return 1
  fi
  if ! docker info >/dev/null 2>&1; then
    echo "无法连接 Docker daemon（docker info 失败）" >&2
    return 1
  fi
  return 0
}

# 准备 daemon 配置、数据目录与 socket 路径。
setup_workdir() {
  mkdir -p "$WORK"/{stacks,data,run,fixtures}
  cat >"$WORK/config.toml" <<EOF
domain = "$DOMAIN"
stacks_root = "$WORK/stacks"
data_roots = ["$WORK/data"]
listen = "unix://$WORK/run/nsetup.sock"
docker_socket = "/var/run/docker.sock"
EOF
  export NSETUP_CONFIG="$WORK/config.toml"
  export NSETUP_ENDPOINT="unix://$WORK/run/nsetup.sock"
}

# nsetup 的简写：始终指向本次测试的 daemon。
E() {
  "$BIN" --endpoint "$NSETUP_ENDPOINT" "$@"
}

# 前台启动 daemon 并等待 socket 可连接。
start_daemon() {
  rm -f "$WORK/run/nsetup.sock"
  NSETUP_CONFIG="$WORK/config.toml" "$BIN" daemon >"$WORK/daemon.log" 2>&1 &
  DAEMON_PID=$!
  local attempt=0
  while [ "$attempt" -lt 50 ]; do
    if E status >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      echo "daemon 已退出，日志如下：" >&2
      cat "$WORK/daemon.log" >&2
      return 1
    fi
    attempt=$((attempt + 1))
    sleep 0.2
  done
  echo "daemon 未在 10 秒内就绪，日志如下：" >&2
  cat "$WORK/daemon.log" >&2
  return 1
}

# 停止 daemon 并等待其释放 socket。
stop_daemon() {
  if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
}

# ---- 夹具 -------------------------------------------------------------------

# 生成静态站点资源目录。
write_assets() {
  local root=$1
  mkdir -p "$root"
  printf '<h1>hi</h1>\n' >"$root/index.html"
}

# 生成 `--files` 上传目录（含一层子目录，用于验证铺平与权限）。
write_files() {
  local root=$1
  mkdir -p "$root/sub"
  printf 'a: 1\n' >"$root/a.yaml"
  printf 'b: 2\n' >"$root/sub/b.yaml"
}

# 生成带 hooks / group_add 的 static 模板夹具。
write_static_toml() {
  local path=$1
  cat >"$path" <<'EOF'
format = 1
template = "static"
name = "docs"
host = "docs"
version = "1.27"
middlewares = ["gzip"]
group_add = ["988"]
[hooks]
pre_start = ["mkdir -p site && ls -l site"]
EOF
}

# 生成多路由 + 用户 label 的项目夹具（R3）。
write_routes_toml() {
  local path=$1
  cat >"$path" <<'EOF'
format = 1
name = "netbird"

[services.server]
image = "netbirdio/netbird"
version = "0.50.0"
container_name = "netbird-server"
port = 80
restart = "unless-stopped"

[services.server.traefik]

[services.server.traefik.routes.grpc]
hosts = ["netbird"]
path_prefix = "/signalexchange.SignalExchange"
port = 10000
protocol = "h2c"
priority = 200

[services.server.traefik.routes.http]
hosts = ["netbird"]
path_prefix = "/api"
port = 80
priority = 100

[services.dashboard]
image = "netbirdio/dashboard"
version = "2.0"
port = 80
labels = [
    "traefik.http.routers.custom-extra.rule=Host(`extra.example.com`)",
    "traefik.http.services.custom-extra.loadbalancer.server.port=8080",
    "traefik.http.routers.custom-extra.priority=7",
    "traefik.http.routers.custom-extra.middlewares=gzip@file",
]

[services.dashboard.traefik]
hosts = ["netbird"]
path_prefix = "/"
priority = 1
EOF
}

# 生成相对挂载源夹具（R6）。
write_relative_mounts_toml() {
  local path=$1
  cat >"$path" <<'EOF'
format = 1
name = "relmount"

[services.web]
image = "example/web"
version = "1.0"
port = 8080
volumes = ["files/config.yaml:/etc/app/config.yaml:ro", "./files:/opt/rel:ro"]

[services.web.traefik]
hosts = ["relmount"]
EOF
}

# 生成命名卷反例夹具（R6）。
write_named_volume_toml() {
  local path=$1
  cat >"$path" <<'EOF'
format = 1
name = "badmount"

[services.web]
image = "example/web"
version = "1.0"
volumes = ["mydata:/data"]
EOF
}

# 生成钩子失败反例夹具（R5）。
write_failing_hook_toml() {
  local path=$1
  cat >"$path" <<'EOF'
format = 1
name = "hookfail"

[services.web]
image = "example/web"
version = "1.0"

[services.web.hooks]
pre_start = ["echo 钩子诊断行; echo 钩子失败原因 >&2; exit 3"]
EOF
}

# 生成声明 OIDC 客户端的应用夹具（R8）。
write_oidc_app_toml() {
  local path=$1
  cat >"$path" <<'EOF'
format = 1
name = "oidcapp"

[services.web]
image = "example/web"
version = "1.0"
port = 8080

[services.web.traefik]
hosts = ["oidcapp"]

[authelia.oidc_clients.oidcapp]
client_name = "OIDC App"
public = true
redirect_uris = ["https://oidcapp.example.com/oauth/callback"]
require_pkce = true
token_endpoint_auth_method = "none"
EOF
}

# 生成可解析的 Authelia 声明：骨架 + 真实密钥 + OIDC provider + 遥测。
#
# 启用 `[oidc]` 是必需的：应用只有在 Authelia 已启用 provider 时才允许声明客户端，
# 而 OIDC 客户端片段正是 R8 要验证的写入路径。私钥使用结构合法的占位 PEM——端到端
# 测试不启动 Authelia 进程，只需要通过 nsetup 的格式校验。
write_authelia_toml() {
  local path=$1
  E template authelia \
    | sed \
      -e 's/replace-with-at-least-32-random-characters/0123456789abcdef0123456789abcdef/g' \
      -e "s|'\$argon2id\$replace-with-generated-password-hash'|'\$argon2id\$v=19\$m=65536,t=3,p=4\$c2FsdA\$aGFzaA'|" \
      -e "s|^default_redirection_url = .*|default_redirection_url = \"https://auth.${DOMAIN}\"|" \
    >"$path"
  cat >>"$path" <<'EOF'

[oidc]
hmac_secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
jwk_private_key = """
-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj
MzEfYyjiWA4R4/M2bS1GB4t7NXp98C3SC6dVMvDuictGeurT8jNbvJZHtCSuYEvu
NMoSfm76oqFvAp8Gy0iz5sxjZmSnXyCdPEovGhLa0VzMaQ8s+CLOyS56YyCFGeJZ
qgtzJ6GR3eqoYSW9b9UMvkBpZODSctWSNGj3P7jRFDO5VoTwCQAWbFnOjDfH5Ulg
p2PKSQnSJP3AJLQNFNe7br1XbrhV//eO+t51mIpGSDCUv3E0DDFcWDTH9cXDTTlR
ZVEiR2B6REkLp4xOZ0hZBvJkPqGrJvRpxPqCsRh5LG
-----END PRIVATE KEY-----
"""

[telemetry]
metrics_address = "tcp://0.0.0.0:9959"
metrics_path = "/metrics"
EOF
}
