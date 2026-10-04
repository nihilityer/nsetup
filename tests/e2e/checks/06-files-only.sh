# R7：--files-only 只同步资源，项目目录 inode 不变（容器无需重建）。
echo "=== R7 --files-only 只同步资源 ==="

write_static_toml "$WORK/fixtures/static.toml"
write_assets "$WORK/fixtures/assets"
write_files "$WORK/fixtures/files"

E up -f "$WORK/fixtures/static.toml" --assets "$WORK/fixtures/assets" \
  --files "$WORK/fixtures/files" --force >/dev/null 2>&1
project="$WORK/stacks/docs"
before_inode=$(file_inode "$project")
before_compose=$(cat "$project/compose.yaml")

printf 'server: {}\n' >"$WORK/fixtures/files/updated.yaml"
out=$(E up -f "$WORK/fixtures/static.toml" --assets "$WORK/fixtures/assets" \
  --files "$WORK/fixtures/files" --files-only 2>&1)
expect_contains "报告容器未重建" '容器未重建' "$out"

after_inode=$(file_inode "$project")
if [ "$before_inode" = "$after_inode" ]; then
  printf 'PASS  %s\n' "项目目录 inode 保持不变"
  PASS=$((PASS + 1))
else
  printf 'FAIL  %s（%s -> %s）\n' "项目目录 inode 保持不变" "$before_inode" "$after_inode"
  FAIL=$((FAIL + 1))
fi

# 运行中容器不需要重建，因此 compose.yaml 与 .env 都不应被重写，钩子也不执行。
if [ "$before_compose" = "$(cat "$project/compose.yaml")" ]; then
  printf 'PASS  %s\n' "files-only 不重写 compose.yaml"
  PASS=$((PASS + 1))
else
  printf 'FAIL  %s\n' "files-only 不重写 compose.yaml"
  FAIL=$((FAIL + 1))
fi
check "新文件已落到项目目录" test -f "$project/files/updated.yaml"
expect_mode "files-only 写入仍用 a+rX" "$project/files/updated.yaml" 644

# 未部署项目不能用 files-only 造出半成品目录。
cat >"$WORK/fixtures/undeployed.toml" <<'EOF'
format = 1
name = "undeployed"

[services.web]
image = "example/web"
version = "1.0"
EOF
expect_fail "未部署项目拒绝 files-only" E up -f "$WORK/fixtures/undeployed.toml" \
  --files "$WORK/fixtures/files" --files-only
check "拒绝后未创建项目目录" test ! -e "$WORK/stacks/undeployed"

# 未提供任何资源时明确报错，而不是空跑一次 deploy。
empty_out=$(E up -f "$WORK/fixtures/static.toml" --files-only 2>&1)
expect_contains "缺少资源时给出明确报错" '--files-only' "$empty_out"
