#!/usr/bin/env bash
# 懒猫应用商店自动同步发布（多应用合集版，应用配置在 apps/<应用名>/app.env）。
#
#   scripts/sync.sh <应用名>            常规：检测上游 release → 构建 → 冒烟 → 发布 → 落状态
#   FORCE=1  scripts/sync.sh <应用名>    已见过最新版也强制重走发布流程（补发）
#   DRY_RUN=1 scripts/sync.sh <应用名>   到构建+lint+冒烟为止：不登录、不发布、不落状态，
#                                       结束后还原工作区（本地演练用）
#
# 应用目录约定：
#   apps/<应用名>/app.env     bash 片段：UPSTREAM_REPO / PKG_ID 必填，IMAGE_REPO、
#                            MARKER_*、GATE_COMPONENT、LANG_MARKER_* 可选
#   apps/<应用名>/packaging/  lzc-build.yml / lzc-manifest.yml / package.yml / content / resources
#   apps/<应用名>/state.json  lastSeenTag / publishedVersion
#   apps/<应用名>/smoke.sh    可选冒烟脚本，参数：镜像引用（IMAGE_REPO 非空时才调用）、版本号
#
# 依赖: curl、python3、docker、git、lzc-cli（npm i -g @lazycatcloud/lzc-cli）
# 凭据: GITHUB_TOKEN（可选，API 限速兜底）、LAZYCAT_APPSTORE_USER / _PASS
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="${1:-}"
APP_DIR="$ROOT/apps/$APP"

log() { printf '\033[1;34m[sync]\033[0m %s\n' "$*" >&2; }
result() { log "RESULT: $*"; }
die() { log "失败: $*"; result "失败：$*"; exit 1; }

[ -n "$APP" ] || die "用法: scripts/sync.sh <应用名>"
[ -f "$APP_DIR/app.env" ] || die "apps/$APP/app.env 不存在"
# shellcheck source=/dev/null
source "$APP_DIR/app.env"
: "${UPSTREAM_REPO:?app.env 未定义 UPSTREAM_REPO}"
: "${PKG_ID:?app.env 未定义 PKG_ID}"
STATE="$APP_DIR/state.json"
[ -f "$STATE" ] || die "缺少 $STATE（参照 README 初始化）"
FORCE="${FORCE:-0}"
DRY_RUN="${DRY_RUN:-0}"
# IMAGE_MODE=registry（默认推荐）把镜像复制到懒猫官方仓库、lpk 只引用；
# IMAGE_MODE=embed 把镜像层内嵌进 lpk（体积大，且依赖构建机 docker 导出可靠）。
IMAGE_MODE="${IMAGE_MODE:-registry}"

# 导出给 plan.py 的 release 解析配置
export MARKER_BEGIN="${MARKER_BEGIN:-}"
export GATE_COMPONENT="${GATE_COMPONENT:-}"
export LANG_MARKER_ZH="${LANG_MARKER_ZH:-}"
export LANG_MARKER_EN="${LANG_MARKER_EN:-}"

RELEASE_JSON="$(mktemp)"

# ── 1. 上游最新稳定 release ──
log "查询 $UPSTREAM_REPO 最新 release"
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
fetch_release || die "GitHub API 请求失败"

# ── 2. 计划 ──
PLAN="$(python3 "$ROOT/scripts/plan.py" "$STATE" "$RELEASE_JSON" "$FORCE")" || die "计划解析失败"
plan_get() {
  printf '%s\n' "$PLAN" | awk -F= -v k="$1" '$1==k{sub(/^[^=]*=/,""); print; exit}'
}
ACTION="$(plan_get action)"
TAG="$(plan_get tag)"
VERSION="$(plan_get version)"
[ -n "$VERSION" ] || VERSION="${TAG#v}"

case "$ACTION" in
skip) result "无有效稳定 release"; exit 0 ;;
uptodate)
  log "商店已是最新（lastSeen=$TAG），无动作"
  result "已是最新（$TAG）"
  exit 0
  ;;
seen_only)
  log "$TAG 不含门控组件变更，只记录状态不发布"
  python3 - "$STATE" "$TAG" <<'PY'
import json, sys
p, t = sys.argv[1], sys.argv[2]
s = json.load(open(p))
s["lastSeenTag"] = t
s["lastAction"] = "seen-only"
json.dump(s, open(p, "w"), ensure_ascii=False, indent=2)
open(p, "a").write("\n")
PY
  result "$TAG 无组件变更，跳过"
  [ "$DRY_RUN" = "1" ] && { git -C "$ROOT" checkout -q -- state.json "apps/$APP" || true; exit 0; }
  git -C "$ROOT" add "$STATE"
  git -C "$ROOT" -c user.name="github-actions[bot]" \
    -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
    commit -q -m "chore($APP): 跳过 $TAG（无门控组件变更）"
  git -C "$ROOT" push -q || die "状态推送失败"
  exit 0
  ;;
publish) ;;
*) die "未知计划 $ACTION" ;;
esac

# ── 3. 镜像：校验存在（并作为冒烟对象） ──
if [ -n "${IMAGE_REPO:-}" ]; then
  log "拉取 $IMAGE_REPO:$VERSION"
  docker pull -q "$IMAGE_REPO:$VERSION" >/dev/null \
    || die "镜像 $IMAGE_REPO:$VERSION 拉取失败（上游 docker 构建未完成？）"
fi

# ── 3b. registry 模式：把镜像复制进懒猫官方仓库，lpk 只引用 ──
# 内嵌模式（embed）依赖构建机 `docker save` 的层完整性：GitHub runner 的
# docker 28 + BuildKit 曾稳定导出被截断的层，盒子上 `image load` 报
# `unexpected EOF`，安装失败。registry 模式完全绕开这条链路，且 lpk 从
# 62MB 降到 1MB（商店官方推荐做法）。
if [ -n "${IMAGE_REPO:-}" ] && [ "$IMAGE_MODE" = "registry" ]; then
  if [ "$DRY_RUN" = "1" ]; then
    log "DRY_RUN：跳过 copy-image，沿用 manifest 里已有的镜像引用"
  else
    log "登录商店账号（copy-image 需要）"
    lzc-cli appstore login -u "$LAZYCAT_APPSTORE_USER" -p "$LAZYCAT_APPSTORE_PASS" \
      || die "商店登录失败"
    log "复制镜像到懒猫官方仓库：$IMAGE_REPO:$VERSION"
    IMAGE_REF="$(lzc-cli appstore copy-image "$IMAGE_REPO:$VERSION" 2>&1 \
      | grep -oE 'registry\.lazycat\.cloud/[A-Za-z0-9._/-]+:[A-Za-z0-9._-]+' | tail -1)"
    [ -n "$IMAGE_REF" ] || die "copy-image 未返回官方仓库地址（镜像是否公网可拉取？）"
    log "官方镜像引用：$IMAGE_REF"
    python3 - "$APP_DIR/packaging/lzc-manifest.yml" "$IMAGE_REF" <<'PY'
import re, sys
path, ref = sys.argv[1], sys.argv[2]
text = open(path).read()
new, n = re.subn(r"^(\s*image:\s*)\S+\s*$", lambda m: m.group(1) + ref, text, count=1, flags=re.M)
if n != 1:
    raise SystemExit(f"lzc-manifest.yml 里找不到 image: 行，无法写入镜像引用")
open(path, "w").write(new)
PY
  fi
fi

# ── 3c. embed 模式：构建前预物化镜像层（降低 docker save 截断概率） ──
if [ -n "${IMAGE_REPO:-}" ] && [ "$IMAGE_MODE" = "embed" ]; then
  log "预物化镜像层（docker save → /dev/null）"
  docker image save "$IMAGE_REPO:$VERSION" > /dev/null || die "镜像层读取失败（存储异常）"
fi

# ── 4. 版本落盘 ──
log "更新 apps/$APP/packaging 配置到 $VERSION"
sed -i -E "s/^version: .*/version: $VERSION/" "$APP_DIR/packaging/package.yml"
if [ -n "${IMAGE_REPO:-}" ] && [ "$IMAGE_MODE" = "embed" ]; then
  sed -i -E "s#FROM $IMAGE_REPO:[^[:space:]]*#FROM $IMAGE_REPO:$VERSION#" "$APP_DIR/packaging/lzc-build.yml"
fi
grep -q "^version: $VERSION" "$APP_DIR/packaging/package.yml" || die "package.yml 版本写入失败"

# ── 5. 构建 + lint + 内嵌镜像完整性校验 ──
build_lpk() {
  cd "$APP_DIR/packaging"
  lzc-cli project build -o "$APP_DIR/$PKG_ID-v$VERSION.lpk"
}

log "lzc-cli 构建 lpk（builder: local）"
build_lpk || die "lpk 构建失败"
LPK="$APP_DIR/$PKG_ID-v$VERSION.lpk"
lzc-cli lpk lint "$LPK" || die "lpk lint 未通过（商店上架要求检查）"

# `lpk lint` 不检查内嵌镜像层是否完整；截断的层会让商店安装直接失败
# （盒子上 docker image load 报 unexpected EOF），必须自己把关。
if ! python3 "$ROOT/scripts/verify_lpk.py" "$LPK"; then
  log "内嵌镜像校验失败，重新构建一次（排除偶发截断）"
  build_lpk || die "lpk 重新构建失败"
  lzc-cli lpk lint "$LPK" >/dev/null || die "lpk lint 未通过（重试）"
  python3 "$ROOT/scripts/verify_lpk.py" "$LPK" || {
    log "诊断：docker $(docker version --format '{{.Server.Version}}' 2>/dev/null || echo '?')"
    docker info 2>/dev/null | grep -iE "storage driver|snapshotter" | head -3 >&2 || true
    die "内嵌镜像两次构建均损坏，已中止发布（不把坏包推给商店）"
  }
fi

# ── 6. 冒烟（apps/<应用名>/smoke.sh，可选） ──
if [ -n "${IMAGE_REPO:-}" ] && [ -f "$APP_DIR/smoke.sh" ]; then
  log "冒烟检查 $IMAGE_REPO:$VERSION"
  bash "$APP_DIR/smoke.sh" "$IMAGE_REPO:$VERSION" "$VERSION" || die "冒烟未通过"
  log "冒烟通过"
fi

# ── 7. DRY_RUN 到此为止 ──
if [ "$DRY_RUN" = "1" ]; then
  log "DRY_RUN：不发布，还原工作区"
  git -C "$ROOT" checkout -q -- "apps/$APP" || true
  exit 0
fi

# ── 8. 发布 ──
log "发布 $PKG_ID $VERSION 到商店"
if [ "$IMAGE_MODE" != "registry" ]; then
  lzc-cli appstore login -u "$LAZYCAT_APPSTORE_USER" -p "$LAZYCAT_APPSTORE_PASS" || die "商店登录失败"
fi
CH_ZH="$(plan_get changelog_zh)"
CH_EN="$(plan_get changelog_en)"
PUBLISH_ERR="$(mktemp)"
publish_once() { # 透传给 lzc-cli
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
git -C "$ROOT" add "$STATE" "$APP_DIR/packaging"
git -C "$ROOT" -c user.name="github-actions[bot]" \
  -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
  commit -q -m "chore($APP): 发布 $PKG_ID $VERSION（上游 $TAG）" || true
git -C "$ROOT" push -q || die "状态推送失败（商店已收包，下轮将命中「版本已存在」兜底）"
result "已提交 $VERSION 审核"
log "完成"
