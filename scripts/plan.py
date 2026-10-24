#!/usr/bin/env python3
"""解析上游 release 与本地状态，产出同步计划（key=value 行，供 sync.sh 消费）。

用法: plan.py <state.json> <release.json> <force 0|1>

输出（stdout，每行 key=value）:
  action=skip       无有效稳定 release，不做任何事
  action=uptodate   最新 tag 与 lastSeenTag 相同且未 force
  action=seen_only  release 标记不含 server 组件（无新镜像），只记录状态
  action=publish    进入发布流程（同时写 /tmp/changelog.{zh,en}.md）
其余键: tag / version / server_changed / marker
"""

import json
import pathlib
import re
import sys


def emit(*pairs):
    print("\n".join(f"{k}={v}" for k, v in pairs))


def section(body: str, start: str, end: str) -> str:
    i = body.find(start)
    if i < 0:
        return ""
    i += len(start)
    j = body.find(end, i) if end else -1
    text = body[i:] if j < 0 else body[i:j]
    lines = text.strip().splitlines()
    # 去掉首个 “## <version> - <date>” 标题行（版本号由 -c/--clangs 元数据承载）
    if lines and lines[0].startswith("## "):
        lines = lines[1:]
    out = []
    for ln in lines:
        ln = re.sub(r"^#{1,6}\s*", "", ln)  # 标题降为纯文本
        out.append(ln)
    return "\n".join(out).strip()[:2000]


def main() -> None:
    state_file, release_file, force = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
    state = json.loads(pathlib.Path(state_file).read_text())
    rel = json.loads(pathlib.Path(release_file).read_text())

    tag = (rel.get("tag_name") or "").strip()
    if not tag or rel.get("draft") or rel.get("prerelease"):
        emit(("action", "skip"))
        return
    version = tag[1:] if tag.startswith("v") else tag

    if not force and state.get("lastSeenTag") == tag:
        emit(("action", "uptodate"), ("tag", tag), ("version", version))
        return

    body = rel.get("body") or ""
    m = re.search(r"fluxdown:release:begin\s+components=([\w,\-]+)[^\>]*?docker=(\w+)", body)
    if m:
        server_changed = "server" in m.group(1).split(",")
        docker_ok = m.group(2) == "ok"
        marker = "1"
    else:
        # 没有 release 标记（老格式/手写），不据此跳过，交给镜像存在性检查兜底
        server_changed, docker_ok, marker = True, True, "0"

    if not (server_changed and docker_ok):
        emit(("action", "seen_only"), ("tag", tag), ("version", version),
             ("server_changed", "0"), ("marker", marker))
        return

    zh = section(body, "<!-- fluxdown:lang:zh -->", "<!-- fluxdown:lang:en -->")
    en = section(body, "<!-- fluxdown:lang:en -->", "")
    pathlib.Path("/tmp/changelog.zh.md").write_text(zh or f"{version} 更新")
    pathlib.Path("/tmp/changelog.en.md").write_text(en or zh or f"{version} update")
    emit(("action", "publish"), ("tag", tag), ("version", version),
         ("server_changed", "1"), ("marker", marker),
         ("changelog_zh", "/tmp/changelog.zh.md"), ("changelog_en", "/tmp/changelog.en.md"))


if __name__ == "__main__":
    main()
