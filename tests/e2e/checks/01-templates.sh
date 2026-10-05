# R1：模板 / 骨架 / export 三条 round-trip，以及 metrics、telemetry 是否真的落地。
#
# 依赖：daemon 就绪（run.sh 已启动）。
echo "=== R1 模板、骨架与 export round-trip ==="

E template traefik >"$WORK/fixtures/traefik.toml"
E template app >"$WORK/fixtures/app.toml"
E template static >"$WORK/fixtures/static.toml"

# 骨架显式带 name，去掉 name 后必须等价可应用（0.2.0 时报「缺少字符串 name」）。
sed 's/^name = "traefik"$//' "$WORK/fixtures/traefik.toml" >"$WORK/fixtures/traefik-noname.toml"
expect_contains "traefik 骨架含 name" 'name = "traefik"' "$(cat "$WORK/fixtures/traefik.toml")"
check "traefik 骨架可直接应用" E up -f "$WORK/fixtures/traefik.toml" --force
check "traefik 骨架省略 name 也可应用" E up -f "$WORK/fixtures/traefik-noname.toml" --force
expect_fail "traefik 骨架写错 name 会被拒绝" bash -c "
  sed 's/^name = \"traefik\"\$/name = \"proxy\"/' '$WORK/fixtures/traefik.toml' >'$WORK/fixtures/traefik-badname.toml'
  '$BIN' --endpoint '$NSETUP_ENDPOINT' up -f '$WORK/fixtures/traefik-badname.toml' --force"

# metrics 开关必须落到 Traefik 启动参数与宿主机回环端口上（D4）。
traefik_compose="$WORK/stacks/traefik/compose.yaml"
expect_contains "metrics 落到启动参数" '--metrics.prometheus=true' "$(cat "$traefik_compose")"
expect_contains "metrics 入口声明容器端口" '--entrypoints.metrics.address=:8081' "$(cat "$traefik_compose")"
expect_contains "metrics 只绑定宿主机回环" '127.0.0.1:8081:8081/tcp' "$(cat "$traefik_compose")"

# export → up round-trip：导出结果必须能重新应用。
# 用户拥有的动态配置（custom.yml）在重新应用后必须保留自己的内容。
custom="$WORK/stacks/traefik/config/dynamic/custom.yml"
printf '# 用户自己的路由\n' >"$custom"
E up -f "$WORK/fixtures/traefik.toml" --force >/dev/null 2>&1
expect_contains "重新应用不覆盖用户编辑的 custom.yml" '# 用户自己的路由' "$(cat "$custom")"

# R10-A：dashboard 只能有一条 router，且必须回源到 api@internal。
# 0.2.1 多出的应用式路由会抢掉同一个 Host()，表现为 dashboard 404。
dashboard="traefik.http.routers.nsetup-traefik-traefik-dashboard"
expect_contains "dashboard 指向 api@internal" \
  "$dashboard.service=api@internal" "$(cat "$traefik_compose")"
expect_contains "dashboard 带显式优先级" "$dashboard.priority=1000" "$(cat "$traefik_compose")"
expect_contains "dashboard 限制内网来源" \
  "$dashboard.middlewares=internal-only@file,authelia@file" "$(cat "$traefik_compose")"
expect_contains "dashboard 声明通配符证书域名" \
  "$dashboard.tls.domains[0].sans=*.example.com" "$(cat "$traefik_compose")"
same_host=$(grep -cF ".rule=Host(\`traefik.example.com\`)" "$traefik_compose" || true)
if [ "$same_host" -eq 1 ]; then
  printf 'PASS  %s\n' "traefik 只生成一条抢同一 Host() 的路由"
  PASS=$((PASS + 1))
else
  printf 'FAIL  %s（实际 %s 条）\n' "traefik 只生成一条抢同一 Host() 的路由" "$same_host"
  FAIL=$((FAIL + 1))
fi
if grep -qF "traefik.http.services.${dashboard}.loadbalancer" "$traefik_compose"; then
  printf 'FAIL  %s\n' "dashboard 不声明容器回源端口"
  FAIL=$((FAIL + 1))
else
  printf 'PASS  %s\n' "dashboard 不声明容器回源端口"
  PASS=$((PASS + 1))
fi

# R12：HEALTHCHECK 是独立进程，缺 --ping 时会恒 unhealthy。
expect_contains "健康检查补上 --ping" \
  'healthcheck:
      test:
      - CMD
      - traefik
      - healthcheck
      - --ping' "$(cat "$traefik_compose")"

# R10-B/C：指标入口必须有 router，且内置 tls 中间件必须继续生成。
dynamic="$WORK/stacks/traefik/config/dynamic/nsetup.yml"
expect_contains "指标入口生成 /metrics router" 'rule: Path(`/metrics`)' "$(cat "$dynamic")"
expect_contains "指标入口指向 prometheus@internal" 'service: prometheus@internal' "$(cat "$dynamic")"
expect_contains "指标入口生成 API router" \
  'rule: PathPrefix(`/api`) || PathPrefix(`/debug`)' "$(cat "$dynamic")"
expect_contains "API router 指向 api@internal" 'service: api@internal' "$(cat "$dynamic")"
expect_contains "内置 tls 中间件仍在生成" 'stsSeconds: 31536000' "$(cat "$dynamic")"
expect_contains "内置 tls 中间件含 includeSubDomains" 'stsIncludeSubdomains: true' "$(cat "$dynamic")"

# 路由表把内置后端显示成 api@internal（不带端口），而不是 traefik:8080。
routes=$(E show traefik --routes 2>&1)
expect_contains "路由表显示内置后端" 'api@internal' "$routes"
if grep -qF "traefik.example.com	/	https	http	1000	api@internal	" <<<"$routes"; then
  printf 'PASS  %s\n' "路由表不再显示 dashboard 容器端口"
  PASS=$((PASS + 1))
else
  printf 'FAIL  %s\n' "路由表不再显示 dashboard 容器端口"
  echo "$routes"
  FAIL=$((FAIL + 1))
fi

# R11：label 与 Traefik 实际加载一致时，doctor 不得报「未被加载 / 已不再声明」。
# 临时 daemon 下 Traefik API 通常不可达（doctor 会明确降级），此时不做结论断言。
doctor_out=$(E doctor 2>&1)
if grep -qE '未被 Traefik 加载|已不再声明' <<<"$doctor_out"; then
  printf 'FAIL  %s\n' "doctor 不误报路由缺失/多余"
  echo "$doctor_out"
  FAIL=$((FAIL + 1))
else
  printf 'PASS  %s\n' "doctor 不误报路由缺失/多余"
  PASS=$((PASS + 1))
fi

rm -f "$WORK/fixtures/traefik-export.toml"
E export traefik -o "$WORK/fixtures/traefik-export.toml"
expect_contains "traefik export 带 name" 'name = "traefik"' "$(cat "$WORK/fixtures/traefik-export.toml")"
check "traefik export 可重新应用" E up -f "$WORK/fixtures/traefik-export.toml" --force

# Authelia：骨架 + 真实密钥 + 遥测，同样要求 export → up 可用。
write_authelia_toml "$WORK/fixtures/authelia.toml"
check "authelia 声明可直接应用" E up -f "$WORK/fixtures/authelia.toml" --force
expect_contains "telemetry 落到 configuration.yml" \
  "address: 'tcp://0.0.0.0:9959'" "$(cat "$WORK/stacks/authelia/config/configuration.yml")"

# R9：Authelia 遥测不得再生成 `telemetry.metrics.path`。
# 0.2.1 会把该键写进 configuration.yml，Authelia 4.39.x 报未知配置键并 fatal 退出。
authelia_config="$WORK/stacks/authelia/config/configuration.yml"
configuration=$(cat "$authelia_config")
expect_contains "遥测 metrics 块仍启用" '  metrics:' "$configuration"
# 只看 telemetry 段落：authentication_backend 与 storage 也有合法的 path。
telemetry_block=$(awk '/^telemetry:/{flag=1} flag && /^[^ ]/ && !/^telemetry:/{exit} flag' \
  "$authelia_config")
expect_contains "遥测块含 metrics 地址" "    address: 'tcp://0.0.0.0:9959'" "$telemetry_block"
if grep -qE '^ +path:' <<<"$telemetry_block"; then
  printf 'FAIL  %s\n' "configuration.yml 不含 telemetry.metrics.path"
  FAIL=$((FAIL + 1))
else
  printf 'PASS  %s\n' "configuration.yml 不含 telemetry.metrics.path"
  PASS=$((PASS + 1))
fi

# 真实镜像对照：带 metrics_path 的声明应用后，authelia 仍能加载配置。
if docker image inspect authelia/authelia:4.39.20 >/dev/null 2>&1; then
  unexpected=$(timeout 20 docker run --rm \
    -v "$WORK/stacks/authelia/config:/config:ro" \
    authelia/authelia:4.39.20 --config /config/configuration.yml 2>&1 |
    grep -c 'metrics.path' || true)
  if [ "$unexpected" -eq 0 ]; then
    printf 'PASS  %s\n' "authelia 不再报未知配置键 telemetry.metrics.path"
    PASS=$((PASS + 1))
  else
    printf 'FAIL  %s（出现 %s 次）\n' "authelia 不再报未知配置键 telemetry.metrics.path" "$unexpected"
    FAIL=$((FAIL + 1))
  fi
else
  skip "authelia 不再报未知配置键 telemetry.metrics.path（缺少本地镜像 authelia/authelia:4.39.20）"
fi

rm -f "$WORK/fixtures/authelia-export.toml"
E export authelia -o "$WORK/fixtures/authelia-export.toml"
expect_contains "authelia export 带 name" 'name = "authelia"' "$(cat "$WORK/fixtures/authelia-export.toml")"
check "authelia export 可重新应用" E up -f "$WORK/fixtures/authelia-export.toml" --force
