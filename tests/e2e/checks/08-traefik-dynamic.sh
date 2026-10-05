# R13：全新环境（config/dynamic 为空）第一次 up 播种的 custom.yml 必须是 Traefik
# 能解析的动态配置。
#
# 0.2.0–0.2.2 的骨架含 `middlewares: {}` 这类空映射，Traefik 的 structures 解码器把
# 它判定为 standalone element 并让 file provider 整体构建失败：同目录 nsetup.yml 里的
# metrics / api 路由与内置 tls 中间件一起失效（/metrics 404、响应缺 HSTS），容器自身
# 却仍然 healthy，从容器状态看不出问题。
#
# 依赖：daemon 就绪（run.sh 已启动）；真实 Traefik 探针在本机有 traefik 镜像与 curl
# 时执行，否则记为 SKIP。
echo "=== R13 首次播种的 custom.yml 与 Traefik file provider ==="

E template traefik >"$WORK/fixtures/traefik-r13.toml"
project="$WORK/stacks/traefik"
dynamic="$project/config/dynamic"
custom="$dynamic/custom.yml"

# 模拟全新机器：整个动态配置目录都不存在，custom.yml 只能由本次 up 播种。
rm -rf "$dynamic"
check "R13 动态配置目录不存在时可直接部署" E up -f "$WORK/fixtures/traefik-r13.toml" --force
check "R13 播种 custom.yml" test -f "$custom"
check "R13 同时生成 nsetup.yml" test -f "$dynamic/nsetup.yml"

# 注释以外的任何一行都会进入 Traefik 的解码器，因此生效内容必须为空。
active=$(grep -vE '^[[:space:]]*(#|$)' "$custom" || true)
if [ -z "$active" ]; then
  printf 'PASS  %s\n' "R13 骨架不含任何生效的 YAML（空映射会让 file provider 整体失败）"
  PASS=$((PASS + 1))
else
  printf 'FAIL  %s\n' "R13 骨架不含任何生效的 YAML（空映射会让 file provider 整体失败）"
  echo "$active"
  FAIL=$((FAIL + 1))
fi
expect_contains "R13 骨架说明空映射的后果" 'standalone element' "$(cat "$custom")"

# 真实 Traefik 加载生成的动态配置目录，断言与验收 A 一致的两个入口（验收 C）。
if ! command -v curl >/dev/null 2>&1; then
  skip "R13 真实 Traefik 加载动态配置（缺少 curl）"
elif image=$(traefik_probe_image "$project"); then
  if port=$(traefik_probe_start "$image" "$dynamic" nsetup-e2e-r13); then
    if code=$(wait_http_status "http://127.0.0.1:$port/metrics" 200); then
      printf 'PASS  %s\n' "R13 冷启动 /metrics 返回 200（$image）"
      PASS=$((PASS + 1))
    else
      printf 'FAIL  %s\n' "R13 冷启动 /metrics 返回 200（$image，实际 $code）"
      FAIL=$((FAIL + 1))
    fi
    if code=$(wait_http_status "http://127.0.0.1:$port/api/rawdata" 200); then
      printf 'PASS  %s\n' "R13 冷启动 /api/rawdata 返回 200"
      PASS=$((PASS + 1))
    else
      printf 'FAIL  %s\n' "R13 冷启动 /api/rawdata 返回 200（实际 $code）"
      FAIL=$((FAIL + 1))
    fi
    log=$(docker logs nsetup-e2e-r13 2>&1 | sed 's/\x1b\[[0-9;]*m//g')
    if grep -q 'standalone element' <<<"$log"; then
      printf 'FAIL  %s\n' "R13 Traefik 不再报 standalone element"
      grep 'standalone element' <<<"$log"
      FAIL=$((FAIL + 1))
    else
      printf 'PASS  %s\n' "R13 Traefik 不再报 standalone element"
      PASS=$((PASS + 1))
    fi
    if grep -q 'tls@file' <<<"$log"; then
      printf 'FAIL  %s\n' "R13 内置 tls 中间件可用（无 tls@file 报错）"
      grep 'tls@file' <<<"$log"
      FAIL=$((FAIL + 1))
    else
      printf 'PASS  %s\n' "R13 内置 tls 中间件可用（无 tls@file 报错）"
      PASS=$((PASS + 1))
    fi
    docker rm -f nsetup-e2e-r13 >/dev/null 2>&1 || true
  else
    skip "R13 真实 Traefik 加载动态配置（无法启动探针容器）"
  fi
else
  skip "R13 真实 Traefik 加载动态配置（本机没有 traefik 镜像）"
fi

# 0.2.0–0.2.2 播种过的旧骨架必须自愈，否则升级后故障仍在（用户不会去删它）。
cat >"$custom" <<'EOF'
# 手工追加的 Traefik 动态配置。
#
# 本文件与 nsetup.yml 位于同一个目录，traefik 的 file provider 会加载目录中所有
# `*.yml`，因此这里的路由与中间件不会被 `nsetup up -f traefik.toml` 清除。
# 也可以在同目录新增其它 `*.yml` 文件，效果相同。
http:
  routers: {}
  services: {}
  middlewares: {}
EOF
E up -f "$WORK/fixtures/traefik-r13.toml" --force >/dev/null 2>&1
active=$(grep -vE '^[[:space:]]*(#|$)' "$custom" || true)
if [ -z "$active" ]; then
  printf 'PASS  %s\n' "R13 升级时替换 nsetup 自己播种过的旧骨架"
  PASS=$((PASS + 1))
else
  printf 'FAIL  %s\n' "R13 升级时替换 nsetup 自己播种过的旧骨架"
  echo "$active"
  FAIL=$((FAIL + 1))
fi

# 用户改过的内容必须逐字节保留，不被新骨架覆盖。
printf '# 用户自己的路由\nhttp:\n  routers:\n    mine:\n      rule: Path(`/mine`)\n      service: noop@internal\n' >"$custom"
cp "$custom" "$WORK/fixtures/custom-edited.yml"
E up -f "$WORK/fixtures/traefik-r13.toml" --force >/dev/null 2>&1
check "R13 用户改过的 custom.yml 逐字节保留" cmp -s "$custom" "$WORK/fixtures/custom-edited.yml"
