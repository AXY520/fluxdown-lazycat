#!/usr/bin/env python3
"""校验 lpk 内嵌镜像（images/ 的 OCI 布局）是否完整可用。

存在的意义：lpk 构建（docker save → 打包）曾在 CI 上产出过**截断的层 blob**——
manifest 尺寸对得上、gzip 头尾也完整，但解压出的 tar 在 1.7MB 处断掉，
导致盒子上 `docker image load` 报 `unexpected EOF`，安装失败。这类损坏在
`lpk lint` 眼里是合法的，只有逐层校验才能拦住。

校验项：
  1. images/index.json → manifest → config 三层引用齐备，尺寸匹配
  2. 每个层 blob 的压缩后尺寸与 manifest 一致
  3. 解压后的 sha256 与 config 的 rootfs.diff_ids 一致（docker 应用层的前提）
  4. tar 能完整读到 EOF（截断 tar 在这里暴露）

用法: verify_lpk.py <lpk 路径>
退出码: 0 通过 / 1 损坏（stderr 给出具体层与原因）
"""

import gzip
import hashlib
import io
import json
import sys
import tarfile
import tempfile
from pathlib import Path


def read_bytes(archive: tarfile.TarFile, name: str) -> bytes:
    member = archive.extractfile(name)
    if member is None:
        raise FileNotFoundError(f"lpk 内缺少 images/{name}")
    return member.read()


def verify_blob(path: Path, digest: str, declared_size: int, expect_diff_id: str) -> None:
    actual_size = path.stat().st_size
    if actual_size != declared_size:
        raise ValueError(
            f"层 {digest[:19]} 尺寸不符：manifest 声明 {declared_size}，实际 {actual_size}"
        )

    raw = path.read_bytes()
    try:
        data = gzip.decompress(raw)
    except OSError as err:
        raise ValueError(f"层 {digest[:19]} gzip 解压失败：{err}") from err

    got_diff_id = "sha256:" + hashlib.sha256(data).hexdigest()
    if expect_diff_id and got_diff_id != expect_diff_id:
        raise ValueError(
            f"层 {digest[:19]} 内容与 config 的 diff_id 不符（期望 {expect_diff_id[:19]}，"
            f"实际 {got_diff_id[:19]}）：镜像配置与层数据不一致"
        )

    # tar 完整性：截断的 tar 会在遍历到一半时抛 ReadError
    try:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as tar:
            count = 0
            for _ in tar:
                count += 1
    except (tarfile.ReadError, EOFError) as err:
        raise ValueError(
            f"层 {digest[:19]} 的 tar 已损坏（解压后仅 {len(data)} 字节，{count if 'count' in dir() else '?'} 个条目）："
            f"{err} —— 典型症状是构建期 docker save 被截断"
        ) from err


def main() -> int:
    if len(sys.argv) != 2:
        print("用法: verify_lpk.py <lpk 路径>", file=sys.stderr)
        return 2
    lpk = Path(sys.argv[1])
    if not lpk.is_file():
        print(f"lpk 不存在: {lpk}", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory() as tmp, tarfile.open(lpk) as outer:
        members = [
            m for m in outer.getmembers()
            if m.name == "images" or m.name.startswith("images/")
        ]
        if not members:
            print(f"{lpk.name}: 未内嵌镜像（无 images/），跳过校验")
            return 0
        outer.extractall(tmp, members=members, filter="data")
        images = Path(tmp) / "images"
        if not (images / "index.json").is_file():
            raise SystemExit(f"{lpk.name}: images/index.json 缺失，打包不完整")

        index = json.loads((images / "index.json").read_text())
        blobs = images / "blobs" / "sha256"
        layers_checked = 0
        for entry in index.get("manifests", []):
            m_digest = entry["digest"].split(":")[1]
            m_path = blobs / m_digest
            if not m_path.is_file():
                raise SystemExit(f"{lpk.name}: manifest blob 缺失 {entry['digest'][:19]}")
            if m_path.stat().st_size != entry.get("size", m_path.stat().st_size):
                raise SystemExit(f"{lpk.name}: manifest blob 尺寸不符 {entry['digest'][:19]}")
            manifest = json.loads(m_path.read_text())

            c_digest = manifest["config"]["digest"].split(":")[1]
            c_path = blobs / c_digest
            if not c_path.is_file():
                raise SystemExit(f"{lpk.name}: config blob 缺失 {manifest['config']['digest'][:19]}")
            config = json.loads(c_path.read_text())
            diff_ids = config.get("rootfs", {}).get("diff_ids", [])

            for i, layer in enumerate(manifest.get("layers", [])):
                l_digest = layer["digest"].split(":")[1]
                l_path = blobs / l_digest
                if not l_path.is_file():
                    raise SystemExit(f"{lpk.name}: 层 blob 缺失 {layer['digest'][:19]}")
                expect = diff_ids[i] if i < len(diff_ids) else ""
                try:
                    verify_blob(l_path, layer["digest"], layer["size"], expect)
                except ValueError as err:
                    raise SystemExit(f"{lpk.name}: 内嵌镜像损坏 —— {err}")
                layers_checked += 1

        if layers_checked == 0:
            raise SystemExit(f"{lpk.name}: index 里没有任何层，疑似打包异常")

    print(f"{lpk.name}: 内嵌镜像校验通过（{layers_checked} 层，tar/diff_id/尺寸一致）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
