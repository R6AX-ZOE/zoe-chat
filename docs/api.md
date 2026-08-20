# 守护进程 HTTP/WS API 契约 v0.1

守护进程监听 `127.0.0.1:<随机端口>`,提供静态 Web UI 与 JSON API。**认证**:首次启动生成 32 字节 token,写入数据目录 `token` 文件(0600);除 `GET /`(返回登录页)外,所有请求需 `Authorization: Bearer <token>`。浏览器访问流程:登录页输入 token → 存入 localStorage → 之后自动携带。CORS:仅允许 `Origin: http://127.0.0.1:*` 与 `null`(file:// 场景),其余拒绝。

## 1. REST 端点

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/v1/me` | 本机身份:user_id、指纹、设备列表、created_at |
| GET | `/api/v1/users` | 用户列表(注册表):`{users:[{id,name,kind,created_at,last_used,active}]}`,`kind`=`plain`|`pin` |
| POST | `/api/v1/users` | 创建用户:`{name, pin}`(PIN ≥4 位);数据落 `users/<id>/`,种子立即加密、不落明文 |
| POST | `/api/v1/users/:id/set-pin` | 为激活的 plain 用户设置 PIN:`{pin}`;v1 仅允许激活用户 |
| POST | `/api/v1/unlock` | 锁定模式解锁:`{pin}`;验证通过后恢复身份/会话/网络 |
| POST | `/api/v1/pair/start` | 进入配对模式;返回 `{pair_code, bt_advertising: bool}` |
| POST | `/api/v1/pair/stop` | 退出配对模式 |
| POST | `/api/v1/pair/verify` | 带外验证:`{peer_id, ok: bool}`(比对指纹结果) |
| GET | `/api/v1/peers` | 已配对 peer 列表(指纹、信任状态、可达传输) |
| POST | `/api/v1/peers/:id/block` | 阻止 peer |
| GET | `/api/v1/card` | 远程名片:`{peer_id, fingerprint, qr: dataURL}` |
| POST | `/api/v1/card/import` | 导入名片:`{text}`(解析 `zoe://peer/...`) |
| GET | `/api/v1/groups` | 群组列表(名称、成员数、epoch、协调者状态;私聊含 `direct`/`direct_peer`/`direct_peer_id`/`direct_name`) |
| POST | `/api/v1/groups` | 建群:`{name}` |
| POST | `/api/v1/groups/:id/invite` | 邀请:`{peer_id}` |
| POST | `/api/v1/groups/:id/leave` | 退出群组 |
| GET | `/api/v1/groups/:id/messages?before=&limit=` | 消息分页(envelope+已解密文本/文件元数据) |
| POST | `/api/v1/groups/:id/messages` | 发消息:`{text}` |
| POST | `/api/v1/groups/:id/files` | 发文件消息:`{name, mime, data}`(data=base64,≤8 MiB;`file_downloaded` 标记小文件自动下载) |
| GET | `/api/v1/files/:msg_hash` | 下载文件消息内容(流式返回;同时落盘 `files/` 并标记已下载) |
| GET | `/api/v1/directs` | 单聊(私聊)列表:group_id ↔ 联系人 peer_id 映射 |
| POST | `/api/v1/directs` | 发起单聊:`{peer_id, addr?}`(addr 可选;已有私聊复用) |
| GET | `/api/v1/devices` | 设备列表(本用户) |
| POST | `/api/v1/devices/:id/revoke` | 吊销设备 |
| GET | `/api/v1/backup/mnemonic` | 生成助记词(需口令确认:`{password}`) |
| POST | `/api/v1/restore` | 助记词恢复:`{mnemonic, password}` |
| GET | `/api/v1/transports` | 各传输状态:ble/lan/net 是否可用、邻居数、打洞状态 |
| POST | `/api/v1/settings` | 设置:明文缓存保留期、自建中继地址、自动启动、`ui_language` |

错误:统一 `{"error": {"code": ..., "message": ...}}`,HTTP 语义映射(400/401/404/409/423/503)。

## 1.1 多用户注册表与锁定模式

- **用户注册表**:`data_dir/users.db` 记录全部用户(见 docs/storage.md §1.1)。每用户运行数据独立目录 `users/<id>/`(各自 `zoe.db`/`mls.db`);`kind=plain` 为无 PIN 用户(含从旧版明文迁移的 `default`),`kind=pin` 的种子以 PIN 派生密钥加密落盘。
- **锁定模式**:激活用户为 `pin` 且守护进程启动未带 `--pin` 时进入锁定态。锁定期间仅放行 `GET /`、`/users`、`/users/*` 与 `/unlock`(以及 `/status`),其余端点一律 **423 Locked**。
- **解锁**:`POST /unlock` 提交 PIN → 注册表 `verify_pin` → 恢复激活用户的身份/设备/MLS 会话/网络栈;成功后锁定态解除,后续请求恢复。PIN 校验失败返回 400。
- **切换用户**:v1 约定为 CLI `zoe-cli user activate <id>` 后重启守护进程(`--user <id> [--pin <pin>]`);HTTP 不提供账号切换。
- **创建用户**:`POST /users` 即时生效无需重启;新用户种子 = 随机 Ed25519 身份,即时以 PIN 加密写入注册表,**明文种子不落任何磁盘**。注册表标记为待激活,重启 daemon 指定 `--user <id> [--pin <pin>]` 后成为激活用户。
- **set-pin**:仅对**激活且已解锁**的 plain 用户开放;为其它用户设置 PIN 返回 400("PIN can only be set for the active user in v1")。
- **restore 限制**:`POST /restore`(助记词恢复)仅对 plain 用户开放;PIN 用户返回 400(恢复会覆盖身份与设备,须先在 CLI 层以 `default` 明文账号执行)。

## 2. WebSocket `/api/v1/events`

服务端推送事件(JSON):

```
{"type":"message",   "group_id":..., "message_id":..., "seq":...}
{"type":"status",    "group_id":..., "message_id":..., "status":"delivered|read|failed"}
{"type":"peer",      "peer_id":..., "state":"paired|verified|blocked|offline|online"}
{"type":"group",     "group_id":..., "event":"created|joined|member_added|member_removed|coordinator_offline"}
{"type":"transport", "transport":"ble|lan|net", "state":"up|down", "detail":...}
{"type":"user",      "user_id":..., "event":"created|pin_set"}
```

- 心跳:客户端每 30s `{"type":"ping"}`,服务端 `pong`,60s 无响应断开。
- UI 收到 `message` 事件后拉取 `GET /groups/:id/messages` 增量。

## 3. 静态资源

- `GET /` → 登录页;`GET /app` → 主界面(会话/群组/设置三个视图)。
- 资源打包进二进制(`include_str!` 或 `rust-embed`),无外部 CDN 依赖(离线可用)。
- UI 无框架:Leptos → wasm(去 npm/tsc/vite),构建产物 < 2MB wasm + 小 js 胶水。
- **消息内容与文件**:文本消息明文为裸 UTF-8;文件消息为结构化二进制(`0x02 0x01 | name | size | mime | data`,见 `crates/zoe-core/src/content.rs`),大小 ≤ 8 MiB。接收端解密后 ≤ 1 MiB 的小文件**自动落盘**到 `data_dir/files/` 并标记 `file_downloaded`;大文件点击"下载"时经 `GET /files/:msg_hash` 落盘并提供浏览器下载。
- **单聊(私聊)**:与联系人的单聊 = 双人 MLS 群,`groups.direct_peer` 记录对端 libp2p peer id;发送时信封只定向投递给对端(不广播)。联系人表 `net_peer_id` 记录 libp2p 标识 ↔ zoe peer_id 映射,`/directs` 返回该映射供 UI 从联系人直接打开已有单聊。
- **多语言(i18n)**:UI 文案走 `webui/src/i18n.rs` 键驱动词典(zh-CN/en-US 各 147 键,无外部 json);语言优先级 = 用户设置(`settings.ui_language`)> `navigator.language` > 默认 en-US。服务端不参与翻译:API 只返回数据与错误码,文案由客户端映射,切换即时生效、无需重启。键集合完整性由单测 + CI 校验(见 docs/modules.md §4)。
- **消息内容**:任意 UTF-8 明文,协议层无语言限制(中文/emoji/RTL 均可);RTL 渲染为 UI 层 CSS 职责(`dir` 属性),与服务端无关。
- **主题(深色/浅色)**:CSS 变量主题体系,`<html data-theme="light|dark">` 切换;默认跟随 `prefers-color-scheme`,用户选择持久化于 `settings.ui_theme`;对比度按 WCAG AA 校验。
- **图标**:统一自绘 SVG 图标集(24×24 viewBox,stroke 1.5px,圆角线帽/线接、平滑曲线路径),打包为内联 sprite;**禁止使用 emoji 作图标**(渲染差异、无障碍、严肃性);装饰图标 `aria-hidden`,功能图标配可访问文本。
- **响应式**:移动优先布局,断点 `<640px` 单栏 / `≥1024px` 三栏(会话列表·消息·详情);触控目标 ≥44px;含 `safe-area-inset` 适配。守护进程仍仅绑定 127.0.0.1——移动端浏览器访问需 SSH 端口转发(如 Termux 场景),UI 按窄视口自适应。

## 4. 安全边界

- 守护进程只绑定 127.0.0.1;拒绝非本机来源。
- token 校验恒时比较;登录失败统一响应,不区分"token 错误/不存在"。
- UI 渲染所有用户内容一律转义(防本地 XSS → 防经由 UI 窃取会话的权限提升)。
- WS 消息带单调递增序号,客户端校验防重放(本地威胁小,但成本极低)。
