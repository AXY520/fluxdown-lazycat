# lazycat-autopublish

无上游仓库权限应用的**懒猫微服商店自动发布合集**：每个应用一个 `apps/<应用名>/` 目录，
共享一套脚本与定时任务，自动跟进上游 GitHub release → 更新打包配置 → 构建 lpk → 冒烟 →
提交商店审核 → 状态落盘。

## 应用索引

| 应用 | 上游 | 商店包 id | 说明 |
|---|---|---|---|
| [fluxdown](apps/fluxdown) | [zerx-lab/FluxDown](https://github.com/zerx-lab/FluxDown) | `cloud.lazycat.app.fluxdown` | 多协议下载管理器（镜像经 copy-image 进懒猫官方仓库，lpk 仅引用，~1MB） |

## 流水线（每 6 小时，或手动 dispatch）

```
上游 release（releases/latest，draft / prerelease 忽略）
  ├─ plan.py             组件门控（MARKER_BEGIN 标记的 components 里须含 GATE_COMPONENT
  │                      且 docker=ok，否则只记状态不发布）+ 双语更新日志提取
  ├─ docker pull         IMAGE_REPO:<版本>（存在性校验 + 冒烟对象）
  ├─ copy-image          镜像复制进 registry.lazycat.cloud，manifest 改写为官方引用
  ├─ sed 落版本号        apps/<应用>/packaging/package.yml
  ├─ lzc-cli build       小包（不含镜像层）
  ├─ lpk lint            商店上架要求检查
  ├─ verify_lpk.py       内嵌镜像完整性校验（embed 模式拦截断层）
  ├─ smoke.sh            应用自定义冒烟（apps/<应用>/smoke.sh，可选）
  ├─ appstore publish    --clangs zh/en 双语更新日志；「版本已存在」自动视为已发布
  └─ state.json          lastSeenTag / publishedVersion 提交回仓库（失败下轮重试）
```

多应用**顺序执行**（避免状态提交互相冲突），单应用失败不拖累其他，Run Summary 里有结果表；
lpk 留档 artifact。

## 镜像交付：registry 优先（重要）

`app.env` 的 `IMAGE_MODE` 决定镜像怎么进包：

| 模式 | 行为 | 何时用 |
|---|---|---|
| `registry`（默认） | `lzc-cli appstore copy-image` 把镜像推到 `registry.lazycat.cloud/<账号>/...`，manifest 里 `image:` 直接引用；lpk ≈1MB | 有上游 docker 镜像的应用（商店官方推荐做法） |
| `embed` | 把镜像层内嵌进 lpk（`images:` + `image: embed:<alias>`），lpk 数十 MB | 镜像无法走官方仓库时 |

**为什么不默认内嵌**：内嵌依赖构建机 `docker save` 导出的层完整性。GitHub runner
（docker 28.0.4 + BuildKit）曾**稳定**导出被截断的层——manifest 尺寸对、gzip 完整，
但解压出的 tar 在 1.7MB 处断掉，`lpk lint` 完全看不出来，装到盒子上一启动就
`docker image load` 报 `unexpected EOF`，安装失败。registry 模式绕开整条链路，
顺带把每包 62MB 的体积降下来。`scripts/verify_lpk.py` 保留在 embed 模式下把关
（逐层校验 diff_id + tar 可完整读到 EOF），校验不过会重建一次，仍不过就中止发布，
绝不把坏包推给商店。

## 更新日志从哪来

`scripts/plan.py` 按优先级提取 release notes：

1. release 里的显式语言标记（`fluxdown:lang:zh` / `fluxdown:lang:en`）
2. 「`## [版本] - 日期`」版本段（git-cliff 风格）
3. 兜底全文

提取前会剥掉 HTML 注释与 `<details>` 折叠块——否则会把「下载与安装说明」整段当成
更新日志推给商店（曾发生过）。

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
- **DRY_RUN 会还原脚本改动的文件**（`package.yml` / `lzc-manifest.yml` / `lzc-build.yml` /
  `state.json`），本地演练前记得先提交自己的配置改动。
- **门控组件发布**：release 标记缺 `GATE_COMPONENT`（如 `server`）或 `docker≠ok` → 只记状态
  不发布（无新镜像可打包）。没有标记约定的应用，不设 `MARKER_BEGIN` 即可（全文做更新日志）。
- **计划任务休眠**：60 天无提交时 GitHub 会停用 schedule；跑一次 dispatch 即恢复。
- **发布失败兜底**：状态未落盘 → 下轮重试；商店已收包时命中「版本已存在」分支自动补记状态。
- **本地网络兼容**：脚本内置「公共 DNS 拿真实 IP 直连」兜底（部分路由器会改写 GitHub 域名
  导致偶发黑洞；CI 网络干净，不会走到这一步）。
