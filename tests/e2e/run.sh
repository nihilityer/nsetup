#!/usr/bin/env bash
# nsetup 端到端验收测试：在临时 daemon + 本机 Docker 上跑 R1–R8 全量检查。
#
# 用法：
#   tests/e2e/run.sh                       # 自动 cargo build 后运行
#   NSETUP_BIN=./target/debug/nsetup tests/e2e/run.sh
#   tests/e2e/run.sh --work /tmp/nsetup-e2e --keep
#
# 依赖：bash、docker（CLI 与可连接的 daemon）、mktemp；可选 cargo（未给二进制时）。
# 不依赖 root：daemon 以前台普通用户身份运行，socket 只服务本用户。
# 详见 tests/e2e/README.md。

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
CHECKS_DIR="$ROOT/tests/e2e/checks"
DOMAIN="test.example.com"
KEEP=0
WORK=""
DAEMON_PID=""

usage() {
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --work)
      WORK=${2:?--work 需要目录参数}
      shift 2
      ;;
    --keep)
      KEEP=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "未知参数: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

# shellcheck source=tests/e2e/lib.sh
. "$ROOT/tests/e2e/lib.sh"

if [ -z "$WORK" ]; then
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/nsetup-e2e-XXXXXX")
fi
mkdir -p "$WORK"

cleanup() {
  stop_daemon
  if [ "$KEEP" -eq 0 ]; then
    rm -rf "$WORK"
  else
    echo "保留测试目录: $WORK"
  fi
}
trap cleanup EXIT

if ! require_commands; then
  exit 1
fi

if [ -z "${NSETUP_BIN:-}" ]; then
  echo "未指定 NSETUP_BIN，使用 cargo build 构建调试二进制"
  if ! command -v cargo >/dev/null 2>&1; then
    echo "缺少 cargo，请用 NSETUP_BIN=<路径> 指定已构建的 nsetup" >&2
    exit 1
  fi
  if ! (cd "$ROOT" && cargo build --quiet); then
    echo "cargo build 失败" >&2
    exit 1
  fi
  NSETUP_BIN="$ROOT/target/debug/nsetup"
fi
BIN=$(cd -- "$(dirname -- "$NSETUP_BIN")" && pwd)/$(basename -- "$NSETUP_BIN")
if [ ! -x "$BIN" ]; then
  echo "nsetup 二进制不可执行: $BIN" >&2
  exit 1
fi

echo "nsetup 二进制: $BIN"
echo "测试工作目录: $WORK"
setup_workdir

if ! start_daemon; then
  exit 1
fi
echo "daemon 版本: $(E status | head -1)"
echo

for check_file in "$CHECKS_DIR"/*.sh; do
  # shellcheck source=/dev/null
  . "$check_file"
  echo
done

total=$((PASS + FAIL))
echo "==================================================="
printf '通过 %d / %d 项' "$PASS" "$total"
if [ "$SKIP" -ne 0 ]; then
  printf '，跳过 %d 项' "$SKIP"
fi
if [ "$FAIL" -ne 0 ]; then
  printf '，失败 %d 项\n' "$FAIL"
  exit 1
fi
printf '\n'
exit 0
