#!/usr/bin/env python3
"""解析上游 release 与应用状态，产出同步计划（key=value 行，供 sync.sh 消费）。

用法: plan.py <state.json> <release.json> <force 0|1>

配置经环境变量读入（由 sync.sh source apps/<app>/app.env 后导出）：
  MARKER_BEGIN     release 首行标记前缀（空 = 不做组件门控）
  GATE_COMPONENT   必须出现在 components= 里的组件名（空 = 只要求 docker=ok）
  LANG_MARKER_ZH/EN 双语更新日志分段标记（空 = 自动从 release notes 正文提取）

输出（stdout，每行 key=value）:
  action=skip       无有效稳定 release，不做任何事
  action=uptodate   最新 tag 与 lastSeenTag 相同且未 force
  action=seen_only  release 不含门控组件 / docker≠ok（无新镜像），只记录状态
  action=publish    进入发布流程（同时写 /tmp/changelog.{zh,en}.md）
其余键: tag / version / changelog_zh / changelog_en
"""

import json
import os
import pathlib
import re
import sys

MAX_CHANGELOG = 2000


def emit(*pairs):
    print("\n".join(f"{k}={v}" for k, v in pairs))


def strip_noise(text: str) -> str:
    """去掉 HTML 注释、<details> 折叠块（下载/安装说明），保持其余行序。"""
    text = re.sub(r"<!--.*?-->", "", text, flags=re.S)
    text = re.sub(r"<details>.*?</details>", "", text, flags=re.S | re.I)
    return text


def clean(text: str) -> str:
    """markdown 标题降为纯文本；折叠多余空行；截断。"""
    out = []
    for line in text.strip().splitlines():
        line = re.sub(r"^#{1,6}\s*", "", line).rstrip()
        if line.strip() == "---":
            continue
        out.append(line)
    body = "\n".join(out)
    body = re.sub(r"\n{3,}", "\n\n", body).strip()
    return body[:MAX_CHANGELOG]


def section(body: str, start: str, end: str) -> str:
    i = body.find(start)
    if i < 0:
        return ""
    i += len(start)
    j = body.find(end, i) if end else -1
    return body[i:] if j < 0 else body[i:j]


def version_section(body: str, version: str) -> str:
    """取「## [x.y.z] - 日期」这类版本段（到下一个版本标题或文末）。"""
    pattern = re.compile(
        r"^##\s+\[?" + re.escape(version) + r"\]?[\s:–-].*$", re.MULTILINE
    )
    m = pattern.search(body)
    if not m:
        return ""
    tail = body[m.end():]
    nxt = re.search(r"^##\s+\[?v?\d+\.\d+\.\d+", tail, re.MULTILINE)
    return tail[: nxt.start()] if nxt else tail


def extract_changelog(body: str, version: str, zh_marker: str, en_marker: str) -> str:
    """按优先级提取更新日志正文。

    1. release 显式语言标记（fluxdown 风格双语 release notes）
    2. 「## [版本] - 日期」区块（git-cliff 风格）
    3. 去掉安装说明折叠块后的全文（最保守的兜底）
    """
    if zh_marker:
        marked = clean(strip_noise(section(body, zh_marker, en_marker)))
        if marked:
            return marked
    stripped = strip_noise(body)
    versioned = clean(version_section(stripped, version))
    if versioned:
        return versioned
    # 兜底：删掉 release 标记行与首行版本标题，其余全留
    fallback = "\n".join(
        l for l in stripped.splitlines() if "fluxdown:release:" not in l
    )
    fallback = re.sub(r"^##\s+\[?v?" + re.escape(version) + r"\]?.*$", "", fallback, count=1, flags=re.MULTILINE)
    return clean(fallback)


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
    marker = os.environ.get("MARKER_BEGIN", "")
    gate = os.environ.get("GATE_COMPONENT", "")

    gated = "0"
    if marker:
        line = next((l for l in body.splitlines() if marker in l), "")
        m = re.search(r"components=([\w,\-]+).*?docker=(\w+)", line)
        if m:
            gated = "1"
            comps = m.group(1).split(",")
            if (gate and gate not in comps) or m.group(2) != "ok":
                emit(("action", "seen_only"), ("tag", tag), ("version", version))
                return
        # 标记缺失（手写 release）：不据此跳过，保守发布

    zh_m = os.environ.get("LANG_MARKER_ZH", "")
    en_m = os.environ.get("LANG_MARKER_EN", "")
    zh = extract_changelog(body, version, zh_m, en_m)
    en = clean(strip_noise(section(body, en_m, ""))) if en_m else ""
    if not en:
        en = zh
    pathlib.Path("/tmp/changelog.zh.md").write_text(zh or f"{version} 更新")
    pathlib.Path("/tmp/changelog.en.md").write_text(en or f"{version} update")
    emit(("action", "publish"), ("tag", tag), ("version", version), ("gated", gated),
         ("changelog_zh", "/tmp/changelog.zh.md"), ("changelog_en", "/tmp/changelog.en.md"))


if __name__ == "__main__":
    main()
