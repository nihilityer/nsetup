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

rm -f "$WORK/fixtures/traefik-export.toml"
E export traefik -o "$WORK/fixtures/traefik-export.toml"
expect_contains "traefik export 带 name" 'name = "traefik"' "$(cat "$WORK/fixtures/traefik-export.toml")"
check "traefik export 可重新应用" E up -f "$WORK/fixtures/traefik-export.toml" --force

# Authelia：骨架 + 真实密钥 + 遥测，同样要求 export → up 可用。
write_authelia_toml "$WORK/fixtures/authelia.toml"
check "authelia 声明可直接应用" E up -f "$WORK/fixtures/authelia.toml" --force
expect_contains "telemetry 落到 configuration.yml" \
  "address: 'tcp://0.0.0.0:9959'" "$(cat "$WORK/stacks/authelia/config/configuration.yml")"
rm -f "$WORK/fixtures/authelia-export.toml"
E export authelia -o "$WORK/fixtures/authelia-export.toml"
expect_contains "authelia export 带 name" 'name = "authelia"' "$(cat "$WORK/fixtures/authelia-export.toml")"
check "authelia export 可重新应用" E up -f "$WORK/fixtures/authelia-export.toml" --force
