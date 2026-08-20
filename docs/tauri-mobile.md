# zoe-mobile：Tauri 2 集成 zoe-core 设计交接

> 状态：**M0 已完成，CI + 真机验收通过**（见"验收记录"；黑屏根因 `windows:[]` 已修复入坑 17）。
> **M1 之后（2026-08-19）：Web UI 已用 Rust 重写（Leptos → wasm），桌面 daemon 与移动端共用同一份产物；移动端内嵌守护进程，WebView 直接加载 `http://127.0.0.1:18571/`。见 §0"Rust Web UI 与内嵌守护进程"。**
> 本文档供新上下文直接执行，避免重新决策。
> 未标注"待定"处均为已定决策；执行时若发现与代码事实冲突，先更新本文档再改代码。

## 0. Rust Web UI 与内嵌守护进程（2026-08-19 定稿）

**动机**：去掉 JS/TS 工具链（npm/tsc/vite），Web UI 全部用 Rust 编写；桌面与 Android 共用同一份 UI 产物；移动端复用桌面守护进程的全部逻辑（建群/消息/MLS/邀请/配对/设备/设置）。

**架构变化**：

```
┌────────────────────────── Android 真机 ──────────────────────────┐
│  WebView → http://127.0.0.1:18571/（内嵌守护进程服务,同源,免 CORS）│
│   ├─ Rust/Leptos wasm UI（webui/dist,zoe-daemon include! 内嵌）  │
│  ┌──────────────────────▼───────────────────────────────┐        │
│  │ Rust: zoe-daemon(lib,default-features=false)         │        │
│  │  ├─ 完整 HTTP/WS API(与桌面完全同构)                  │        │
│  │  ├─ zoe-core/zoe-transport(ble-mobile)              │        │
│  │  └─ bridge.rs(回环 TCP 桥 → Kotlin BLE 物理层)        │        │
│  └──────────────────────────────────────────────────────┘        │
│                            │ 127.0.0.1:18570(桥,保留)            │
│  Kotlin: Bridge / ZoeBleServer / ZoeAdvertiser（BLE 物理层）      │
└──────────────────────────────────────────────────────────────────┘
```

- **webui/ 为独立 crate（`zoe-webui`）**：Leptos 0.7 CSR → `wasm32-unknown-unknown`，`scripts/build.sh|.ps1` 产出 `dist/`（index.html + styles.css + assets/zoe_webui.js + zoe_webui_bg.wasm）。**dist 与 webui/Cargo.lock 提交入库**（zoe-daemon 编译期 `include_str!/include_bytes!` 内嵌），CI 校验 dist 与源码一致（不一致 → 有网络机器跑 build 脚本后提交）。独立于根 workspace（自带 Cargo.lock；`cargo test --manifest-path webui/Cargo.toml` 跑原生单测，含 i18n 键集合一致性）。
- **zoe-daemon 拆 lib + bin**：`crates/zoe-daemon/src/lib.rs` 导出 `start(DaemonConfig)`（建库/身份/会话恢复/net 传输/axum 启动）；bin 为薄壳（参数解析 + 打印）。`net` feature 可选：桌面默认开，移动端 `default-features = false` 排除 libp2p。
- **移动端启动顺序（lib.rs setup）**：`zoe_daemon::start(DaemonConfig::mobile(data_dir))`（`tauri::async_runtime::block_on`，固定端口 18571，绑 127.0.0.1，移动端模式禁用户切换/自重启）→ 创建 WebView（`WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))`，先起服务后建窗口，无竞态）。config 里 `app.windows: []`（避免 tauri 先建窗口加载空地址）。
- **多用户与锁定（2026-08-20 定稿）**：`DaemonConfig::mobile` 不带 `--user/--pin`，按注册表激活账号运行；v1 移动端保持**单用户**（无账号切换入口，`POST /users/:id/activate` 返回 400）。激活账号为 PIN 用户时进入锁定模式（其余 API 423），Web UI 显示锁定屏，`POST /unlock` 输 PIN 解锁——桌面与移动端共用同一锁定/解锁逻辑与 UI。
- **无令牌引导（2026-08-20）**：访问令牌与 `zoe_boot_token`/登录页已移除。UI 启动直连内嵌守护进程（同源）,先探 `/users` 判定锁定态;桌面浏览器同理,无登录步骤。
- **Android cleartext**：`android/gen-patches/AndroidManifest.xml` 的 application 硬编码 `android:usesCleartextTraffic="true"`（仅回环 127.0.0.1）。
- **构建管线**：android-tauri.yml 与 ci.yml **不再使用 npm/Node**；tauri CLI 经 `cargo install tauri-cli`。wasm-bindgen-cli 版本 = webui/Cargo.lock 中 wasm-bindgen 版本（脚本自动提取）。
- **app/ 前端目录已删除**（vite/package.json/src/main.js 控制台 UI 不再使用）；`tauri.conf.json` 保留 `frontendDist: ../../webui/dist`（APK 内打包兜底，实际窗口加载外部 URL 不读取）。

**已知限制（诚实声明）**：
- 内嵌守护进程的 `/pair/start` 仍只置状态（BLE 广告需桥接 Kotlin，尚未接线——移动端 BLE 配对 UI 显示"驱动不可用"）；BLE 联调仍走 bridge 命令。
- WebView 加载外链 http://127.0.0.1 时 tauri 的能力（invoke）按窗口 label 匹配，capabilities 的 `windows: ["main"]` 保持。
- `window.confirm()`（退出群组/吊销设备/恢复身份确认）依赖 WebChromeClient 的 onJsConfirm —— 桌面浏览器与 Android WebView 行为待真机确认；如不可用需改为内联对话框（建群已改为内联对话框，见下）。

**UI 修复记录（2026-08-19，真机/浏览器实测后）**：
1. **登录报 invalid token**（已随令牌移除而消失,2026-08-20）：`api.rs login()` 曾用泛型 `()` 解析响应,而服务端返回 `{"ok":true}`(map)——`()` 只能反序列化 null → 解析失败被 UI 误报"令牌无效"。改为显式 `serde_json::Value`。
2. **登录页顶部大量空白**：`mount_to_body` 把内容挂到 `<body>`，而 CSS 的 `#app{height:100%}` 作用在**空的** `#app` div 上（占满视口、把内容挤到下方）。改为 `mount_to(#app)`（`leptos::mount::mount_to(el, App).forget()`，el 需 `unchecked_into::<web_sys::HtmlElement>()`）。
3. **窄窗口无入口**：<640px 侧栏隐藏，而"新建群组/设置"按钮只在侧栏头部 → 顶栏新增 `+`/齿轮按钮（`.nav-action`，所有宽度可见，不隐藏）；<1024px 时设置视图占满主区域 + 返回按钮（`.settings-back`）。
4. **建群改用内联对话框**：不依赖 `window.prompt`（Android WebView 下 prompt 不可靠）。
5. **Peer ID 显示乱码**：历史遗留 —— 曾用 PS 5.1 `Get-Content/Set-Content`（默认 GBK 读写）批量改源码，损坏 `app.rs`/`api.rs` 的非 ASCII 字符（`…`/`·`/中文注释）。整体重写为干净 UTF-8（见坑 18）。

## 目标

用 Tauri 2 把 `zoe-core`（E2E/MLS）+ `zoe-transport` 的 MeshOverlay（分片/重组/去重/TTL）
集成进手机 App，替代纯 Kotlin 方案 B（`android-old/` 保留作后备）。
最终真机验证：Windows `zoe-cli ble connect` ↔ 手机 App 全栈 MeshOverlay 互通。

## 架构总览（定稿）

> ⚠️ 下图与 M0 文件清单为 **2026-08-19 之前的旧架构（Vite 控制台 UI）**，已被 §0"Rust Web UI 与内嵌守护进程"取代：
> WebView 加载内嵌守护进程的 `http://127.0.0.1:18571/`（同一份 Rust wasm UI），vite/package.json/src/main.js 已删除，
> 前端构建不再使用 npm。桥（bridge.rs ↔ Kotlin BLE）与 M1 回环 TCP 协议**保持有效**（移动端 BLE 物理层不变）。

```
┌────────────────────────── Android 真机 ──────────────────────────┐
│                                                                  │
│  WebView(Vite 控制台 UI, release 时嵌入 APK assets)              │
│   ├─ invoke: app_info / hello_frame / start_bridge / stop_bridge │
│   │          / set_echo / send_message(M2)                       │
│   └─ listen: bridge-log(M1) / inbound(M2)                        │
│                              │ tauri::command(无 JNI)            │
│  ┌──────────────────────────▼───────────────────────────┐        │
│  │ Rust: zoe-core + zoe-transport(MeshOverlay)          │        │
│  │  ├─ commands(同上)                                    │        │
│  │  ├─ bridge.rs   : tokio TcpListener 127.0.0.1:18570  │        │
│  │  │               └─ 协议编解码 + 桥状态机(M1)         │        │
│  │  └─ mobile_driver.rs: BleDriver 实现(M2)             │        │
│  │     MeshOverlay<MobileDriver>  ←→ bridge 消息         │        │
│  └──────────────────────────┬───────────────────────────┘        │
│                             │ 127.0.0.1 回环 TCP,每行一个 JSON   │
│  ┌──────────────────────────▼───────────────────────────┐        │
│  │ Kotlin: Bridge(Application 级单例)                    │        │
│  │  ├─ Socket 客户端:连 Rust,协议编解码,断线 2s 重连      │        │
│  │  ├─ ZoeAdvertiser.kt 广播 zoe 服务                    │        │
│  │  └─ ZoeBleServer.kt GATT server(写/通知 + echo)       │        │
│  └──────────────────────────────────────────────────────┘        │
└──────────────────────────────────────────────────────────────────┘
                                │ BLE GATT(写 7a5e0002…/通知 7a5e0003…)
┌───────────────────────────────▼──────────────────────────────────┐
│ Windows: zoe-cli ble scan / connect(已验证 central)              │
└──────────────────────────────────────────────────────────────────┘
```

分层原则：**Kotlin 只管 BLE 物理层，Rust 管帧/MeshOverlay/MLS 全部逻辑**。
Kotlin 侧零逻辑（仅透传字节 + 生命周期），Rust 侧零平台依赖（android 目标可编译）。

## 已定架构（勿改，除非有充分理由）

- 新工程 **`app/`**：Tauri 2 mobile。**独立于根 cargo workspace**（`app/src-tauri/Cargo.toml`
  内放空 `[workspace]` 表隔离；自带独立 Cargo.lock，由 CI/有网络机器生成后提交）。
- **Rust 核心**（`app/src-tauri/src`）：嵌入 `zoe-core` + `zoe-transport`
  （需给 zoe-transport 加 feature **`ble-mobile = []`**，并把 lib.rs 门控放开：
  `#[cfg(any(feature = "ble-linux", feature = "ble-windows", feature = "ble-mobile"))] pub mod ble;`
  —— ble 模块的帧/MeshOverlay 是纯逻辑，android 目标可编译）。
- **BLE 传输**：Kotlin 侧**复用已验证的** `ZoeBleServer.kt` / `ZoeAdvertiser.kt` / `ZoeFrame.kt`
  （canonical 源码在 **`android/`**，由 `scripts/sync-android-ble.sh|.ps1` 同步进 Tauri 生成的
  `app/src-tauri/gen/android/...` 工程，勿在 gen/android 手改）。Rust ↔ Kotlin 经
  **127.0.0.1 回环 TCP** 桥接（Kotlin 起 socket 客户端连 Rust 的 tokio TcpListener）：
  - Kotlin→Rust：`{"t":"frame","a":"<mac>","d":"<hex>"}`、`{"t":"log","d":"..."}`
  - Rust→Kotlin：`{"t":"start"}`、`{"t":"stop"}`、`{"t":"send","a":"<mac>","d":"<hex>"}`、`{"t":"echo","v":bool}`
  - 理由：零 JNI，规避 Tauri 移动插件桥（`mobile_entry_point` 插件宏）的学习曲线与 CI 反复。
- **前端**：Vite + 原生 JS 控制台 UI（`@tauri-apps/api` 的 `invoke`/`listen`）。
- **构建：仅走 CI**（本地沙箱无 crates.io 网络，装不了 cargo-ndk / tauri CLI / Android targets）。
  新 workflow **`.github/workflows/android-tauri.yml`**，APK 从 Actions 下载。
  （现有 `.github/workflows/android.yml` 是方案 B Kotlin APK 构建，继续构建 `android-old/`，保留不动。）

## 里程碑

| 里程碑 | 内容 | 验收 |
|---|---|---|
| M0 | Tauri android 骨架；zoe-core/zoe-transport 编进 APK；`hello_frame` 冒烟命令（帧 构造→解析 回路） | CI 出 APK，装机能显示版本+帧 hex |
| M1 | 回环 TCP 桥 + Kotlin BLE 服务接入（广播+GATT+echo） | 功能等价现有 APK；Windows `ble connect --send` 回显 |
| M2 | MeshOverlay + zoe-core 全栈：大信封分片重组、去重/TTL、E2E | Windows ↔ 手机 全栈互通（消息收发） |

执行顺序 M0 → M1 → M2，每步独立可验收；M1 完成即替代现有 APK 的联调角色。

## 前置改动：zoe-transport 支持 android 目标（M0 第一步）

1. `crates/zoe-transport/Cargo.toml` `[features]` 增加一行：

   ```toml
   ble-mobile = []
   ```

   （纯空 feature：只放开 `ble` 模块编译；`ble/mod.rs` 内 linux/windows 子模块
   已有 `#[cfg(all(feature = "ble-linux", target_os = "linux"))]` 等门控，android
   目标下不会编译。mod.rs 其余代码只依赖 tokio/zoe-core/uuid/hex，可编译。）
2. `crates/zoe-transport/src/lib.rs` 门控放开：

   ```rust
   #[cfg(any(feature = "ble-linux", feature = "ble-windows", feature = "ble-mobile"))]
   pub mod ble;
   ```

3. 验证（CI 上自动做）：`cargo check --no-default-features --features ble-mobile --target aarch64-linux-android`。
   注意 ble 模块引用了 `crate::{Availability, Inbound, Transport, TransportError}`，
   这些与 net/libp2p 无关，`--no-default-features` 下仍可用。

## M0 文件清单

```
app/package.json            # name zoe-mobile;deps: @tauri-apps/api ^2;devDeps: @tauri-apps/cli ^2, vite ^6
app/package-lock.json       # 已提交(本地经 npm 镜像代理生成)
app/vite.config.js          # port 1420, strictPort
app/index.html              # 控制台 UI 骨架
app/src/main.js             # invoke("app_info") / invoke("hello_frame") / listen 日志
app/src/style.css
app/src-tauri/Cargo.toml    # [lib] crate-type = ["staticlib","cdylib","rlib"]; tauri 2;
                            # zoe-core path 依赖;zoe-transport path 依赖
                            #   (default-features = false, features = ["ble-mobile"] —— 排除 libp2p,瘦身+省编译);
                            # hex;serde/serde_json;空 [workspace]
app/src-tauri/Cargo.lock    # 由 CI 生成(本地无 crates.io 网络);有网机器生成后可提交
app/src-tauri/build.rs      # tauri_build::build()
app/src-tauri/tauri.conf.json  # identifier com.zoechat.mobile;frontendDist ../dist;
                            # beforeDevCommand npm run dev(android 配置在 tauri.android.conf.json,
                            # 不在 tauri.conf.json —— 顶层 android 键会被 schema 拒绝);
                            # app.windows 必须有 label:"main" 的窗口(空数组=Android 不创建
                            # WebView,装机能启动但黑屏 —— 实测,见关键坑 17)
app/src-tauri/tauri.android.conf.json  # { "bundle": { "android": { "minSdkVersion": 26 } } }
                            # 注意:平台覆盖文件名是 tauri.android.conf.json(与主配置同结构合并),
                            # 不是 tauri.android.json —— 写错文件名会静默不生效(minSdk 回退 24)
app/src-tauri/capabilities/default.json  # permissions: ["core:default"]
app/src-tauri/icons/        # 全套图标已入库(icon.png 1024² 源图 + tauri icon 生成的各尺寸/android|ios)
app/src-tauri/src/main.rs   # fn main() { zoe_mobile_lib::run() }
app/src-tauri/src/lib.rs    # #[cfg_attr(mobile, tauri::mobile_entry_point)] pub fn run();
                            # commands: app_info / hello_frame(复用 zoe_transport::ble 同一份代码)
app/src-tauri/gen/android/  # CI 生成,不入库(gitignore);构建前缺失时 npx tauri android init
.github/workflows/android-tauri.yml
```

### M0 执行步骤

1. **zoe-transport 前置改动**（上文），本地 `cargo check` 确认桌面 target 不回归。
2. **搭 app/ 骨架**：`npm create tauri-app`（或手工按清单建文件）→ `npx tauri android init`。
   gen/android 生成后立即 git 提交（否则 CI 上 Kotlin 工程不完整）。
3. **写两个 command**：
   - `app_info() -> AppInfo{app_version, zoe_core_version, frame_example_hex}`：
     app 版本取 `env!("CARGO_PKG_VERSION")`；zoe-core 版本用常量
     `pub const ZOE_CORE_VERSION: &str = "0.1.0"`（随根 workspace 升版同步；两处一致由提交时人工核对）。
     `frame_example_hex`：`frame_chunks([0x42;8], 3, b"zoe-mobile")` 第一帧的 hex。
   - `hello_frame() -> String`：`frame_chunks` 构造 → `parse_frame` 解析 → 返回
     `帧hex + 解析摘要(msg_id/ttl/chunk/total/data 长度)`，两端结果一致才算过。
4. **写 CI workflow**（见"CI 规格"），push 出 APK。
5. **验收**（见下）。

### M0 验收

1. Actions `android-tauri.yml` 全绿，`zoe-mobile-apk` artifact 可下载（aarch64 真机 APK）。
2. 装真机打开 App：显示 `zoe-mobile v0.1.0 / zoe-core 0.1.0` 与示例帧 hex。
3. 点"帧回路测试"：显示 构造hex == 解析hex 且摘要字段与预期一致
   （msg_id=4242…42, ttl=3, chunk 1/1, data="zoe-mobile"）。

## M1：回环 TCP 桥 + Kotlin BLE 接入

### 桥协议完整规格（定稿，M1/M2 共用）

- 传输：TCP 回环。Rust `tokio::TcpListener` 只绑 `127.0.0.1:18570`（不暴露外部端口）。
  Kotlin 侧 socket 客户端**主动连接**；断线后每 2s 重连（幂等，可随时重连）。
- 成帧：**每行一个 JSON 对象**（newline-delimited，UTF-8；所有字节载荷一律 hex 小写）。
  任一端收到无法解析的行：丢弃该行 + 本端记日志，不中断连接。
  未知消息类型：忽略 + 记日志（向前兼容）。
- 消息表（M1 用 hello/frame/log/start/stop/send/echo；conn/disc 是 M2 扩展）：

| 方向 | 消息 | 字段 | 说明 |
|---|---|---|---|
| K→R | hello | `{"t":"hello","v":1}` | 连接建立后 Kotlin 立即发；Rust 收到后桥状态置 Connected，发 `bridge-log` |
| K→R | frame | `{"t":"frame","a":"<mac>","d":"<hex>"}` | BLE 收到完整帧字节（含 13B 头）；mac 为 `AA:BB:CC:DD:EE:FF` |
| K→R | log | `{"t":"log","d":"<text>"}` | 任意日志行，Rust 转发为 `bridge-log` 事件 |
| K→R | conn | `{"t":"conn","a":"<mac>"}` | (M2) GATT 客户端连上（onConnectionStateChange CONNECTED） |
| K→R | disc | `{"t":"disc","a":"<mac>"}` | (M2) GATT 客户端断开 |
| R→K | start | `{"t":"start","n":"<广播名>"}` | 启动广播+GATT server；`n` 可选，缺省 `zoe-device` |
| R→K | stop | `{"t":"stop"}` | 停止广播+GATT server |
| R→K | send | `{"t":"send","a":"<mac>","d":"<hex>"}` | 向指定已连接设备发通知帧 |
| R→K | echo | `{"t":"echo","v":bool}` | 设置 server 端自动回显（对应 `ZoeBleServer.setEcho`） |

- 生命周期：`start`/`echo` 幂等可重复发送；`stop` 后 Kotlin 停止 server 但**保持 TCP 连接**；
  桥退出时 TCP 关闭，Kotlin 2s 重连循环自然结束（App 进程退出即止）。

### M1 新增文件

```
android/app/src/main/java/com/zoechat/ble/     # canonical Kotlin 源码(本仓库根下,已就绪)
    ZoeFrame.kt                # 从 android-old/ 原样拷贝(package com.zoechat.ble 不变)
    ZoeBleServer.kt            # 从 android-old/ 原样拷贝
    ZoeAdvertiser.kt           # 从 android-old/ 原样拷贝
    Bridge.kt                  # 新增:回环 TCP 桥客户端(object Bridge)
scripts/sync-android-ble.sh    # 同步脚本(CI ubuntu):android/ ble/*.kt → gen/android
scripts/sync-android-ble.ps1   # 同步脚本(Windows 本地)
app/src-tauri/src/bridge.rs    # TcpListener + 协议编解码 + 桥状态机 + 事件发射
app/src-tauri/src/lib.rs       # +commands: start_bridge/stop_bridge/set_echo/bridge_status
app/src-tauri/gen/android/app/src/main/java/com/zoechat/ble/   # 同步产物(勿手改;由 sync 脚本覆盖)
app/src-tauri/gen/android/app/src/main/java/com/zoechat/mobile/MainActivity.kt  # 修改(Tauri 生成)
app/src-tauri/gen/android/app/src/main/AndroidManifest.xml             # 修改(加权限)
```

Kotlin 改动流程：只改 `android/` 下源码 → 跑 sync 脚本覆盖进 gen/android →
CI 构建前再跑一次（见 CI 规格）。MainActivity/Manifest 例外：它们由 tauri CLI
生成/管理，直接在 gen/android 里改（不入 `android/`）。

### M1 Rust 侧设计（bridge.rs 要点）

- `pub enum BridgeState { Disconnected, Connected }`；单例 `Mutex<BridgeState>` + 命令通道。
- 命令实现（tauri command，全部异步、幂等）：
  - `start_bridge()`：若未监听则 `TcpListener::bind("127.0.0.1:18570")` 起 accept 循环；
    accept 到连接后等待 Kotlin 的 `{"t":"hello","v":1}`（超时 5s 未到则断开等重连），
    收到后状态置 Connected；重复调用不重复起监听。**另发 `{"t":"start"}` 启动 BLE**：
    未连接时先记 start_requested，hello 到达后补发（否则点"启动 BLE 服务"后 BLE 不启动）。
  - `stop_bridge()`：发 `{"t":"stop"}` 给 Kotlin（连接存在时），不关 TCP。
  - `set_echo(v: bool)`：发 `{"t":"echo","v":...}`。
  - `bridge_status() -> BridgeStatus{connected: bool, last_error: Option<String>}`。
  - 命令放 `bridge` 模块时可见性用 `pub(crate)`（坑 16 只禁 `pub`；pub(crate) 无 E0255）。
- Kotlin 的 `log` 消息与本地桥状态变化 → `app.emit("bridge-log", line)`。
- 写失败/连接关闭：状态置 Disconnected，发 `bridge-log`"桥断开，等待 Kotlin 重连"；
  accept 循环继续（Kotlin 会重连，无需 Rust 侧重连逻辑）。

### M1 Kotlin 侧设计（`android/app/src/main/java/com/zoechat/ble/Bridge.kt` 要点，已就绪）

- `object Bridge`（Application 级单例，MainActivity.onCreate 里 `Bridge.start(this)`；
  不引入 Service —— 桥生命周期=进程生命周期，后台保活列为 M3 未来项，与现有 APK 等价）。
- 线程模型：`Thread { socket 循环 }`，主线程 Handler 投递 BLE 回调（沿用
  ZoeBleServer 现有"binder 线程 → main.post"模式）。
- 消息映射：
  - `onLog` → `{"t":"log","d":...}`；`onFrame` → `{"t":"frame","a":device.address,"d":hex(raw)}`
  - 收 `start` → `ZoeAdvertiser(this).start()` + `ZoeBleServer(this, listener).start()`，
    失败发 log；收 `stop` → 停两者；收 `send` → `server.sendNotification(按 a 找 device, hex 解码)`；
    收 `echo` → `server.setEcho(v)`。
- 重连：每 2s `Socket("127.0.0.1", 18570)`，连接成功后先发 `hello`。
- AndroidManifest 增加（与 android-old/ 工程一致）：

  ```xml
  <uses-permission android:name="android.permission.BLUETOOTH_SCAN"/>
  <uses-permission android:name="android.permission.BLUETOOTH_CONNECT"/>
  <uses-permission android:name="android.permission.BLUETOOTH_ADVERTISE"/>
  <uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" android:maxSdkVersion="30"/>
  <uses-feature android:name="android.hardware.bluetooth_le" android:required="true"/>
  ```

  运行时权限申请放 MainActivity（照抄现有 MainActivity 的 checkPermissions 流程，Tauri
  WebView 之外再加一个"启动桥"按钮，或直接 onCreate 自动申请+启动）。
- 设备查找：`send` 的 mac → `BluetoothAdapter.getRemoteDevice(mac)`（BLE 客户端必然已连接，
  GATT server 侧可查到）。

### M1 验收

1. 新 APK 装真机，点"启动 BLE 服务"：其他设备（手机 nRF Connect / Windows scan）能看到
   `zoe-device` 广播，可连上 GATT（服务/写/通知 UUID 齐全，CCCD 可订阅）。
2. Windows：`zoe-cli ble scan` 看到手机 → `zoe-cli ble connect <MAC> --send 5a010203040506070809000001ab`
   （合法帧：magic 5a + msg_id 8B + ttl 09 + 片 00 + total 0001 + 数据 ab），收到 echo 通知帧，
   手机日志显示"帧 msg_id=0102030405060708 ttl=9 片 1/1 数据 1B"（与现有 APK 行为一致）。
3. App 控制台日志区实时显示 `[收]/[发]` 行 —— 证明 K→R→UI 全链路通
   （Kotlin log → bridge-log → UI）。
4. App 内 echo 开关能即时生效（`zoe-cli ble connect` 不再回显）。

## M2：MeshOverlay + zoe-core 全栈

### M2 新增文件

```
app/src-tauri/src/mobile_driver.rs   # BleDriver 实现:协议经 bridge 通道转发
app/src-tauri/src/lib.rs             # +M2: MeshOverlay 装配 + commands: send_message / inbound 事件
crates/zoe-cli/src/ble.rs            # 小改:--send-env 与收包重组打印(见下)
```

### M2 Rust 侧设计（mobile_driver.rs 要点）

- `MobileDriver`（`BleDriver` impl，`Conn = MobileConn`）：
  - 内部持 bridge 发送通道（向 Kotlin 发消息）+ 每 MAC 读队列表。
  - `start_advertising(name)` → 发 `{"t":"start","n":name}`；`stop_advertising` → `{"t":"stop"}`。
  - `scan(_)` → `Ok(vec![])`：**M2 手机无 central 能力**（BluetoothLeScanner/GattClient 未接入，
    记录为已知限制，M3 再做；MeshOverlay 的 scan_loop 每 5s 空转无害）。
  - `connect(_)` → `Err("mobile: central 未实现")`。
  - `listen()` → 返回 `mpsc::Receiver<MobileConn>`；bridge 收到 `conn` 消息时构造
    MobileConn（含该 MAC 的读队列）投递；收到 `disc` 时关闭对应读队列（读返回 None）。
- `MobileConn`：`write(frame)` → 发 `{"t":"send","a":mac,"d":hex(frame)}`；
  `read()` → 读队列 recv；`peer_addr()` → mac。
- bridge 收到 `frame` 消息时：查读队列表 → 投递原始帧字节（M2 起 K 的 frame 直接喂 MeshOverlay）。
- 配套 Kotlin 小改：`ZoeBleServer.Listener` 增加 `onConn(device)` / `onDisc(device)`，
  Bridge 在 `onConnectionStateChange` 里转发 `conn`/`disc` 消息（其余逻辑零改动）。
- 装配：`MeshOverlay::spawn(MobileDriver::new(...), "zoe-device", ttl=3)`；
  `subscribe()` 的 inbound → `app.emit("inbound", ...)`（信封 hex + from）；
  UI `send_message(text)` → `MlsSession.encrypt` → `Envelope::new` → `overlay.send("*", env)`。

### M2 前置小改：zoe-cli（Windows 侧联调工具，必做）

`zoe-cli ble connect` 目前只能发裸 hex 帧。M2 验收需要信封级收发，给 `ble connect` 增加：

- `--send-env <TEXT>`：`Envelope::new(0, MSG_PRIVATE, b"g1", 0, 0, seq, TEXT 原文)` →
  `frame_chunks` → 逐帧发送（明文信封，只验证 分片/重组/去重/TTL 传输栈）。
- 收包侧：按 msg_id 重组 → `Envelope::decode` → 打印信封摘要 + payload
  （复用 `zoe_transport::ble::{frame_chunks, parse_frame}`，参照 mod.rs 内重组逻辑）。

### M2 验收

**M2a（传输栈，先做）**：
1. 手机 App 启动 BLE 服务（桥已连）；Windows `zoe-cli ble connect <MAC> --send-env <文本>`
   （文本 >499B 强制分片）→ 手机 UI 收到一条 inbound：信封 hex 完整、payload 与原文一致。
2. 手机 UI 发消息 → Windows 侧打印重组后的信封摘要一致。
3. 同一条信封连发两次 → 手机 UI 只显示一次（dedup 生效）。
4. 手机侧把 echo 关闭，Windows 直连期间 App 无异常（TTL/超时重组清理无泄漏——观察日志无 panic）。

**M2b（E2E/MLS，完整验收）**：
1. 手机 App 演示模式：启动生成身份 + 建组（b"g1"），UI 显示 fingerprint 与 key package hex。
2. Windows `zoe-cli` 增加 `ble mesh --kp <hex>` 小工具：导入手机 key package →
   `add_member` → welcome 经 BLE 信封发手机 → 手机 `join` → 双向加密消息。
3. 验收：两端加密消息互收，`Processed::Message` 明文一致；重复投递被去重；
   杀 App 重启后组状态不丢（存储接入）——存储接入若工期紧，M2b 可先内存态，标注为后续项。

## CI 规格（.github/workflows/android-tauri.yml，M0 已交付）

要点（与仓库内实际文件一致）：

- **复用验证**：构建前先 `cargo test -p zoe-transport --no-default-features --features ble-mobile --lib`
  —— 移动侧与 Linux 侧同一份 ble 模块（帧/分片/重组/去重/存储转发 5 个单测）在 host 全绿。
- **gen/android 不入库**：CI 构建前 `if [ ! -d src-tauri/gen/android ]; then npx tauri android init; fi`
  生成（本地无 Android cmdline-tools、dl.google.com 不可达，无法本地生成）。
- **图标已入库**：本地 `npx tauri icon` 生成全套（icons/ 含 android|ios 子目录）后提交，
  CI 不再生成。
- **APK 验证**：`unzip -l` 断言 native lib（zoe-core/zoe-transport 编入证明）+ `aapt dump badging`
  （包名/版本/minSdk/ABI）。
- **结果回传**：GitHub API 在沙箱不可达，CI 把结果（job 状态 + badging + sha256 + 失败时构建日志尾部）
  提交到 **`ci/report`** 分支（force push），本地 `git fetch origin ci/report` 即可验收；
  artifact 名 `zoe-mobile-apk`（Actions 页下载）。
- cargo 命令**均不用 `--locked`**（沿用坑 6）；CI 以 `--apk --split-per-abi` 出 **4 个分 ABI release APK**（arm64-v8a / armeabi-v7a / x86 / x86_64，减小体积，真机只装对应 ABI；产物 `app/build/outputs/apk/<abi>/release/*.apk`）。tauri 默认即 release 构建，无 `--release` 参数。**release 签名坑（2026-08-20）**：tauri 模板 release buildType 不设 signingConfig，产出 APK 未签名，侧载报 `packageInfo is null / 未找到签名`；修复 = `scripts/patch-android-gen.sh` 向 `app/build.gradle.kts` 末尾**追加** release 签名块（不能 `apply(from=...)` 独立脚本：Kotlin DSL 跨脚本无 `android{}` 访问器，报 `Unresolved reference: android`），用仓库内 `android/zoe-ci-release.keystore`（开发签名，非生产密钥）签名 release；正式发布由发布者另配 keystore 替换。

## 关键坑（实测结论，勿重走）

1. **本地构建与网络（2026-08-19 更新）**：crates.io **可达**（`cargo search/fetch/install` 正常）；
   **rustup 分发服务器不可达**，装 target 需镜像：`RUSTUP_DIST_SERVER=https://rsproxy.cn RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup rustup target add wasm32-unknown-unknown`。
   旧问题（bluer bluetoothd 依赖离线解析失败）仅在断网时存在：临时去掉
   `crates/zoe-transport/Cargo.toml` 里 bluer 的 `features = ["bluetoothd"]`，构建后恢复（勿提交）。
2. **图标**：本地 `npx tauri icon src-tauri/icons/icon.png` 可跑（npm 走镜像代理），
   全套图标（含 android/ios 子目录）直接入库，CI 不再生成。
3. **NDK**：CI 用 `sdkmanager "ndk;27.0.12077973" --sdk_root="$ANDROID_HOME"`（配合
   android-actions/setup-android@v3）；另装 platforms;android-34/35 + build-tools;34.0.0（aapt 用）。
4. **gen/android 不入库**：本地无 cmdline-tools（dl.google.com 被代理拦），`tauri android init`
   本地跑不了；CI 构建前缺失时生成（幂等守卫）。M1 起 `com/zoechat/ble/*.kt` 为**同步产物**
   （canonical 在 `android/`），由 `scripts/sync-android-ble.sh|.ps1` 覆盖，CI 在 init 之后、build 之前同步。
5. **git push**：需 `-c http.proxy=http://127.0.0.1:58591` 且工作区写权限不够时提权（Git Credential Manager）。
6. CI 的 cargo 命令**均未用 --locked**，runner 自动补锁；锁文件最终在有网络机器上
   `cargo build -p zoe-transport --features ble-linux` 生成并提交。
7. **app/ 的 Cargo.lock**：本地无 crates.io 网络，由 CI 生成（首次 `tauri android build` 时）；
   `tauri android build` 会下载 gradle distribution + AGP 插件（CI 有网）。
8. **回环 TCP 只绑 127.0.0.1**：Android 真机回环可用（模拟器才是 10.0.2.2，本项目只验收真机）；
   不绑 0.0.0.0，避免暴露端口。
9. **zoe-transport 依赖瘦身**：app 里用 `default-features = false, features = ["ble-mobile"]`，
   排除 libp2p（移动端不需要 net），APK 更小、android 编译面更小。
10. **M1 不用前台 Service**：桥放 Application 级单例 + Activity 生命周期，与现有 APK 等价
    （免 FOREGROUND_SERVICE / POST_NOTIFICATIONS 权限）；后台保活列为 M3 未来项。
11. **Kotlin 包名不改**：拷入的三个 .kt 保持 `package com.zoechat.ble`，与 applicationId
    `com.zoechat.mobile` 不冲突，省去改包名风险。
12. **echo 与 MeshOverlay 互斥**：M2 启动 MeshOverlay 后必须 `set_echo(false)`，
    否则 Kotlin 的 echo 与 Rust 重组/去重同时处理同一帧（日志会乱，去重仍兜底但不干净）。
13. **平台配置文件名与结构**：android 配置在 `src-tauri/tauri.android.conf.json`
    （与主配置同结构合并，`{ "bundle": { "android": { "minSdkVersion": 26 } } }`）；
    tauri.conf.json 顶层 `android` 键被 schema 拒绝（init 报
    `Additional properties are not allowed ('android' was unexpected)`）；
    文件名写错（如 `tauri.android.json`）会**静默不生效**，minSdk 回退默认 24
    （用 `aapt dump badging` 验证 sdkVersion 确认生效）。
14. **npm 本地可用（镜像代理）**：`npm_config_proxy=http://127.0.0.1:58591` + 默认
    registry.npmmirror.com 可达 → 可本地 `npm install`、`npx tauri icon`；但 esbuild/tauri 的
    postinstall 在沙箱会 EPERM（子进程管道限制），用 `--ignore-scripts`（平台包自带二进制，不影响）。
    注意 npm 全局缓存被 DSH 占用，需 `--cache <项目内目录>`。
15. **CI 结果经 git 回传**：沙箱无法访问 GitHub REST API（代理只放行 git 与 npm 镜像），
    验收走 `ci/report` 分支（CI force push 结果文件），`git fetch origin ci/report` 读取。
16. **`#[tauri::command]` 函数不要加 `pub`**：pub 会使宏生成的 `__cmd__*` 名称被再导出，
    rustc 报 `E0255: the name __cmd__xxx is defined multiple times`（android 目标编译 lib 时实测）；
    命令函数保持模板默认的私有可见性即可（同 crate 内 test 仍可访问）。
    **跨模块（M1 起 bridge.rs）用 `pub(crate)`**：宏源码（tauri-macros wrapper.rs）对
    `pub`/Restricted 可见性都加 `#[macro_export]`（crate root 定义一次），但 `pub(crate) use`
    在模块内不产生 crate-root 重复定义 → 无 E0255；crate root 上的 `pub` 才会撞。
17. **`app.windows` 与窗口创建顺序（2026-08-19 更新）**：旧结论"不能为空数组"成立的前提是**没有任何代码创建窗口**（tauri 遍历 config 创建 WebView，空数组=永不创建 → 黑屏）。
    **Rust Web UI 方案（§0）改为在 setup 内手动创建**：先 `block_on(zoe_daemon::start(...))` 起服务，再
    `tauri::WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url)).build()` —— 此时
    `app.windows: []` 是**正确配置**（避免 tauri 先建窗口加载空地址）；窗口 label 仍须与 capabilities 的
    `windows: ["main"]` 对应（invoke/listen 的 ACL 校验）。
18. **PS 5.1 默认编码会损坏 UTF-8 源码（实测，代价惨重）**：`Get-Content/Set-Content` 不带 `-Encoding`
    时按系统 ANSI（中文系统=GBK）读写；对含中文/`·`/`…` 的 UTF-8 文件做批量替换后文件变成乱码
    （多层往返不可逆）。**规则：Rust 源码一律用专用工具（本会话的 write/edit）或
    `[System.IO.File]::ReadAllText($f,[Text.Encoding]::UTF8)` + `WriteAllText($f,$s,(New-Object Text.UTF8Encoding($false)))`**；
    改完用 `Select-String -Pattern "Â|Ã|â‚¬"` 自查。
19. **`request<T>` 泛型反序列化**：服务端 `{"ok":true}` 类响应**不能**用 `()` 接收（`()` 只认 null），
    webui 的 `request` 一律显式 `let _: serde_json::Value = request(...).await?`（login 曾因此误报"invalid token";
    login 已随令牌移除而删除,2026-08-20）。
20. **Leptos 挂载目标**：`mount_to_body` 把内容追加到 `<body>` 末尾；若 CSS 布局依赖 `#app{height:100%}`，
    必须 `mount_to(#app)`（`get_element_by_id("app")` 后 `unchecked_into::<web_sys::HtmlElement>()`），
    否则空的 `#app` 占满视口、真实内容被挤到首屏之外。

## M0 验收记录（2026-08-17）

CI（android-tauri.yml）run 32027550351（commit de4df18）**成功**，报告经 `ci/report` 分支回传：

- APK：`app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`
  （debug 通用包 520MB/4 ABI；release 会小得多），sha256 `3729608f…`
- native libs：`lib/arm64-v8a|armeabi-v7a|x86|x86_64/libzoe_mobile_lib.so`
  —— zoe-core + zoe-transport(ble-mobile) 编入 APK 的证明
- badging：`package='com.zoechat.mobile' versionName='0.1.0'`，label `zoe-mobile`，
  targetSdk 36；**sdkVersion=24**（首轮发现 minSdk 配置未生效 → 已修 `tauri.android.conf.json`，
  后续轮 badging 应为 26）
- artifact：`zoe-mobile-apk`（Actions 页下载）；`zoe-transport` 共享 ble 模块单测 5/5 绿
  （复用验证）
- 本地验证（离线）：`cargo check/test -p zoe-transport --no-default-features --features ble-mobile` 通过
- **真机验收（2026-08-17，小米 Android 15/16，adb uiautomator dump 确认）**：
  打开 App 显示 `zoe-mobile v0.1.0 / zoe-core 0.1.0` + 示例帧 hex；点"帧回路测试"显示
  `构造→解析 OK / 帧 hex 5a424242… / msg_id=4242424242424242 ttl=3 片 1/1 数据 16B` —— 验收 2、3 项通过。
  首版 APK 曾黑屏：根因 `tauri.conf.json app.windows: []`（tauri 遍历 windows 配置创建 WebView，
  空数组=永不创建 → content 视图为空），修复为 `label:"main"` 窗口后正常（见关键坑 17）。

M0 已闭环，下一里程碑：**M1（回环 TCP 桥 + Kotlin BLE 接入）**。

## 现有资产（可直接复用）

- `android/app/src/main/java/com/zoechat/ble/`：**canonical Kotlin 源码** ——
  ZoeFrame.kt / ZoeBleServer.kt / ZoeAdvertiser.kt（零依赖、已验证：CCCD 显式描述符 +
  onDescriptorWriteRequest 应答均已修好）+ Bridge.kt（M1 回环 TCP 桥客户端，已就绪）
- `android-old/`：旧纯 Kotlin APK 工程（方案 B 后备，其 CI 继续出 APK）
- `crates/zoe-transport/src/ble/mod.rs`：`frame_chunks` / `parse_frame` / `MeshOverlay<D: BleDriver>`
- `zoe-cli ble scan/connect`（Windows，已验证）+ `tools/ble-gatt-test/`（Chrome 备用）

## 相关文档

- `docs/termux-ble.md`：GATT 服务/特性 UUID 规范、角色矩阵、方案 A'（手机 peripheral）联调流程
- `docs/envelope.md`：信封格式与 BLE 帧格式（§2.1）
- `docs/DESIGN.md` §6.2：MeshOverlay 设计
- `crates/zoe-cli/src/ble.rs`：Windows central 联调命令实现（M2 小改的落点）
