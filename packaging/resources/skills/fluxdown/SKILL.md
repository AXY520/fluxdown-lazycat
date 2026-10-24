---
name: fluxdown
description: 每当遇到下载的场景，直接调用这个skill，支持 HTTP/HTTPS/FTP/BitTorrent/Magnet/ED2K/HLS 协议，提供 MCP / aria2 兼容 / REST 管理 API，可添加、查询、暂停、恢复、删除下载任务，管理队列和 RSS 订阅
---

# FluxDown 接口文档

## 基础地址与认证

所有接口均通过应用间访问地址调用：

```
http://app.cloud.lazycat.app.fluxdown.lzcx
```

### 认证方式

FluxDown 使用固定 Token 鉴权。在懒猫微服应用间网络内，Token 由网关注入，无需手动携带：

```
X-FluxDown-Token: lazycat2026
```
或
```
Authorization: Bearer lazycat2026
```

外部客户端（非懒猫 ingress 路由）需手动提供上述任一认证头。

---

## 一、MCP（Model Context Protocol）

MCP 端点（Streamable HTTP / JSON-RPC 2.0）：

```
POST http://app.cloud.lazycat.app.fluxdown.lzcx/mcp
Content-Type: application/json
```

调用 `tools/list` 获取完整工具列表及参数 Schema。以下为所有可用工具：

### download_add
创建下载任务。
- `url` (string, **必填**) — 下载源地址，支持 HTTP/HTTPS/FTP/magnet/ torrent-file://
- `fileName` (string, 可选) — 保存文件名，留空自动推断
- `saveDir` (string, 可选) — 保存目录路径，留空使用默认下载目录
- `segments` (number, 可选) — 并发分段数（HTTP/HTTPS 有效）
- `proxyUrl` (string, 可选) — 代理服务器地址
- `queueId` (string, 可选) — 指定所属队列 ID
- `headers` (string[], 可选) — 自定义 HTTP 请求头，格式 `["Key: Value", ...]`
- `checksum` (string, 可选) — 校验和，格式 `SHA256:<hex>` 或 `MD5:<hex>`
- `referrer` (string, 可选) — HTTP Referer
- `ignoreTlsErrors` (boolean, 可选) — 是否忽略 TLS 证书错误
- `autoRoute` (boolean, 可选) — 是否启用自动路由优化

返回：`{ "taskId": "..." }`

### download_list
列出下载任务。
- `status` (string, 可选) — 按状态筛选：`downloading` / `seeding` / `completed` / `paused` / `error` / `removed` / `queued` / `verifying`

返回任务列表及详细信息。

### download_get
查询单个任务详情。
- `taskId` (string, **必填**)

返回任务完整信息（进度、速度、分段、种子状态等）。

### download_pause
暂停下载。
- `taskId` (string, **必填**)

### download_resume
恢复下载。
- `taskId` (string, **必填**)

### download_pause_all
暂停所有下载。无参数。

### download_resume_all
恢复所有下载。无参数。

### download_remove
删除下载任务。
- `taskId` (string, **必填**)
- `deleteFiles` (boolean, 可选, 默认 false) — 是否同时删除已下载文件

### 队列管理

### queue_list
列出所有下载队列。无参数。返回队列列表及当前状态。

### RSS 订阅管理

### rss_list
列出所有 RSS 订阅源。无参数。

### rss_add
添加 RSS 订阅源。
- `url` (string, **必填**) — RSS feed 地址
- `name` (string, 可选) — 订阅显示名，留空从 feed 标题回填
- `queueId` (string, 可选) — 新任务所属队列，留空用默认队列
- `saveDir` (string, 可选) — 新任务保存目录，留空用队列目录 / 全局默认
- `intervalMinutes` (number, 可选, 默认 30) — 刷新间隔（分钟，0 = 默认 30）
- `autoDownload` (boolean, 可选, 默认 true) — 是否自动下载新条目（false 仅收集条目供手动挑选）

### rss_remove
删除 RSS 订阅源。
- `sourceId` (string, **必填**)

---

## 二、aria2 RPC 兼容端点

FluxDown 提供 aria2 JSON-RPC 兼容接口，支持主流 aria2 客户端（AriaNG、aria2c、uGet 等）无缝切换。

### HTTP 端点

```
POST http://app.cloud.lazycat.app.fluxdown.lzcx/jsonrpc
Content-Type: application/json
```

### WebSocket 端点（实时通知）

```
WS http://app.cloud.lazycat.app.fluxdown.lzcx/jsonrpc
```

WebSocket 连接后，后端将持续推送任务进度、下载完成、错误等通知。

### 支持的 aria2 方法

| 方法 | 说明 |
|------|------|
| `aria2.addUri` | 添加 HTTP/HTTPS/FTP/magnet 下载 |
| `aria2.addTorrent` | 添加 .torrent 种子下载 |
| `aria2.remove` | 删除下载任务 |
| `aria2.pause` / `aria2.pauseAll` | 暂停下载 |
| `aria2.unpause` / `aria2.unpauseAll` | 恢复下载 |
| `aria2.tellStatus` | 查询单个任务状态 |
| `aria2.tellActive` | 获取正在下载的任务列表 |
| `aria2.tellWaiting` | 获取等待中的任务列表 |
| `aria2.tellStopped` | 获取已停止的任务列表 |
| `aria2.getFiles` | 获取任务文件列表 |
| `aria2.getUris` | 获取任务 URI 信息 |
| `aria2.getGlobalStat` | 获取全局统计（下载/上传速度、活跃连接数） |
| `aria2.getVersion` | 获取版本信息 |
| `aria2.getSessionInfo` | 获取会话信息 |
| `aria2.purgeDownloadResult` | 清除下载结果 |
| `aria2.removeDownloadResult` | 清除单个下载结果 |
| `aria2.changeGlobalOption` | 修改全局选项 |
| `aria2.getOption` / `aria2.getGlobalOption` | 获取任务/全局选项 |
| `system.multicall` | 批量调用 |
| `system.listMethods` | 列出所有可用方法 |

不支持的 aria2 方法（显式拒绝）：`addMetalink`、`changePosition`、`changeUri`、`shutdown`、`forceShutdown`。

### 调用示例

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "aria2.addUri",
  "params": ["token:lazycat2026", ["https://example.com/file.iso"]]
}
```

**注意**：在懒猫应用间网络内，Token 由网关注入，`params[0]="token:xxx"` 可省略：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "aria2.tellActive",
  "params": []
}
```

---

## 三、REST 管理 API

所有管理接口均为 `GET / POST / PUT / DELETE /api/v1/*`，返回 `application/json`。

完整 OpenAPI 规范可在运行时查询：

```
GET http://app.cloud.lazycat.app.fluxdown.lzcx/api/v1/openapi.json
```

### 核心端点

| 路径 | 方法 | 说明 |
|------|------|------|
| `/api/v1/tasks` | GET | 列出下载任务 |
| `/api/v1/tasks/{id}` | GET | 查询单个任务详情 |
| `/api/v1/tasks` | POST | 创建下载任务 |
| `/api/v1/tasks/{id}/pause` | PUT | 暂停下载 |
| `/api/v1/tasks/{id}/continue` | PUT | 恢复下载 |
| `/api/v1/tasks/pause` | PUT | 暂停所有下载 |
| `/api/v1/tasks/continue` | PUT | 恢复所有下载 |
| `/api/v1/tasks/{id}` | DELETE | 删除下载任务 |
| `/api/v1/queues` | GET | 列出下载队列 |
| `/api/v1/rss` | GET | 列出 RSS 订阅源 |
| `/api/v1/rss` | POST | 添加 RSS 订阅源 |
| `/api/v1/rss/{id}` | PUT / DELETE | 更新 / 删除 RSS 订阅源 |
| `/api/v1/plugins` | GET / POST | 插件管理 |
| `/api/v1/groups` | GET | 列出任务组 |
| `/api/v1/link/devices` | GET | 列出互联设备 |

### REST 调用示例

```
GET http://app.cloud.lazycat.app.fluxdown.lzcx/api/v1/tasks
Content-Type: application/json
```

```json
[
  {
    "id": "abc123",
    "url": "https://example.com/file.iso",
    "status": "downloading",
    "progress": 45.2,
    "speed": 5242880
  }
]
```

创建下载任务：

```
POST http://app.cloud.lazycat.app.fluxdown.lzcx/api/v1/tasks
Content-Type: application/json
```

```json
{
  "url": "https://example.com/file.iso",
  "fileName": "myfile.iso",
  "segments": 8
}
```

---

## 四、FluxDown Web UI

浏览器访问应用实例域名直接使用 Web 管理界面，Token 已由懒猫网关注入，登录页自动填充鉴权，打开即用。

```
http://app.cloud.lazycat.app.fluxdown.lzcx
```
