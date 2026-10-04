# R2：--assets/--files 上传后的权限，以及 static 模板的 hooks / user / group_add。
#
# 0.2.0 的回归点是上传目录固定 0750，容器内非 root 进程（nginx worker uid 101）
# 无法穿行；这里同时校验默认 a+rX 与 --assets-perms private 两种策略。
echo "=== R2 上传资源权限与 static 模板能力 ==="

write_assets "$WORK/fixtures/assets"
write_files "$WORK/fixtures/files"
write_static_toml "$WORK/fixtures/static.toml"

E up -f "$WORK/fixtures/static.toml" --assets "$WORK/fixtures/assets" \
  --files "$WORK/fixtures/files" --force >/dev/null 2>&1

site="$WORK/stacks/docs/site"
files="$WORK/stacks/docs/files"
expect_mode "site 目录为 0755（a+rX）" "$site" 755
expect_mode "site 文件为 0644" "$site/index.html" 644
expect_mode "files 目录为 0755" "$files" 755
expect_mode "files 子目录为 0755" "$files/sub" 755
expect_mode "files 文件为 0644" "$files/sub/b.yaml" 644
expect_contains "static 模板接受 group_add" '988' "$(cat "$WORK/stacks/docs/compose.yaml")"
expect_contains "static 模板保留 hooks" 'ls -l site' "$(cat "$WORK/stacks/docs/.env")"

E up -f "$WORK/fixtures/static.toml" --assets "$WORK/fixtures/assets" \
  --assets-perms private --assets-mode replace --force >/dev/null 2>&1
expect_mode "--assets-perms private 目录为 0750" "$site" 750
expect_mode "--assets-perms private 文件为 0640" "$site/index.html" 640

# replace 语义：已下线的旧文件必须被删除。
printf 'old\n' >"$WORK/fixtures/assets/obsolete.html"
E up -f "$WORK/fixtures/static.toml" --assets "$WORK/fixtures/assets" \
  --assets-perms private --assets-mode replace --force >/dev/null 2>&1
rm -f "$WORK/fixtures/assets/obsolete.html"
E up -f "$WORK/fixtures/static.toml" --assets "$WORK/fixtures/assets" \
  --assets-perms private --assets-mode replace --force >/dev/null 2>&1
expect_fail "replace 模式删除已下线文件" test -f "$site/obsolete.html"

# merge 语义：与既有部署合并 —— 既有文件保留，同名文件被上传内容覆盖。
# 受管 site/ 目录在每次部署时整体重写，因此先用一次 replace 部署“旧站点”，
# 再用 merge 部署“新站点”，既有文件才会保留下来。
site="$WORK/stacks/docs/site"
rm -rf "$WORK/fixtures/assets-old"
mkdir -p "$WORK/fixtures/assets-old"
printf 'old index\n' >"$WORK/fixtures/assets-old/index.html"
printf 'keep\n' >"$WORK/fixtures/assets-old/keep.html"
E up -f "$WORK/fixtures/static.toml" --assets "$WORK/fixtures/assets-old" \
  --assets-mode replace --force >/dev/null 2>&1
E up -f "$WORK/fixtures/static.toml" --assets "$WORK/fixtures/assets" \
  --assets-mode merge --force >/dev/null 2>&1
check "merge 模式保留未上传的既有文件" test -f "$site/keep.html"
expect_contains "merge 模式覆盖同名文件" '<h1>hi</h1>' "$(cat "$site/index.html")"
