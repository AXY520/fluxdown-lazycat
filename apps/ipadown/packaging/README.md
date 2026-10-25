# ipadown

登录自己的 Apple ID，从 App Store 下载**历史版本** IPA（原版签名、零修改），然后在 iPhone 或 iPad 的 **Safari** 中点击安装。安装链接使用一小时随机令牌，避免 OTA 路径被直接枚举。

对标闭源工具 ipaDown（吾爱破解），本项目为开源实现，**Docker 一键部署 + Web 界面**。调研笔记见 [`research/`](./research/README.md)。

> **架构说明**：历史版本目录以 [爱果资源](https://app.agzy.cn/) 的 JSON 接口为唯一查询源，一次返回版本号、版本 ID、发布日期与大小；查询时只发送公开的 App Store ID 和国家/地区代码，不发送 Apple ID、Cookie 或会话信息。[majd/ipatool](https://github.com/majd/ipatool) 仅负责 Apple 账号会话和最终 IPA 下载，不再查询历史版本。

## 懒猫微服部署（推荐）

```bash
lzc-cli project release -o ipadown.lpk   # 构建 LPK 包
lzc-cli app install ipadown.lpk          # 安装到默认盒子
```

安装后复制 `https://ipadown.<你的微服域名>`，使用 iPhone 或 iPad 的 Safari 打开。页面会阻止 Chrome、Firefox、Edge 和应用内 WebView，安装接口也会再校验一次 Safari `User-Agent`。懒猫平台自带受信 HTTPS 与域名，不需要 host 网络、局域网 IP 或额外端口。OTA 端点（`/ota/**`）声明为 `public_path`，供 iOS 系统在安装时拉取 manifest 和 IPA。

应用使用懒猫多实例模式：每位微服用户拥有独立的容器与数据目录，Apple ID、下载任务和 IPA 文件不会跨用户共享。页面底部保留账户、搜索、下载三个入口；搜索可切换 iPhone、iPad 和 Apple TV，设备类型会同时传给 App Store 搜索与 ipatool 下载。下载触发双重认证时，页面会直接弹出 2FA 表单，验证成功后清理对应失败记录并自动重试原下载。

## Docker 部署

```bash
docker compose up -d --build
```

建议先在当前目录的 `.env` 中设置 Web 密码：

```dotenv
IPADOWN_WEB_PASSWORD=请换成高强度密码
```

compose 使用普通 bridge 网络，映射 `8080:8080`。安装前需用受信 HTTPS 域名反向代理到 8080，并设置 `PUBLIC_BASE_URL`。在 Safari 中依次完成：**登录 Apple ID → 搜索应用 → 选择历史版本 → 查看详情并下载 → 安装**。

> 首次登录时 ipatool 需下载 Apple 签名组件并完成初始化，可能需要几分钟，属正常现象。

数据（账号会话 + 下载的 IPA）持久化在 `./data` 卷。

### 容器日志

应用日志与操作日志全部写入容器 stdout/stderr（`docker logs` 或懒猫应用日志直接可看）：

- `http`：请求方法、路径、状态码、耗时（前端轮询端点只在出错时记录）
- `auth`：登录/登出/切换账号，含 2FA 挑战
- `download`：任务入队、开始、完成（大小/耗时）、失败原因
- `ipatool`：子进程命令（密码打码）、耗时、退出码、错误信息、网络重试
- `ota`：安装令牌发放、manifest/IPA 分发（含 Range）
- `history` / `files` / `startup` / `shutdown`：历史版本拉取、文件删除、启动与停止

排查下载失败时优先看 `download` 与 `ipatool` 两类日志。

### 环境变量

| 变量 | 必填 | 说明 |
|---|---|---|
| `PUBLIC_BASE_URL` | 域名安装必填 | 对外受信 HTTPS 地址，如 `https://ipa.example.com`；未设置时按 Host 头推导 |
| `IPADOWN_TRUST_PROXY` | 反代部署建议设置 | 设为 `1` 时信任 `X-Forwarded-Host/Proto` 头推导安装链接；仅在自家反向代理后开启，避免伪造头注入安装链接 |
| `PORT` | 否 | 主服务端口（Web/API），默认 `8080` |
| `IPADOWN_DATA_DIR` | 否 | 数据目录，默认 `/data` |
| `IPADOWN_WEB_USERNAME` | 否 | Docker Web/API Basic Auth 用户名，默认 `ipadown` |
| `IPADOWN_WEB_PASSWORD` | 公开网络必填 | Web/API Basic Auth 密码；留空时仅应在可信局域网使用 |
| `TLS_CERT` / `TLS_KEY` | 否 | 不走反代时让主服务直接提供 HTTPS |
| `HTTPS_PROXY` | 否 | 苹果服务器连接被重置时使用，如 `http://192.168.1.1:7890`（ipatool 与搜索均走系统代理环境变量） |
| `IPADOWN_MIN_FREE_BYTES` | 否 | 允许新下载任务入队所需的最小剩余空间，默认 2 GiB |

### Safari 直接安装

`itms-services://` 是 iOS/iPadOS 专用协议。本项目不生成二维码，也不提供局域网自签证书地址；必须用 Safari 打开应用页面，然后在“下载”中点击“安装”。

Apple 的 `itms-services` 机制强制要求 manifest 走**受信 HTTPS**（[官方文档](https://support.apple.com/en-gb/guide/deployment/depce7cefc4d/web)）：

1. 准备一个域名，签证书（Let's Encrypt 免费）；
2. 用反向代理（nginx / caddy / Nginx Proxy Manager）把 `https://你的域名` 转发到本服务 `8080`；或者把证书挂进容器用 `TLS_CERT`/`TLS_KEY`；
3. 把 `PUBLIC_BASE_URL` 设为该域名地址；
4. 用 Safari 打开应用页面，点击“安装”。

Caddy 反代示例：`ipa.example.com { reverse_proxy ipadown:8080 }`

## 本地开发

```bash
cargo run -p ipadown-web          # Web 服务（默认 8080）
```

## 重要提醒

- **不要修改 IPA**：任何修改都会破坏原版签名，失去"永不过期"特性。
- 手机的 App Store 账号需**拥有该 App**（免费 App 在手机上点一次"获取"即可）；付费 App 需先购买。
- 历史版本默认按发布日期从新到旧展示，下滑自动追加；点击版本后查看应用与版本详情，再发起下载。
- 建议使用**小号 Apple ID**（私有协议有风控风险，参考 Asspp 的警告）。
- 会话文件含账号凭据（0600 权限），请妥善保管数据目录。

## 项目结构

```
crates/
  store/   # App Store 搜索、爱果历史版本、ipatool 账号/下载封装与 IPA 元数据
  ota/     # manifest.plist / itms-services 浏览器安装链接
  web/     # axum Web 服务 + 内嵌单页前端（Docker 部署入口）
```

## 免责声明

仅供学习研究使用，与 Apple Inc. 无关。下载的 IPA 受 Apple 服务条款约束，请勿分发。
