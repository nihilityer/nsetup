# R4/R5：OIDC 变更文案与实际重启一致；钩子输出可见、失败时报容器状态。
echo "=== R4/R5 钩子语义与 OIDC 文案 ==="

# 钩子成功时 stdout 必须回显。
hook_out=$(E up -f "$WORK/fixtures/static.toml" --assets "$WORK/fixtures/assets" \
  --files "$WORK/fixtures/files" --force 2>&1)
expect_contains "钩子成功时回显 stdout" 'pre_start 钩子输出' "$hook_out"

# 钩子失败时错误里要带退出码、stderr 与容器状态。
write_failing_hook_toml "$WORK/fixtures/hookfail.toml"
hook_err=$(E up -f "$WORK/fixtures/hookfail.toml" --force 2>&1)
expect_contains "钩子失败含退出码" '退出码' "$hook_err"
expect_contains "钩子失败回显 stdout" '钩子诊断行' "$hook_err"
expect_contains "钩子失败回显 stderr" '钩子失败原因' "$hook_err"
expect_contains "钩子失败说明容器状态" '容器' "$hook_err"
expect_contains "钩子失败给出补救命令" 'nsetup up' "$hook_err"

# 钩子失败不得静默成功：项目状态可能已写入，但命令必须返回非零。
expect_fail "钩子失败使 up 返回非零" E up -f "$WORK/fixtures/hookfail.toml" --force

# R4：声明 OIDC 客户端后，authelia 片段更新并真的重启，文案必须说明已重启。
write_oidc_app_toml "$WORK/fixtures/oidcapp.toml"
oidc_out=$(E up -f "$WORK/fixtures/oidcapp.toml" --force 2>&1)
expect_contains "OIDC 变更提示已重启" '已重启 authelia' "$oidc_out"
if grep -qF '重启 authelia 后生效' <<<"$oidc_out"; then
  printf 'FAIL  %s\n' "已重启时不得提示「重启 authelia 后生效」"
  echo "----- 实际输出 -----"
  echo "$oidc_out"
  echo "--------------------"
  FAIL=$((FAIL + 1))
else
  printf 'PASS  %s\n' "已重启时不得提示「重启 authelia 后生效」"
  PASS=$((PASS + 1))
fi
fragment="$WORK/stacks/authelia/config/oidc-clients/oidcapp.yml"
check "OIDC 片段已写入" test -s "$fragment"
expect_contains "片段包含 client_id" 'client_id: oidcapp' "$(cat "$fragment")"
expect_contains "公共客户端不写 client_secret" "client_secret: ''" "$(cat "$fragment")"
expect_contains "require_pkce 输出 S256" 'pkce_challenge_method: S256' "$(cat "$fragment")"
