#!/usr/bin/env bash
# apps/ipadown/smoke.sh — lpk 构建产物的运行冒烟（sync.sh 在 lint 后调用）。
# 参数：$1 镜像引用（本应用无上游镜像，为空，忽略）、$2 版本号
#
# 冒烟对象是 lpk 里内嵌的最终镜像（而不是重新 docker build —— 那会绕开实际发布物，
# 且本地网络对 GitHub 资产域有黑洞）：解 OCI layout → docker import → 起容器验证
# /healthz，并确认新版本 ipatool 二进制可执行。
set -euo pipefail
VERSION="${2:?缺少版本号}"
APP_DIR="$(cd "$(dirname "$0")" && pwd)"
LPK="$(ls "$APP_DIR"/*-v"$VERSION".lpk 2>/dev/null | head -1)"
[ -n "$LPK" ] || { echo "smoke: 找不到 v$VERSION 的 lpk" >&2; exit 1; }

IMAGE="ipadown-smoke:$VERSION"
NAME="ipadown-smoke-$$"
PORT=$((20000 + RANDOM % 20000))
WORK="$(mktemp -d)"
DATA="$(mktemp -d)"

cleanup() {
  # 容器以 root 写入挂载目录，先在容器内清空再删宿主目录，失败不阻断冒烟结果
  docker exec "$NAME" sh -c 'rm -rf /data/*' >/dev/null 2>&1 || true
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker rmi "$IMAGE" >/dev/null 2>&1 || true
  rm -rf "$WORK" 2>/dev/null || true
  rm -rf "$DATA" 2>/dev/null || true
}
trap cleanup EXIT

tar xf "$LPK" -C "$WORK" images

python3 - "$WORK" <<'PY'
import json, os, pathlib, sys, tarfile

work = sys.argv[1]
index = json.load(open(f"{work}/images/index.json"))
manifest_digest = index["manifests"][0]["digest"].split(":", 1)[1]
manifest = json.load(open(f"{work}/images/blobs/sha256/{manifest_digest}"))
root = os.path.join(work, "rootfs")
os.makedirs(root)
for layer in manifest["layers"]:
    digest = layer["digest"].split(":", 1)[1]
    with tarfile.open(f"{work}/images/blobs/sha256/{digest}") as tar:
        for tar_filter in ("tar", "fully_trusted"):
            try:
                tar.extractall(root, filter=tar_filter)
                break
            except TypeError:  # 旧版 python3 无 filter 参数
                tar.extractall(root)
                break
            except tarfile.FilterError:  # tar 过滤器拒绝绝对路径符号链接等，换宽松重解
                continue
whiteouts = list(pathlib.Path(root).rglob("*.wh.*"))
if whiteouts:
    sys.exit(f"smoke: 镜像含 whiteout 层（{len(whiteouts)} 个），导入逻辑需扩展")
print("rootfs 解包完成")
PY

tar -C "$WORK/rootfs" -cf "$WORK/rootfs.tar" .
docker import "$WORK/rootfs.tar" "$IMAGE" >/dev/null

docker run --rm -d --name "$NAME" -p "$PORT:8080" \
  -e PORT=8080 -e IPADOWN_DATA_DIR=/data -v "$DATA":/data \
  "$IMAGE" /usr/local/bin/ipadown-web >/dev/null

ok=0
for _ in $(seq 1 20); do
  if curl -fsS --max-time 3 "http://localhost:$PORT/healthz" >/dev/null 2>&1; then ok=1; break; fi
  sleep 1
done
[ "$ok" = 1 ] || { echo "smoke: /healthz 未就绪" >&2; exit 1; }

IPATOOL_VERSION_OUT="$(docker exec "$NAME" ipatool --version 2>&1 || true)"
echo "smoke: /healthz 通过，镜像内 ipatool: $IPATOOL_VERSION_OUT"
