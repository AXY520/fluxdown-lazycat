# lazycat-autopublish

无上游仓库权限应用的**懒猫微服商店自动发布合集**：每个应用一个 `apps/<应用名>/` 目录，
共享一套脚本与定时任务，自动跟进上游 GitHub release → 更新打包配置 → 构建 lpk → 冒烟 →
提交商店审核 → 状态落盘。

## 应用索引

| 应用 | 上游 | 商店包 id | 说明 |
|---|---|---|---|
| [fluxdown](apps/fluxdown) | [zerx-lab/FluxDown](https://github.com/zerx-lab/FluxDown) | `cloud.lazycat.app.fluxdown` | 多协议下载管理器（全内嵌 `ghcr.io/zerx-lab/fluxdown-server` 镜像，amd64） |

## 流水线（每 6 小时，或手动 dispatch）

```
上游 release（releases/latest，draft / prerelease 忽略）
  ├─ plan.py             组件门控（MARKER_BEGIN 标记的 components 里须含 GATE_COMPONENT
  │                      且 docker=ok，否则只记状态不发布）+ 双语更新日志提取
  ├─ docker pull         IMAGE_REPO:<版本>（= 存在性校验 + 本地构建依赖）
  ├─ sed 落版本号        apps/<应用>/packaging/{package.yml,lzc-build.yml}
  ├─ lzc-cli build       builder: local，lpk 全内嵌镜像（CI 无 lzc-cli 远端构建依赖）
  ├─ lpk lint            商店上架要求检查
  ├─ smoke.sh            应用自定义冒烟（apps/<应用>/smoke.sh，可选）
  ├─ appstore publish    --clangs zh/en 双语更新日志；「版本已存在」自动视为已发布
  └─ state.json          lastSeenTag / publishedVersion 提交回仓库（失败下轮重试）
```

多应用**顺序执行**（避免状态提交互相冲突），单应用失败不拖累其他，Run Summary 里有结果表；
lpk 留档 artifact。

## 目录结构

```
apps/<应用名>/
├── app.env       # bash 片段：UPSTREAM_REPO / PKG_ID 必填；IMAGE_REPO、
│                 # MARKER_BEGIN、GATE_COMPONENT、LANG_MARKER_ZH/EN 可选
├── packaging/    # lzc-build.yml / lzc-manifest.yml / package.yml / content / resources
├── smoke.sh      # 可选冒烟脚本，参数：<镜像引用> <版本号>（IMAGE_REPO 非空才调用）
└── state.json    # {"lastSeenTag": "vX.Y.Z", "publishedVersion": "X.Y.Z", ...}
```

## 新增一个应用

1. `mkdir apps/<应用名> && cp -r apps/fluxdown/{app.env,packaging,state.json} apps/<应用名>/`
2. 改 `app.env`（上游仓库、包 id、镜像、release 标记约定），换成该应用的 `packaging/` 打包配置，
   重写（或删掉）`smoke.sh`
3. `state.json` 的 `lastSeenTag` 初始化为**商店里已发布的最后一个版本**对应的上游 tag（留空则
   首轮直接发布最新版）
4. 本地演练：`FORCE=1 DRY_RUN=1 scripts/sync.sh <应用名>`
5. 提交推送即可 —— workflow 自动遍历 `apps/*/`，无需改任何公共文件

## Secrets（仓库级，全应用共享）

| 名称 | 值 |
|---|---|
| `LAZYCAT_APPSTORE_USER` | 懒猫应用商店账号 |
| `LAZYCAT_APPSTORE_PASS` | 懒猫应用商店密码 |

## 手动操作

```bash
# 本地全流程演练（构建+冒烟，不发布、不落状态）
FORCE=1 DRY_RUN=1 scripts/sync.sh <应用名>

# GitHub 手动触发：Actions → lazycat-store-publish → Run workflow
#   app  = all / 单个应用名
#   force = 强制重走发布（补发）
```

## 注意事项

- **商店审核是人工的**：本流水线自动「提交审核」，不是自动上架。
- **门控组件发布**：release 标记缺 `GATE_COMPONENT`（如 `server`）或 `docker≠ok` → 只记状态
  不发布（无新镜像可打包）。没有标记约定的应用，不设 `MARKER_BEGIN` 即可（全文做更新日志）。
- **计划任务休眠**：60 天无提交时 GitHub 会停用 schedule；跑一次 dispatch 即恢复。
- **发布失败兜底**：状态未落盘 → 下轮重试；商店已收包时命中「版本已存在」分支自动补记状态。
- **本地网络兼容**：脚本内置「公共 DNS 拿真实 IP 直连」兜底（部分路由器会改写 GitHub 域名
  导致偶发黑洞；CI 网络干净，不会走到这一步）。
