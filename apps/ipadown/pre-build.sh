#!/usr/bin/env bash
# apps/ipadown/pre-build.sh — ipaDown 跟随 ipatool 上游的发布前钩子（sync.sh 在构建前调用）。
#
# 输入环境变量：
#   UPSTREAM_TAG     上游（majd/ipatool）release tag，如 v2.8.0
#   UPSTREAM_VERSION 去 v 前缀的上游版本号
#   APP_DIR          应用目录（packaging/ 与 state.json 所在）
#   RELEASE_JSON     sync.sh 已拉取的上游 release 元数据（从中取资产 id，经
#                    api.github.com 下载校验和，避开 github.com 资产域的黑洞路由）
#
# 职责：
#   1. 应用自身版本号 patch+1（以 state.json 的 publishedVersion 为基准，
#      而不是 package.yml —— sync.sh 此刻已把上游版本号 sed 进去了）
#   2. Dockerfile：ARG IPATOOL_VERSION 与 amd64/arm64 两个 sha256 校验和
#      （从上游 release 的 .sha256sum 资产取，缺失即失败，下轮重试）
#   3. 改写 /tmp/changelog.{zh,en}.md 为「升级内置 ipatool」导向的中英文案
#   4. 把最终版本号写入 $APP_DIR/.version（sync.sh 回读用于 lpk 命名与状态落盘）
set -euo pipefail

TAG="${UPSTREAM_TAG:?}"
UPVER="${UPSTREAM_VERSION:?}"
: "${APP_DIR:?}"
: "${RELEASE_JSON:?}"
PKG_DIR="$APP_DIR/packaging"
STATE="$APP_DIR/state.json"

# ── 1. 自身版本号 patch+1 ──
BASE="$(python3 -c "import json;print(json.load(open('$STATE'))['publishedVersion'])")"
NEW_VERSION="$(python3 - "$BASE" <<'PY'
import sys
major, minor, patch = sys.argv[1].split(".")
print(f"{major}.{minor}.{int(patch) + 1}")
PY
)"
sed -i -E "s/^version: .*/version: $NEW_VERSION/" "$PKG_DIR/package.yml"
grep -q "^version: $NEW_VERSION" "$PKG_DIR/package.yml" || { echo "pre-build: 版本号写入失败" >&2; exit 1; }

# ── 2. Dockerfile 跟随上游 ──
sed -i -E "s/^ARG IPATOOL_VERSION=.*/ARG IPATOOL_VERSION=$UPVER/" "$PKG_DIR/Dockerfile"

asset_sha() { # $1 = arch；从 RELEASE_JSON 取资产 id，经 API 下载校验和（间歇性 TLS 黑洞，长重试窗口够过浪）
  local name="ipatool-$UPVER-linux-$1.tar.gz.sha256sum" id sha attempt
  id="$(python3 -c "import json;r=json.load(open('$RELEASE_JSON'));print(next((a['id'] for a in r['assets'] if a['name']=='$name'), ''))")" \
    || return 1
  [ -n "$id" ] || return 1
  local auth=()
  [ -n "${GITHUB_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
  for attempt in 1 2 3 4 5 6; do
    sha="$(curl -fsSL --max-time 30 "${auth[@]}" \
      -H "Accept: application/octet-stream" \
      "https://api.github.com/repos/majd/ipatool/releases/assets/$id" | cut -d' ' -f1)" && break
    sleep $((attempt * 5))
  done
  printf '%s' "$sha" | grep -qE '^[0-9a-f]{64}$' || return 1
  printf '%s' "$sha"
}

for arch in amd64 arm64; do
  sha="$(asset_sha "$arch")" \
    || { echo "pre-build: 上游 $arch 校验和获取失败" >&2; exit 1; }
  sed -i -E "s#^(\s*${arch}\)) expected=\"[0-9a-f]{64}\"#\1 expected=\"$sha\"#" "$PKG_DIR/Dockerfile"
done
grep -q "^ARG IPATOOL_VERSION=$UPVER" "$PKG_DIR/Dockerfile" || { echo "pre-build: IPATOOL_VERSION 写入失败" >&2; exit 1; }

# ── 3. 更新日志（ipatool 原文 → 中英导向文案） ──
NOTES="$(cat /tmp/changelog.en.md 2>/dev/null || true)"
cat > /tmp/changelog.zh.md <<EOF
升级内置 ipatool 至 v$UPVER，跟进 App Store 下载协议的修复与改进（登录兼容、下载可靠性等）。
上游更新说明：
$NOTES
EOF
cat > /tmp/changelog.en.md <<EOF
Update the bundled ipatool to v$UPVER, following upstream App Store download protocol fixes and improvements (login compatibility, download reliability, etc.).
Upstream release notes:
$NOTES
EOF

# ── 4. 最终版本号交还 sync.sh ──
printf '%s' "$NEW_VERSION" > "$APP_DIR/.version"
echo "pre-build: ipaDown $NEW_VERSION（内置 ipatool v$UPVER）" >&2
