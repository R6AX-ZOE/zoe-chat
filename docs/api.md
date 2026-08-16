# 守护进程 HTTP/WS API 契约 v0.1

守护进程监听 `127.0.0.1:<随机端口>`,提供静态 Web UI 与 JSON API。**认证**:首次启动生成 32 字节 token,写入数据目录 `token` 文件(0600);除 `GET /`(返回登录页)外,所有请求需 `Authorization: Bearer <token>`。浏览器访问流程:登录页输入 token → 存入 localStorage → 之后自动携带。CORS:仅允许 `Origin: http://127.0.0.1:*` 与 `null`(file:// 场景),其余拒绝。

## 1. REST 端点

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/v1/me` | 本机身份:user_id、指纹、设备列表、created_at |
| POST | `/api/v1/pair/start` | 进入配对模式;返回 `{pair_code, bt_advertising: bool}` |
| POST | `/api/v1/pair/stop` | 退出配对模式 |
| POST | `/api/v1/pair/verify` | 带外验证:`{peer_id, ok: bool}`(比对指纹结果) |
| GET | `/api/v1/peers` | 已配对 peer 列表(指纹、信任状态、可达传输) |
| POST | `/api/v1/peers/:id/block` | 阻止 peer |
| GET | `/api/v1/card` | 远程名片:`{peer_id, fingerprint, qr: dataURL}` |
| POST | `/api/v1/card/import` | 导入名片:`{text}`(解析 `zoe://peer/...`) |
| GET | `/api/v1/groups` | 群组列表(名称、成员数、epoch、协调者状态) |
| POST | `/api/v1/groups` | 建群:`{name}` |
| POST | `/api/v1/groups/:id/invite` | 邀请:`{peer_id}` |
| POST | `/api/v1/groups/:id/leave` | 退出群组 |
| GET | `/api/v1/groups/:id/messages?before=&limit=` | 消息分页(envelope+已解密文本) |
| POST | `/api/v1/groups/:id/messages` | 发消息:`{text}` |
| GET | `/api/v1/devices` | 设备列表(本用户) |
| POST | `/api/v1/devices/:id/revoke` | 吊销设备 |
| GET | `/api/v1/backup/mnemonic` | 生成助记词(需口令确认:`{password}`) |
| POST | `/api/v1/restore` | 助记词恢复:`{mnemonic, password}` |
| GET | `/api/v1/transports` | 各传输状态:ble/lan/net 是否可用、邻居数、打洞状态 |
| POST | `/api/v1/settings` | 设置:明文缓存保留期、自建中继地址、自动启动、`ui_language` |

错误:统一 `{"error": {"code": ..., "message": ...}}`,HTTP 语义映射(400/401/404/409/503)。

## 2. WebSocket `/api/v1/events`

服务端推送事件(JSON):

```
{"type":"message",   "group_id":..., "message_id":..., "seq":...}
{"type":"status",    "group_id":..., "message_id":..., "status":"delivered|read|failed"}
{"type":"peer",      "peer_id":..., "state":"paired|verified|blocked|offline|online"}
{"type":"group",     "group_id":..., "event":"created|joined|member_added|member_removed|coordinator_offline"}
{"type":"transport", "transport":"ble|lan|net", "state":"up|down", "detail":...}
```

- 心跳:客户端每 30s `{"type":"ping"}`,服务端 `pong`,60s 无响应断开。
- UI 收到 `message` 事件后拉取 `GET /groups/:id/messages` 增量。

## 3. 静态资源

- `GET /` → 登录页;`GET /app` → 主界面(会话/群组/设置三个视图)。
- 资源打包进二进制(`include_str!` 或 `rust-embed`),无外部 CDN 依赖(离线可用)。
- UI 无框架:vanilla TS,构建产物 < 200KB gzip。
- **多语言(i18n)**:UI 文案走客户端目录 `locales/{zh-CN,en-US}.json`;语言优先级 = 用户设置(`settings.ui_language`)> `navigator.language` > 默认 en-US。服务端不参与翻译:API 只返回数据与错误码,文案由客户端映射,切换即时生效、无需重启。键集合完整性由 CI 校验(见 docs/modules.md §4)。
- **消息内容**:任意 UTF-8 明文,协议层无语言限制(中文/emoji/RTL 均可);RTL 渲染为 UI 层 CSS 职责(`dir` 属性),与服务端无关。
- **主题(深色/浅色)**:CSS 变量主题体系,`<html data-theme="light|dark">` 切换;默认跟随 `prefers-color-scheme`,用户选择持久化于 `settings.ui_theme`;对比度按 WCAG AA 校验。
- **图标**:统一自绘 SVG 图标集(24×24 viewBox,stroke 1.5px,圆角线帽/线接、平滑曲线路径),打包为内联 sprite;**禁止使用 emoji 作图标**(渲染差异、无障碍、严肃性);装饰图标 `aria-hidden`,功能图标配可访问文本。
- **响应式**:移动优先布局,断点 `<640px` 单栏 / `≥1024px` 三栏(会话列表·消息·详情);触控目标 ≥44px;含 `safe-area-inset` 适配。守护进程仍仅绑定 127.0.0.1——移动端浏览器访问需 SSH 端口转发(如 Termux 场景),UI 按窄视口自适应。

## 4. 安全边界

- 守护进程只绑定 127.0.0.1;拒绝非本机来源。
- token 校验恒时比较;登录失败统一响应,不区分"token 错误/不存在"。
- UI 渲染所有用户内容一律转义(防本地 XSS → 防经由 UI 窃取会话的权限提升)。
- WS 消息带单调递增序号,客户端校验防重放(本地威胁小,但成本极低)。
