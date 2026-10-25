#!/usr/bin/env python3
"""解析上游 release 与应用状态，产出同步计划（key=value 行，供 sync.sh 消费）。

用法: plan.py <state.json> <release.json> <force 0|1>

配置经环境变量读入（由 sync.sh source apps/<app>/app.env 后导出）：
  MARKER_BEGIN     release 首行标记前缀（空 = 不做组件门控，全文做更新日志）
  GATE_COMPONENT   必须出现在 components= 里的组件名（空 = 只要求 docker=ok）
  LANG_MARKER_ZH/EN 双语更新日志分段标记（空 = 全文做中文更新日志）

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


def emit(*pairs):
    print("\n".join(f"{k}={v}" for k, v in pairs))


def clean(text: str, drop_leading_version_heading: bool) -> str:
    """标题降为纯文本；drop_leading_version_heading 去掉首个「## x.y.z - 日期」标题行。"""
    lines = text.strip().splitlines()
    if drop_leading_version_heading and lines and re.match(r"^## [\w.\-]+\b", lines[0]):
        lines = lines[1:]
    out = [re.sub(r"^#{1,6}\s*", "", ln) for ln in lines]
    return "\n".join(out).strip()[:2000]


def section(body: str, start: str, end: str) -> str:
    i = body.find(start)
    if i < 0:
        return ""
    i += len(start)
    j = body.find(end, i) if end else -1
    return clean(body[i:] if j < 0 else body[i:j], drop_leading_version_heading=True)


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

    gated = False
    if marker:
        line = next((l for l in body.splitlines() if marker in l), "")
        m = re.search(r"components=([\w,\-]+).*?docker=(\w+)", line)
        if m:
            gated = True
            comps = m.group(1).split(",")
            docker_ok = m.group(2) == "ok"
            if (gate and gate not in comps) or not docker_ok:
                emit(("action", "seen_only"), ("tag", tag), ("version", version))
                return
        # 标记缺失（老格式/手写 release）：不据此跳过，保守发布

    zh_m = os.environ.get("LANG_MARKER_ZH", "")
    en_m = os.environ.get("LANG_MARKER_EN", "")
    zh = section(body, zh_m, en_m) if zh_m else ""
    en = section(body, en_m, "") if en_m else ""
    if not zh:
        zh = clean(body, drop_leading_version_heading=False)
    if not en:
        en = zh
    pathlib.Path("/tmp/changelog.zh.md").write_text(zh or f"{version} 更新")
    pathlib.Path("/tmp/changelog.en.md").write_text(en or f"{version} update")
    emit(("action", "publish"), ("tag", tag), ("version", version), ("gated", "1" if gated else "0"),
         ("changelog_zh", "/tmp/changelog.zh.md"), ("changelog_en", "/tmp/changelog.en.md"))


if __name__ == "__main__":
    main()
