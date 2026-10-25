#!/usr/bin/env bash
# FluxDown 冒烟检查（由 scripts/sync.sh 调用）：
#   $1 = 镜像引用（ghcr.io/zerx-lab/fluxdown-server:<版本>）
#   $2 = 期望版本号
# 通过标准：二进制内含 access-key（Web 登录自动填充选择器哨兵）+ 容器探活 /ping 版本匹配。
set -euo pipefail
IMAGE="$1"; VERSION="$2"

docker run --rm --entrypoint sh "$IMAGE" \
  -c 'grep -aq access-key /app/fluxdown-agent' \
  || { echo "二进制内找不到 access-key：Web 登录自动填充选择器可能已变，需人工核对 apps/fluxdown/packaging/lzc-manifest.yml 注入" >&2; exit 1; }

docker rm -f fluxdown-smoke >/dev/null 2>&1 || true
trap 'docker rm -f fluxdown-smoke >/dev/null 2>&1 || true' EXIT
docker run -d --rm --name fluxdown-smoke -p 127.0.0.1:17899:17800 "$IMAGE" >/dev/null
for _ in $(seq 1 30); do
  if curl -fsS -m 3 http://127.0.0.1:17899/ping 2>/dev/null | grep -q "\"version\":\"$VERSION\""; then
    exit 0
  fi
  sleep 2
done
echo "/ping 30 次轮询后仍未返回版本 $VERSION" >&2
exit 1
