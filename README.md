# zoe-chat

轻量化端到端加密消息系统：Linux/Windows 本地守护进程 + Web UI，蓝牙近场网状直连，路径穿越实现去中心化远程通信。群组加密基于 MLS（RFC 9420，openmls）。

- 设计文档：[DESIGN.md](DESIGN.md) 及 `docs/`（envelope 格式、SQLite schema、会话协议、API 契约、模块布局）
- 移动端（Tauri Android）设计与验收：[docs/tauri-mobile.md](docs/tauri-mobile.md)

## 当前状态

**M0（骨架验证）✅**
- `zoe-core`：身份层（Ed25519 + BIP39 助记词，PIN 派生密钥加密身份种子）、统一信封编解码、openmls 封装、多用户注册表
- `zoe-transport`：Transport trait + loopback 传输
- `zoe-cli`：`init` / `fingerprint` / `demo`（双节点消息、群组、update）/ `user`（多用户管理）

**M2（近场）✅ 驱动待真机验证**
- `BleDriver` trait + `MeshOverlay`：BLE GATT 网状覆盖（分片/重组、去重、TTL 存储转发），mock 驱动测试通过
- 驱动：bluer（Linux，CI 编译验证）、btleplug + windows-rs（Windows，本机编译验证）
- 角色：Linux 全角色（广播 + GATT 服务端 + 扫描/连接）；Windows 仅 central（扫描/连接）——广播与 GATT 服务端均需 UWP 包标识，桌面二进制不可用（0x80070057，已实测确认）
- 真机联调工具：`zoe-cli ble adv|scan|connect|diag`（Linux 全角色 / Windows central）、`scripts/termux/`（Android 扫描/构建）、`tools/ble-gatt-test/`（Web Bluetooth GATT 测试页），流程见 [docs/termux-ble.md](docs/termux-ble.md)

**M3（远程）✅**
- libp2p 远程通道：手动拨号 + mDNS 发现 + DCUtR 打洞，noise 用身份密钥（与 QR 名片同钥）
- 守护进程消息核心：入站信封分发、KeyPackage 交换、邀请流程、消息路由
- **双守护进程 E2E 通过**：建群 → 邀请 → 双向加密消息，双方群组状态一致

**M1（Tauri 移动端）✅**（详情见 [docs/tauri-mobile.md](docs/tauri-mobile.md)）
- Android APK（Tauri 2）：回环 TCP 桥（Bridge.kt）+ Kotlin BLE（广播/GATT/echo）+ 内嵌 zoe-daemon（`127.0.0.1:18571`）+ Rust/Leptos wasm UI
- GitHub Actions 产出 APK：`Android Tauri Build` 工作流，产物在 Actions artifact，验收报告回传 `ci/report` 分支

## 多用户与 PIN 保护

单个守护进程可服务多个用户（每用户独立数据目录 `data_dir/users/<id>/`，含各自的 `zoe.db`/`mls.db`）：

- 创建用户时设置 **PIN**；身份种子以 `argon2id(PIN) → ChaCha20-Poly1305` 加密落盘（`salt||nonce||ciphertext`），默认**不落明文种子**
- 守护进程启动带 `--pin` 即解锁；否则以"锁定模式"启动（仅用户管理与解锁 API，其余返回 423）
- 切换用户：`zoe-cli user activate <id>` 后重启守护进程（v1 约定）
- 旧版明文数据自动迁移为 `default` 用户（无 PIN，可 `zoe-cli user set-pin` 补设）

```sh
# 管理用户（桌面）
zoe-cli user list
zoe-cli user add --name alice --pin 123456
zoe-cli user activate <user_id>
```

## 构建与运行

依赖要求：Rust 稳定版（当前 CI 固定 1.93.0）；wasm target 用于构建 webui。

```sh
# webui（Rust/Leptos → wasm；仅当修改了 webui/src 才需要）
cargo install wasm-bindgen-cli --version $(awk '/^wasm-bindgen =/{getline; print $3}' webui/Cargo.lock | tr -d '"')
bash webui/scripts/build.sh        # Windows: powershell webui/scripts/build.ps1

cargo test --workspace              # 全量测试
cargo test -p zoe-transport --features ble-windows   # Windows 下含 BLE mock 测试

cargo run -p zoe-cli -- demo        # M0 双节点 loopback 演示

cargo run -p zoe-daemon -- --data-dir zoe-data
cargo run -p zoe-daemon -- --data-dir zoe-data --pin 123456   # 锁定模式需要 PIN 解锁
# 启动后浏览器打开输出的 http://127.0.0.1:<port>，输入访问令牌
# 双设备互通：一台在 UI 设置页复制"监听地址"发给对方 → 对方在群组详情页粘贴邀请
# 可选参数：--port N(固定端口)、--token STR(指定令牌)
```

## 移动端（Android）

完整设计与验收流程见 [docs/tauri-mobile.md](docs/tauri-mobile.md)。CI 产出 debug APK（GitHub Actions → `Android Tauri Build` → artifact）：

```sh
# 本地构建（需 Android SDK；gen/android 由 CI 生成，本地用 npx tauri android init）
cd app
npx tauri android build --apk --debug
```

## BLE 真机联调（Termux）

另一台 Android 手机（Termux）参与 BLE 真机联调的完整流程见 [docs/termux-ble.md](docs/termux-ble.md)。
快速开始：

```sh
# Linux 节点（peripheral，需蓝牙适配器；完整 GATT 服务端角色）
cargo build -p zoe-cli --features ble-linux
target/debug/zoe-cli ble adv --name zoe-device --echo

# Windows 节点（仅 central：扫描/连接；广播需 UWP 包标识，不可用）
cargo build -p zoe-cli --features ble-windows
target/debug/zoe-cli ble scan --timeout 10     # 配合手机 nRF Connect 模拟 peripheral

# 手机 Chrome 打开 tools/ble-gatt-test/index.html → 连接 zoe-device → 发帧/echo 测试

# 手机 Termux：安装环境 → 扫描验证广播
bash scripts/termux/setup-termux.sh
bash scripts/termux/ble-scan.sh --filter zoe --count 3
```

## 目录结构

```
crates/
├─ zoe-core/      身份 · 信封 · MLS 会话 · SQLite 存储 · 用户注册表/PIN 加密
├─ zoe-transport/ 传输抽象 + loopback + BLE GATT 覆盖网（平台驱动）
├─ zoe-cli/       调试 CLI（含 BLE 联调子命令与用户管理）
└─ zoe-daemon/    HTTP/WS 守护进程 + 内嵌 UI（锁定/解锁模式）
webui/            UI 源码（Rust/Leptos）→ dist（编译产物，内嵌进守护进程）
app/              Tauri 移动端（Android：回环桥 + Kotlin BLE + 内嵌守护进程）
android/          canonical Kotlin：ZoeFrame/ZoeBleServer/ZoeAdvertiser/Bridge
tools/ble-gatt-test/  手机端 Web Bluetooth GATT 测试页（单文件，无构建）
scripts/          CI 补丁与联调脚本（sync-android-ble / patch-android-gen / termux）
docs/             协议与接口规格（含 tauri-mobile.md / termux-ble.md 真机联调指南）
```

## 许可

[MIT](LICENSE)