# R6：volumes 支持相对项目目录的挂载源，命名卷与越界路径仍被拒绝。
echo "=== R6 相对项目目录的挂载源 ==="

write_relative_mounts_toml "$WORK/fixtures/relmount.toml"
printf 'server: {}\n' >"$WORK/fixtures/files/config.yaml"

check "相对挂载源可应用" E up -f "$WORK/fixtures/relmount.toml" --files "$WORK/fixtures/files" --force
relmount_compose="$WORK/stacks/relmount/compose.yaml"
expect_contains "相对源展开为项目内绝对路径" "$WORK/stacks/relmount/files/config.yaml" "$(cat "$relmount_compose")"
expect_contains "相对目录挂载同样展开" "$WORK/stacks/relmount/files:/opt/rel:ro" "$(cat "$relmount_compose")"
# 展开结果不能残留 `./`，否则 Compose 会把它当命名卷。
if grep -qF "/./" "$relmount_compose"; then
  printf 'FAIL  %s\n' "挂载路径不残留 ./ 分量"
  FAIL=$((FAIL + 1))
else
  printf 'PASS  %s\n' "挂载路径不残留 ./ 分量"
  PASS=$((PASS + 1))
fi

# export 必须过滤掉 --files 注入的挂载，否则重新应用会重复挂载。
rm -f "$WORK/fixtures/relmount-export.toml"
E export relmount -o "$WORK/fixtures/relmount-export.toml"
if grep -qF '/opt/nsetup/files' "$WORK/fixtures/relmount-export.toml"; then
  printf 'FAIL  %s\n' "export 过滤 --files 注入挂载"
  FAIL=$((FAIL + 1))
else
  printf 'PASS  %s\n' "export 过滤 --files 注入挂载"
  PASS=$((PASS + 1))
fi
expect_contains "export 保留用户自己的相对源" 'files/config.yaml' "$(cat "$WORK/fixtures/relmount-export.toml")"
check "export 结果可重新应用" E up -f "$WORK/fixtures/relmount-export.toml" --files "$WORK/fixtures/files" --force

# 反例：命名卷与越出项目目录的相对路径。
write_named_volume_toml "$WORK/fixtures/named-volume.toml"
expect_fail "命名卷仍被拒绝" E up -f "$WORK/fixtures/named-volume.toml" --force
sed 's|"mydata:/data"|"../outside:/data"|' "$WORK/fixtures/named-volume.toml" >"$WORK/fixtures/escape.toml"
expect_fail "越出项目目录的相对路径被拒绝" E up -f "$WORK/fixtures/escape.toml" --force

# 绝对路径（data_roots 内）继续可用。
mkdir -p "$WORK/data/absdemo"
cat >"$WORK/fixtures/absmount.toml" <<EOF
format = 1
name = "absmount"

[services.web]
image = "example/web"
version = "1.0"
volumes = ["$WORK/data/absdemo:/data"]
EOF
check "data_roots 绝对路径继续可用" E up -f "$WORK/fixtures/absmount.toml" --force
