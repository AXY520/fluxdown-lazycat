# fluxdown-lazycat

[FluxDown](https://github.com/zerx-lab/FluxDown) → 懒猫微服应用商店的**自动发布流水线**。

本仓库是 FluxDown 懒猫 LPK 打包配置的**唯一事实源**（`packaging/` 目录）；上游每次发布含
server 组件的稳定版，这里自动完成：版本检测 → 配置更新 → lpk 构建 → 冒烟 → 商店提交 → 状态落盘。

```
上游 release（zerx-lab/FluxDown，含 server 组件）
        │  每 6 小时轮询 / 手动 dispatch
        ▼
scripts/sync.sh
  ├─ plan.py           解析 release 标记（components / docker / 双语 changelog）
  ├─ ghcr 镜像校验      ghcr.io/zerx-lab/fluxdown-server:<版本> 必须已推送
  ├─ lzc-cli build     packaging/ 构建 lpk（builder: local，全内嵌镜像，amd64）
  ├─ lpk lint          商店上架要求检查
  ├─ 冒烟              容器探活 /ping 版本匹配 + 登录自动填充选择器 access-key 存在
  ├─ appstore publish  双语更新日志（--clangs zh/en），提交审核
  └─ state.json        lastSeenTag / publishedVersion 提交回仓库
```

## 状态文件

| 字段 | 含义 |
|---|---|
| `lastSeenTag` | 已处理过的最新上游 tag（含「无 server 变更、跳过」的版本） |
| `publishedVersion` | 商店已提交的版本 |

跳过逻辑：最新 release 的标记 `components=` 不含 `server` 或 `docker≠ok` → 只记录
`lastSeenTag`，不发布（纯客户端版本无新镜像可打包）。镜像 tag 缺失则本轮失败，下轮重试。

## Secrets（仓库 Settings → Secrets and variables → Actions）

| 名称 | 值 |
|---|---|
| `LAZYCAT_APPSTORE_USER` | 懒猫应用商店账号（手机号） |
| `LAZYCAT_APPSTORE_PASS` | 懒猫应用商店密码 |

## 手动操作

```bash
# 本地全流程演练（构建+冒烟，不发布、不落状态）
FORCE=1 DRY_RUN=1 scripts/sync.sh

# GitHub 上手动触发（Actions → lazycat-store-publish → Run workflow，勾选 force 可补发当前最新版）
# 仓库 Secrets 配置：
gh secret set LAZYCAT_APPSTORE_USER -R <owner>/fluxdown-lazycat -b "..."
gh secret set LAZYCAT_APPSTORE_PASS -R <owner>/fluxdown-lazycat -b "..."
```

## 维护须知

- **登录自动填充选择器**：`packaging/lzc-manifest.yml` 注入依赖 Web SPA 的 `#access-key`
  输入框 id。冒烟检查会 grep 镜像内二进制确认字符串仍在，但 SPA 重写后请人工开一次页面确认。
- **API 文档 Skill**：`packaging/resources/skills/fluxdown/SKILL.md` 描述 MCP / aria2 / REST
  接口，随版本演进需人工同步更新。
- **架构**：lpk 全内嵌 `linux/amd64`（与历史上架版本一致；懒猫盒子以 x86 为主）。
- **商店审核是人工的**：本流水线自动「提交审核」，不是自动上架；发版过密会占审核队列。
- **计划任务休眠**：公开/私有仓库 60 天无提交时 GitHub 会停用 schedule；跑一次
  dispatch（任意提交）即可恢复。
- 发布失败的兜底：状态未落盘 → 下轮重试；若商店已收包，会命中「版本已存在」分支自动补记状态。

## 许可

FluxDown 为 AGPL-3.0；本仓库仅包含其打包配置与流水线脚本。
