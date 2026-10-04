# R8：Authelia 的 /config 必须可写；OIDC 片段必须原地重写且容器内可读。
#
# 官方镜像 entrypoint 会执行 `chown -R ${PUID}:${PGID} /config`（镜像默认 0:0），
# 只读挂载会让它每次启动都往容器日志写 `chown: ... Read-only file system`。
# 本检查在最后运行：容器会以 root 改写工作目录内配置的属主，结束时已恢复。
echo "=== R8 Authelia 配置挂载与片段写入 ==="

authelia_compose="$WORK/stacks/authelia/compose.yaml"
# `/config` 必须可写：只读挂载会让官方 entrypoint 的 chown 持续报 EROFS。
check "/config 以可写方式挂载" grep -qE "authelia/config:/config$" "$authelia_compose"
if grep -qF "authelia/config:/config:ro" "$authelia_compose"; then
  printf 'FAIL  %s\n' "/config 不得带 :ro"
  FAIL=$((FAIL + 1))
else
  printf 'PASS  %s\n' "/config 不得带 :ro"
  PASS=$((PASS + 1))
fi
expect_contains "secrets 目录保持只读" "authelia/secrets:/secrets:ro" "$(cat "$authelia_compose")"

# OIDC 片段原地重写：重新应用前后 inode 必须一致（改名会换掉 inode 与属主）。
write_oidc_app_toml "$WORK/fixtures/oidcapp.toml"
E up -f "$WORK/fixtures/oidcapp.toml" --force >/dev/null 2>&1
fragment="$WORK/stacks/authelia/config/oidc-clients/oidcapp.yml"
check "OIDC 片段存在" test -s "$fragment"
inode_before=$(file_inode "$fragment")
sed -i 's/client_name = "OIDC App"/client_name = "OIDC App v2"/' "$WORK/fixtures/oidcapp.toml"
E up -f "$WORK/fixtures/oidcapp.toml" --force >/dev/null 2>&1
inode_after=$(file_inode "$fragment")
if [ "$inode_before" = "$inode_after" ]; then
  printf 'PASS  %s\n' "OIDC 片段原地重写（inode 不变）"
  PASS=$((PASS + 1))
else
  printf 'FAIL  %s（%s -> %s）\n' "OIDC 片段原地重写（inode 不变）" "$inode_before" "$inode_after"
  FAIL=$((FAIL + 1))
fi
expect_contains "片段内容已更新" 'OIDC App v2' "$(cat "$fragment")"
expect_mode "片段权限保持 0640" "$fragment" 640
expect_fail "原地重写不留孤儿临时文件" bash -c "ls '$WORK/stacks/authelia/config/oidc-clients/' | grep -q replace"

# 容器内可读性：以只读方式挂载配置，模拟 Authelia 进程读取片段。
check "片段在容器内可读" docker run --rm \
  -v "$WORK/stacks/authelia/config:/config:ro" alpine:3.23 \
  head -1 /config/oidc-clients/oidcapp.yml

# 真实镜像对照：可写挂载不应出现 chown 只读报错。
if docker image inspect authelia/authelia:4.39.20 >/dev/null 2>&1; then
  error_count=$(timeout 15 docker run --rm \
    -v "$WORK/stacks/authelia/config:/config" \
    authelia/authelia:4.39.20 --config /config/configuration.yml 2>&1 \
    | grep -c 'Read-only file system' || true)
  if [ "$error_count" -eq 0 ]; then
    printf 'PASS  %s\n' "可写挂载下无 chown 只读报错"
    PASS=$((PASS + 1))
  else
    printf 'FAIL  %s（出现 %s 次）\n' "可写挂载下无 chown 只读报错" "$error_count"
    FAIL=$((FAIL + 1))
  fi
  # 容器 entrypoint 会把配置目录 chown 成镜像默认的 PUID:PGID，这里恢复属主，
  # 保证测试目录本身不留下 root 属主。
  docker run --rm -v "$WORK/stacks/authelia:/fix" alpine:3.23 \
    chown -R "$(id -u):$(id -g)" /fix >/dev/null 2>&1
else
  skip "可写挂载下无 chown 只读报错（缺少本地镜像 authelia/authelia:4.39.20）"
fi
