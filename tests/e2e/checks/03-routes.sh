# R3：show --routes 必须同时给出模板生成路由与用户 label 路由，并带 BACKEND/PRIORITY。
#
# 0.2.0 的回归点是 router 名前缀按空项目名拼接，导致模板生成的路由永远匹配不上，
# 输出固定为「该项目没有 Traefik 路由」。
echo "=== R3 show --routes 路由表 ==="

write_routes_toml "$WORK/fixtures/routes.toml"
check "多路由项目可应用" E up -f "$WORK/fixtures/routes.toml" --force

routes=$(E show netbird --routes 2>&1)
expect_contains "输出路由表标题" '最终生效的 Traefik 路由' "$routes"
expect_contains "输出表头" 'HOST	PATH	ENTRYPOINT	SCHEME	PRIORITY	BACKEND	MIDDLEWARES	来源' "$routes"
expect_contains "列出模板生成路由" 'server:10000' "$routes"
expect_contains "带 h2c 方案" 'h2c' "$routes"
expect_contains "带显式优先级" '200' "$routes"
expect_contains "带来源 nsetup" 'nsetup' "$routes"
expect_contains "列出用户 label 路由" 'extra.example.com' "$routes"
expect_contains "label 路由带来源 labels" 'labels' "$routes"
expect_contains "label 路由带后端端口" 'dashboard:8080' "$routes"
expect_contains "label 路由带中间件" 'gzip@file' "$routes"

# 无路由项目必须给出明确结论，而不是静默输出空表。
cat >"$WORK/fixtures/noroutes.toml" <<'EOF'
format = 1
name = "noroutes"

[services.web]
image = "example/web"
version = "1.0"
EOF
E up -f "$WORK/fixtures/noroutes.toml" --force >/dev/null 2>&1
other=$(E show noroutes --routes 2>&1)
expect_contains "无路由项目给出明确结论" '该项目没有 Traefik 路由' "$other"
expect_contains "无路由项目仍显示项目信息" '项目名: noroutes' "$other"

# 项目名必须真的参与匹配：列出的是本项目路由，而不是别的同名服务。
docs=$(E show docs --routes 2>&1)
expect_contains "路由表按项目名解析" 'docs.test.example.com' "$docs"
