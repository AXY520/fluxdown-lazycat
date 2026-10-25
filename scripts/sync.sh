#!/usr/bin/env bash
# FluxDown → 懒猫应用商店 自动同步发布。
#
#   FORCE=1     即使 lastSeenTag 已等于最新 tag，也强制重新走发布流程
#   DRY_RUN=1    走到「构建 + lint + 冒烟」为止：不登录商店、不发布、不落状态，
#               结束后还原工作区（用于本地 / 手动验证）
#
# 依赖: curl、python3、docker、git、lzc-cli（npm i -g @lazycatcloud/lzc-cli）
# 凭据: GITHUB_TOKEN（公共 API 限速兜底，可空）、LAZYCAT_APPSTORE_USER / _PASS
set -euo pipefail

UPSTREAM_REPO="${UPSTREAM_REPO:-zerx-lab/FluxDown}"
IMAGE_REPO="ghcr.io/zerx-lab/fluxdown-server" # 必须与 packaging/lzc-build.yml 的 FROM 一致
PKG_ID="cloud.lazycat.app.fluxdown"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STATE="$ROOT/state.json"
RELEASE_JSON="$(mktemp)"
FORCE="${FORCE:-0}"
DRY_RUN="${DRY_RUN:-0}"

log() { printf '\033[1;34m[sync]\033[0m %s\n' "$*" >&2; }
die() { log "失败: $*"; exit 1; }

API_URL="https://api.github.com/repos/$UPSTREAM_REPO/releases/latest"
fetch_release() {
  local auth=()
  [ -n "${GITHUB_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
  if curl -fsS --retry 2 --retry-all-errors --retry-delay 2 "${auth[@]}" \
    -H "Accept: application/vnd.github+json" -o "$RELEASE_JSON" "$API_URL"; then
    return 0
  fi
  # 本地网络兜底：部分路由器把 GitHub 域名改写到加速/假 IP，偶发黑洞；
  # 用公共 DNS 拿真实 IP 直连（CI 网络干净，不会走到这一步）。
  if command -v dig >/dev/null 2>&1; then
    local ip
    ip="$(dig +short +time=3 +tries=1 api.github.com @223.5.5.5 2>/dev/null | grep -E '^[0-9]+\.' | tail -1 || true)"
    if [ -n "$ip" ]; then
      curl -fsS --retry 3 --retry-all-errors --retry-delay 2 "${auth[@]}" \
        -H "Accept: application/vnd.github+json" \
        --resolve "api.github.com:443:$ip" -o "$RELEASE_JSON" "$API_URL" && return 0
    fi
  fi
  return 1
}

# ── 1. 上游最新稳定 release ──
log "查询 $UPSTREAM_REPO 最新 release"
fetch_release || die "GitHub API 请求失败"

# ── 2. 计划 ──
PLAN="$(python3 "$ROOT/scripts/plan.py" "$STATE" "$RELEASE_JSON" "$FORCE")" || die "计划解析失败"
plan_get() { # 从计划中取 key=value 的 value
  printf '%s\n' "$PLAN" | awk -F= -v k="$1" '$1==k{sub(/^[^=]*=/,""); print; exit}'
}
ACTION="$(plan_get action)"
TAG="$(plan_get tag)"
VERSION="$(plan_get version)"
[ -n "$VERSION" ] || VERSION="${TAG#v}"

case "$ACTION" in
skip) log "无有效稳定 release"; exit 0 ;;
uptodate) log "商店已是最新（lastSeen=$TAG），无动作"; exit 0 ;;
seen_only)
  log "$TAG 不含 server 组件变更，只记录状态不发布"
  python3 - "$STATE" "$TAG" <<'PY'
import json, sys
p, t = sys.argv[1], sys.argv[2]
s = json.load(open(p))
s["lastSeenTag"] = t
s["lastAction"] = "seen-only"
json.dump(s, open(p, "w"), ensure_ascii=False, indent=2)
open(p, "a").write("\n")
PY
  [ "$DRY_RUN" = "1" ] && { git -C "$ROOT" checkout -q -- state.json; exit 0; }
  git -C "$ROOT" add state.json
  git -C "$ROOT" -c user.name="github-actions[bot]" \
    -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
    commit -q -m "chore: 跳过 $TAG（无 server 组件变更）"
  git -C "$ROOT" push -q || die "状态推送失败"
  exit 0
  ;;
publish) ;;
*) die "未知计划 $ACTION" ;;
esac

# ── 3. 镜像拉取（存在性校验 + 本地构建依赖；上游 docker 未发布则本轮终止，下轮重试） ──
log "拉取 $IMAGE_REPO:$VERSION"
docker pull -q "$IMAGE_REPO:$VERSION" >/dev/null \
  || die "镜像 $IMAGE_REPO:$VERSION 拉取失败（上游 docker 构建未完成？）"

# ── 4. 版本落盘 ──
log "更新 packaging 配置到 $VERSION"
sed -i -E "s/^version: .*/version: $VERSION/" "$ROOT/packaging/package.yml"
sed -i -E "s#FROM $IMAGE_REPO:[^[:space:]]*#FROM $IMAGE_REPO:$VERSION#" "$ROOT/packaging/lzc-build.yml"
grep -q "^version: $VERSION" "$ROOT/packaging/package.yml" || die "package.yml 版本写入失败"

# ── 5. 构建 + lint ──
cd "$ROOT/packaging"
log "lzc-cli 构建 lpk（builder: local）"
lzc-cli project build -o "$ROOT/$PKG_ID-v$VERSION.lpk" || die "lpk 构建失败"
LPK="$ROOT/$PKG_ID-v$VERSION.lpk"
lzc-cli lpk lint "$LPK" || die "lpk lint 未通过（商店上架要求检查）"

# ── 6. 冒烟：镜像能起、版本正确、登录自动填充选择器仍在 ──
log "冒烟检查 $IMAGE_REPO:$VERSION"
docker run --rm --entrypoint sh "$IMAGE_REPO:$VERSION" \
  -c 'grep -aq access-key /app/fluxdown-agent' \
  || die "冒烟失败：二进制内找不到 access-key（Web 登录自动填充选择器可能已变，需人工核对 lzc-manifest.yml 注入）"
docker rm -f fluxdown-smoke >/dev/null 2>&1 || true
docker run -d --rm --name fluxdown-smoke -p 127.0.0.1:17899:17800 "$IMAGE_REPO:$VERSION" >/dev/null
trap 'docker rm -f fluxdown-smoke >/dev/null 2>&1 || true' EXIT
SMOKE_OK=""
for _ in $(seq 1 30); do
  if curl -fsS -m 3 http://127.0.0.1:17899/ping 2>/dev/null | grep -q "\"version\":\"$VERSION\""; then
    SMOKE_OK=1; break
  fi
  sleep 2
done
[ -n "$SMOKE_OK" ] || die "冒烟失败：/ping 未返回版本 $VERSION"
docker rm -f fluxdown-smoke >/dev/null 2>&1 || true
trap - EXIT
log "冒烟通过（探活 + 版本 + access-key）"

# ── 7. DRY_RUN 到此为止 ──
if [ "$DRY_RUN" = "1" ]; then
  log "DRY_RUN：不发布，还原工作区"
  git -C "$ROOT" checkout -q -- packaging state.json || true
  exit 0
fi

# ── 8. 发布 ──
log "登录商店并发布 $PKG_ID $VERSION"
lzc-cli appstore login -u "$LAZYCAT_APPSTORE_USER" -p "$LAZYCAT_APPSTORE_PASS" || die "商店登录失败"
CH_ZH="$(plan_get changelog_zh)"
CH_EN="$(plan_get changelog_en)"
PUBLISH_ERR="$(mktemp)"
publish_once() { # $1..= 透传给 lzc-cli
  lzc-cli appstore publish "$LPK" "$@" 2> "$PUBLISH_ERR"
}
is_duplicate() { grep -qiE "存在|重复|exists|duplicate" "$PUBLISH_ERR"; }

if ! publish_once --clangs "zh:$CH_ZH" --clangs "en:$CH_EN"; then
  if is_duplicate; then
    log "商店提示该版本已存在，视为已发布"
  elif ! publish_once --clang zh -c "$(cat "$CH_ZH")"; then
    if is_duplicate; then
      log "商店提示该版本已存在，视为已发布"
    else
      cat "$PUBLISH_ERR" >&2
      die "商店发布失败"
    fi
  fi
fi
log "发布提交成功：$PKG_ID $VERSION（等待商店审核）"

# ── 9. 状态落盘 + 推送 ──
python3 - "$STATE" "$TAG" "$VERSION" <<'PY'
import json, sys
p, t, v = sys.argv[1], sys.argv[2], sys.argv[3]
s = json.load(open(p))
s["lastSeenTag"] = t
s["publishedVersion"] = v
s["lastAction"] = "publish"
json.dump(s, open(p, "w"), ensure_ascii=False, indent=2)
open(p, "a").write("\n")
PY
git -C "$ROOT" add -A
git -C "$ROOT" -c user.name="github-actions[bot]" \
  -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
  commit -q -m "chore: 发布 $PKG_ID $VERSION（上游 $TAG）" || true
git -C "$ROOT" push -q || die "状态推送失败（商店已收包，下轮将命中「版本已存在」兜底）"
log "完成"
