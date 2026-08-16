# zoe-chat

轻量化端到端加密消息系统:Linux/Windows 本地守护进程 + Web UI,蓝牙近场网状直连,路径穿越实现去中心化远程通信。群组加密基于 MLS(RFC 9420,openmls)。

设计文档:`DESIGN.md` 及 `docs/`(envelope 格式、SQLite schema、会话协议、API 契约、模块布局)。

## 当前状态

**M0(骨架验证)✅**
- `zoe-core`:身份层(Ed25519 + BIP39 助记词)、统一信封编解码、openmls 封装
- `zoe-transport`:Transport trait + loopback 传输
- `zoe-cli`:init / fingerprint / demo(双节点消息、群组、update)

**M2(近场)✅ 驱动待真机验证**
- `BleDriver` trait + `MeshOverlay`:BLE GATT 网状覆盖(分片/重组、去重、TTL 存储转发),mock 驱动 5 项测试通过
- 驱动:bluer(Linux,CI 编译验证)、btleplug + windows-rs(Windows,本机编译验证)
- 角色:Linux 全角色(广播 + GATT 服务端 + 扫描/连接);Windows 可广播(仅广告,手机可扫描到 zoe-device)、可扫描/连接,但 **GATT 服务端受 UWP 限制不可用**(GattServiceProvider 需包标识)
- 真机联调工具:`zoe-cli ble adv|scan|connect`(Linux/Windows)、`scripts/termux/`(Android 扫描/构建)、`tools/ble-gatt-test/`(手机 Chrome Web Bluetooth GATT 测试页),流程见 [docs/termux-ble.md](docs/termux-ble.md)

**M3(远程)✅**
- libp2p 远程通道:手动拨号 + mDNS 发现 + DCUtR 打洞,noise 用身份密钥(与 QR 名片同钥)
- 守护进程消息核心:入站信封分发、KeyPackage 交换、邀请流程、消息路由
- **双守护进程 E2E 通过**:建群 → 邀请 → 双向加密消息,双方群组状态一致

## 构建与运行

```sh
# 依赖已锁定在 Cargo.lock;webui 需要先构建(仅当修改了 webui/src)
cd webui && npm install --save-dev typescript && npm run build && cd ..

cargo test --workspace          # 全量测试
cargo test -p zoe-transport --features ble-windows   # Windows 下含 BLE mock 测试

cargo run -p zoe-cli -- demo    # M0 双节点 loopback 演示

cargo run -p zoe-daemon -- --data-dir zoe-data
# 启动后浏览器打开输出的 http://127.0.0.1:<port>,输入访问令牌
# 双设备互通:一台在 UI 设置页复制"监听地址"发给对方 → 对方在群组详情页粘贴邀请
# 可选参数:--port N(固定端口)、--token STR(指定令牌)
```

## BLE 真机联调(Termux)

另一台 Android 手机(Termux)参与 BLE 真机联调的完整流程见 [docs/termux-ble.md](docs/termux-ble.md)。
快速开始:

```sh
# Linux 节点(peripheral,需蓝牙适配器;完整 GATT 服务端角色)
cargo build -p zoe-cli --features ble-linux
target/debug/zoe-cli ble adv --name zoe-device --echo

# Windows 节点(仅广播/扫描/连接;无 GATT 服务端 —— 手机可见但连不上)
cargo build -p zoe-cli --features ble-windows
target/debug/zoe-cli ble adv --name zoe-device

# 手机 Termux:安装环境 → 扫描验证广播
bash scripts/termux/setup-termux.sh
bash scripts/termux/ble-scan.sh --filter zoe --count 3

# 手机 Chrome 打开 tools/ble-gatt-test/index.html → 连接 zoe-device → 发帧/echo 测试
```

## 目录结构

```
crates/
├─ zoe-core/      身份 · 信封 · MLS 会话 · SQLite 存储
├─ zoe-transport/ 传输抽象 + loopback + BLE GATT 覆盖网(平台驱动)
├─ zoe-cli/       调试 CLI(含 BLE 联调子命令,Linux)
└─ zoe-daemon/    HTTP/WS 守护进程 + 内嵌 UI
webui/            UI 源码(TypeScript)→ dist(编译产物,内嵌进守护进程)
tools/ble-gatt-test/  手机端 Web Bluetooth GATT 测试页(单文件,无构建)
scripts/termux/   Termux 联调脚本:setup/ble-scan/build/run-daemon
docs/             协议与接口规格(含 docs/termux-ble.md 真机联调指南)
```
