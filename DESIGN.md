# zoe-chat 设计文档 v0.1

轻量化端到端加密消息系统:**Linux 本地守护进程 + Web UI,蓝牙近场网状直连,路径穿越实现去中心化远程通信**。

**详细规格**:`docs/envelope.md`(统一信封与传输帧)· `docs/storage.md`(SQLite schema)· `docs/protocol.md`(会话与排序流程)· `docs/api.md`(HTTP/WS 契约)· `docs/modules.md`(模块布局与测试策略)· `docs/webui.md`(Rust Web UI 构建与架构)· `docs/dmls-evaluation.md`(M4:DMLS 评估)

## 1. 项目定位与目标

- 运行在 Linux 客户端,守护进程常驻,浏览器访问 `127.0.0.1` 上的 Web UI。
- 单二进制、无框架、资源占用低(目标:常驻内存 < 80MB)。
- 跨平台:**Linux 为主目标,Windows 全功能对等(含 BLE 近场)**——平台相关部分隔离在传输驱动层(见 §6.2),其余栈(openmls/libp2p/axum/SQLite)纯跨平台;Android/Termux 为远期可选平台。
- 近场:蓝牙 mesh 拓扑直连(无任何服务器)。
- 远程:路径穿越(hole punching)建立去中心化 P2P 通道,内容不经过任何中继;零基础设施——发现完全靠手动交换 Peer ID(QR/剪贴板)。
- 群组 E2EE 基于 **MLS(RFC 9420)**,使用 `openmls` 参考实现。

## 2. 威胁模型

**对抗者能力假设**:
- 可以监听/篡改所有传输信道(蓝牙空中接口、互联网路径)。
- 可以运营任何我们可能依赖的基础设施节点(自建中继等——v1 默认不依赖,见 §6.4)。
- 可以是群组内的恶意成员(MLS 的前向/后向保密正是为此设计)。
- 可能获得用户丢失/被没收的设备。

**不防御**:
- 终端侧恶意软件、键盘记录、屏幕截图、语音录音(E2EE 不防端侧,需在文档中诚实声明)。
- 物理接触下的内存/存储窃取(除非用户启用全盘加密)。
- 社会工程、身份冒充(仅靠协议无法解决,靠带外验证缓解)。

**元数据现实(诚实声明)**:
- 蓝牙广播近场可见:任何在射频范围内的人都能观察到"有节点在通信"——存在性与接近性无法隐藏。
- 互联网直连暴露 IP;若远期引入 DHT,则"谁在查询谁"对 DHT 节点可见。
- 群组规模、通信时间点、消息频率在本地传输链路上可见(加密不隐藏流量模式)。

## 3. 系统架构总览

```
┌────────────────────────────────────────────────┐
│ Web UI(嵌入式静态页面,127.0.0.1,随机 token 认证) │
│  会话列表 · QR 指纹验证 · 设备管理 · 密钥备份恢复 │
├────────────────────────────────────────────────┤
│ 本地守护进程(Rust 单二进制)                      │
│ ├─ 身份层:Ed25519 用户身份密钥 + 24 词助记词备份  │
│ ├─ 会话层:MLS(openmls,锁版本)                   │
 │ │     └─ 群组状态机 + 排序策略(见 §5.3)         │
│ ├─ 传输抽象层:统一信封,多传输投递                │
│ │     ├─ 传输 A:BLE GATT 网状覆盖网(v1)         │
│ │     ├─ 传输 B:SIG Bluetooth Mesh(Phase 2)    │
│ │     └─ 传输 C:libp2p 远程(打洞直连;中继自建可选)│
│ └─ 存储:SQLite WAL(密文为主,本地可选缓存)        │
└────────────────────────────────────────────────┘
```

**技术栈(假设,待确认)**:Rust 2021 edition;`openmls`、`bluer`(BlueZ D-Bus)、`rust-libp2p`、`axum`(内嵌 HTTP)、`rusqlite`、`argon2`。

## 4. 身份模型

- **用户身份**:每用户一个 Ed25519 长期身份密钥,用 24 词助记词(BIP39 词表)备份/恢复。
- **设备身份**:每个设备生成自己的 MLS 签名密钥与凭据,由用户身份密钥签名授权(用户 = 多设备的集合,设备级吊销可行)。
- **指纹验证(TOFU + 带外)**:身份密钥哈希派生 Safety-Number 风格指纹;Web UI 显示字符串 + QR 码,双方扫码完成首次信任与后续校验。服务器/传输层永远不持有身份信任锚。
- v1 简化:每个设备一个 MLS 身份(一个用户多设备 = 多个成员),设备间不共享会话状态。

## 5. 会话层:MLS(openmls)

### 5.1 选型理由与代价

- MLS 原生提供:前向保密(每 epoch 更新)、后向保密/自我修复(update 提案)、高效群组增删、非交互式 Welcome。
- **代价:不可否认性缺失(已接受,见 §11)**。MLS 所有提交与消息均带成员签名,密文 + 签名可作为"某人确说过"的证据——这与 Signal 的可否认设计相反。经决策接受这一属性并写入威胁模型。
- openmls 为 pre-1.0,API 变动频繁:锁一个具体版本,并定期随上游升级。

### 5.2 KeyPackage 分发(无服务器的关键)

MLS 加入群组需要对方的 KeyPackage。没有服务器时:

- **近场首次接触**:蓝牙直连交换身份指纹 + KeyPackage(附用户身份密钥签名,防伪造)。
- **远程(已定:纯手动发现)**:双方先经 QR/剪贴板交换 Peer ID 与身份指纹,建立直连通道后再交换 KeyPackage——不依赖任何公开目录。
- KeyPackage 有有效期与一次性使用约束,按 RFC 9420 语义执行。

### 5.3 排序策略:没有 DS 的 MLS(核心难点)

MLS 协议假定一个提供**全序**的投递服务(DS):所有提案/提交按同一顺序到达所有成员,epoch 才能一致推进。去中心化网络没有天然全序。方案分级:

- **v1(已定):组协调者 = 每群一个 DS**。由建群者(或轮换选举)承担排序与缓冲职责;协调者只接触密文,零知识;协调者离线时,群组消息仍可通过其他成员之间的直连投递,但**群组结构变更(加人/移除/update)受限**——诚实文档化这一可用性边界。
- **远期:DMLS(去中心化 MLS)**。IETF 已有活跃草案([draft-kohbrok-mls-dmls-00](https://datatracker.ietf.org/doc/html/draft-kohbrok-mls-dmls-00)、[draft-xue-distributed-mls](https://datatracker.ietf.org/doc/html/draft-xue-distributed-mls)),以 DAG + 共识排序替代中心 DS。等草案稳定后评估接入,不自行发明排序协议。**M4 已完成评估(维持现状,详见 [docs/dmls-evaluation.md](docs/dmls-evaluation.md));零成本改进:协调者可轮换**。

### 5.4 群组生命周期

- 建群 → 邀请(直接发 KeyPackage/Welcome,不走服务器)→ 加人/移除/成员 update → 解散。
- 群组 ID 为随机 32 字节;群组名仅本地存储。
- **注意:Welcome 消息体积**——v1 启用 `use_ratchet_tree_extension`(无服务器架构下加入者无法带外取树),Welcome 携带完整 ratchet tree,数十人群可达 KB 级,经 BLE 需分片;这是 SIG Mesh(11 字节载荷)阶段的主要障碍,也是 GATT 覆盖网优先的原因。

## 6. 传输抽象层

### 6.1 统一信封

所有传输共享信封:

```
[群组 ID | epoch | sender 设备ID | 消息类型 | 消息序号] + MLS 密文
```

传输层职责仅限于投递 + 去重(按信封哈希)+ 尽力重传;**永不接触明文与密钥**。多传输并存时按"可达性优先"路由(近场 BLE > 局域网 mDNS > 互联网打洞),同一消息可多路径投递以提高到达率。

### 6.2 传输 A:BLE GATT 网状覆盖网(v1)

- 通过 BlueZ D-Bus(`bluer`)操作:节点同时作为 peripheral/central 与邻居建连,MTU 协商到 512B+,实现可靠的近场双向通道。
- 网状拓扑:每个节点维护邻居表,对非本节点的消息**存储转发**,用信封哈希去重,限制跳数(TTL)防泛洪风暴。
- 物理层只做"哑管道",不引入任何 BLE 层安全机制作为信任根(BlueZ 的 SMP 配对仅用于防近场窃听,不作为 E2EE 依据)。

**平台驱动抽象(Windows 兼容的关键)**:`BleDriver` trait 定义最小面——广告(advertise)、扫描(scan)、连接(connect)、GATT 读写与通知、MTU 协商。实现分角色:
- **central(连接方)**:`btleplug`(一个 crate 覆盖 Windows/WinRT、macOS、Linux/BlueZ),两平台共用;
- **peripheral(广告方)**:LINUX 用 `bluer`(BlueZ D-Bus);WINDOWS 用 `windows-rs` 的 `BluetoothLEAdvertisementPublisher` + `GattServiceProvider`。

上层逻辑(存储转发、去重、TTL、信封分片)只依赖 trait。**全功能对等为既定目标**:Windows peripheral 角色受系统约束(广告与 GATT server 并发、服务数量、需 Win10 1709+),故 **M2 先在 Linux 验证网状拓扑,Windows BLE 驱动紧随其后**并在实现中处理这些限制;Android(远期)经 Android BLE API 实现同一 trait。

### 6.3 传输 B:SIG Bluetooth Mesh(Phase 2)

- 标准 SIG Mesh 应用载荷仅约 11 字节,分片上限 ~380 字节、无确认——作为聊天传输低效,仅在与标准 mesh 设备/网络互通时有价值。
- 接入方式:作为**哑管道**承载统一信封(载荷 = 信封分片),网络层 Key 只负责泛洪网络本身的完整性。分片、重组、去重、重传在应用层实现。
- 是 GATT 覆盖网不可达而 SIG Mesh 可达场景(如远距离一跳)的补充,不作为主传输。

### 6.4 传输 C:libp2p 远程(路径穿越,已定:零基础设施)

- 发现模型(已定:**纯手动**):Web UI 生成二维码/文本形式的 Peer ID + 身份指纹,双方扫码或粘贴完成互认;不部署任何公共 bootstrap/DHT。
- 连接:局域网内 `mDNS` 自动直连;跨网用 `DCUtR` 做 TCP/QUIC 打洞。
- 中继兜底(已定:默认无公共中继):打洞失败时该路径不可用——诚实声明这一边界;支持用户**自建** `circuit-relay-v2` 中继供自己使用(中继只见密文)。
- libp2p 的 `noise` 握手可选开启(防御传输层窃听;内容安全已由 MLS 保证)。
- 可选远期:Tor 隐藏服务作为匿名传输(隐藏 IP),与打洞通道并存;若未来社区化,再评估公共 bootstrap + DHT 自动发现(仅路由,零知识)。

## 7. 守护进程与 Web UI

- 守护进程:常驻、开机自启(可选),监听 `127.0.0.1:随机端口`,首次启动生成 token,浏览器访问需携带 token(防本机其他进程/网页跨站调用)。
- UI:**Rust 编写(Leptos CSR → wasm32-unknown-unknown),零 JS/TS 工具链**(npm/tsc/vite 全部移除;wasm-bindgen 胶水 ~5KB 属运行机制)。产物 `webui/dist`(index.html + styles.css + zoe_webui.js + zoe_webui_bg.wasm)由 zoe-daemon **编译期内嵌并服务**;构建 = `cargo build --target wasm32-unknown-unknown` + wasm-bindgen(`webui/scripts/build.sh|.ps1`),CI 校验提交的 dist 与源码一致。
- 功能:会话列表、消息流(分页)、QR 指纹验证、配对模式、设备管理(吊销)、密钥备份/恢复(助记词)、对端阻止/带外验证、传输状态指示(BLE/局域网/互联网/SIG Mesh)。界面文案走 i18n 键目录(zh-CN/en-US 各 113 键,CI 校验键集合一致,见 webui/src/i18n.rs)。样式:深色/浅色主题(默认跟随系统,可手动切换);图标统一自绘圆滑路径 SVG,**禁止 emoji 作图标**;移动优先响应式(移动端单栏 / 桌面三栏;<1024px 设置视图占满主区域,顶栏 `+`/齿轮入口所有宽度可见)。构建/开发/平台注意详见 [docs/webui.md](docs/webui.md)。
- **移动端(Tauri)**:内嵌守护进程(`zoe-daemon` lib,`default-features=false` 排除 libp2p),固定端口 `127.0.0.1:18571`,WebView 加载该地址(同源免 CORS);令牌经 `zoe_boot_token` tauri command 引导;同一份 UI 产物与桌面共用(见 docs/tauri-mobile.md §0)。
- 所有密钥操作在守护进程内完成,浏览器只拿渲染数据;UI 通过本地 HTTP + WebSocket(仅本机)与守护进程通信。

## 8. 存储与备份

- SQLite(WAL):消息**密文**持久化;为流畅渲染可缓存已解密明文(标记为本地明文,可选清除)。
- 设备密钥:文件权限 0600;可选用户口令派生密钥(argon2)加密存储。
- 备份/恢复:24 词助记词恢复用户身份密钥;设备间信任关系通过重新扫码授权重建;群组会话历史随设备重建(从群内其他成员处同步密文)。

## 9. 性质清单对照

| 性质 | 状态 | 说明 |
|---|---|---|
| 端到端保密性 | ✅ 原生 | MLS 内容加密,协调者/中继/传输层零知识 |
| 前向保密 | ✅ 原生 | MLS epoch 推进即密钥轮换 |
| 后向保密(PCS) | ✅ 原生 | update 提案恢复安全性 |
| 真实性/防中间人 | ✅ 原生 | MLS 签名 + 凭据链;指纹 QR 带外验证 |
| 抗重放/乱序 | ✅ 原生 | MLS 序号 + 信封去重 |
| 可否认性 | ❌ 不满足 | MLS 消息带成员签名(见 Q3) |
| 异步通信 | ⚠️ 部分 | 离线消息由邻居/协调者缓冲;无服务器时可达性不保证 |
| 群组加密 | ✅ 原生 | MLS 原生群组,规模变化高效 |
| 多设备 | ⚠️ v1 简化 | 每设备独立 MLS 身份,统一由用户身份授权 |
| 元数据最小化 | ⚠️ 有限 | 近场存在性/IP/DHT 查询可见(§2 诚实声明) |
| 零知识服务器 | ✅ | 无服务器;v1 零基础设施,中继仅自建可选 |
| 开源可审计 | ✅ | openmls = RFC 9420 参考实现;自研部分全部开源 |
| 默认安全 | ✅ | 无明文降级路径;指纹不匹配即拒绝 |
| 失败安全 | ✅ | 密钥/指纹/序号异常一律报错,不静默降级 |

## 10. 里程碑

- **M0(骨架验证)✅**:仓库骨架、身份层、openmls 集成、loopback 传输(内存通道)、CLI 验证双人消息 + 群组 + update。
- **M1(产品外壳)✅**:守护进程 + Web UI(docs/api.md 全量端点已实现):会话、消息分页、指纹 QR 验证/带外验证流程、配对模式、对端阻止、设备管理(吊销)、助记词备份**与恢复**、传输状态指示(含 SIG Mesh);CI 双平台构建(Linux/Windows)。**2026-08-19:UI 以 Rust 重写(Leptos → wasm,去 npm/tsc/vite),桌面与 Tauri 移动端共用同一份产物**;真机/浏览器实测修复:登录解析、布局挂载、窄窗口入口、建群对话框、源码编码(见 §7、docs/webui.md §5 与 docs/tauri-mobile.md 坑 18-20)。
- **M2(近场)✅(驱动待真机验证)**:`BleDriver` trait + `MeshOverlay` 存储转发覆盖网(分片/重组、去重、TTL)全部以 mock 驱动测试通过;bluer(Linux)与 btleplug+windows-rs(Windows)驱动已编写并通过编译验证。已知限制:Windows 广告角色受 SDK 绑定限制暂不可用(Windows 以 central 身份连出);GATT server 角色(被连接方)Windows 待 M2.5。真机联调需蓝牙硬件。
- **M3(远程)✅**:libp2p 远程通道:手动 Peer ID 交换 + mDNS 局域网发现 + DCUtR 打洞(noise 用身份密钥,与 QR 名片同钥);守护进程消息核心(入站分发/KeyPackage 交换/邀请流程/消息路由);**双守护进程 E2E 通过**:建群 → 邀请 → 双向加密消息,双方群组状态一致(epoch/members)。
- **M2(近场)**:BLE GATT 网状覆盖网,**Linux 先行**,`BleDriver` trait 定稿;多节点存储转发;Windows BLE 驱动紧随补齐(全功能对等目标)。
- **M3(远程)**:libp2p 远程通道:手动 Peer ID 交换 + DCUtR 打洞(+ 可选自建 relay)。
- **M4(互通与演进)🔄**:SIG Mesh 适配 **✅**(`zoe-transport` feature `sigmesh`:分片/重组/去重/重传 + 网络层 TTL/泛洪抽象,FloodHub mock 全测通过,见 docs/envelope.md §2.2);**DMLS 评估 ✅(结论:维持协调者排序,不替换)**——两份草案均过期、合并语义未决、FS 有损,收益与成本不匹配;再评估触发条件与零成本改进(协调者可轮换)见 docs/dmls-evaluation.md。

## 11. 决策记录与剩余开放问题

**已定决策**:
- **Q1 远程发现(已定)**:纯手动 Peer ID 交换,零公共基础设施;DHT 自动发现留作远期社区化选项。
- **Q2 v1 排序(已定)**:组协调者即每群 DS,只接触密文;DMLS 草案留作 M4 评估。
- **Q3 不可否认性(已定)**:接受——MLS 消息签名不可否认是刻意接受的属性,已写入威胁模型。
- **Q4 技术栈(已定)**:Rust 单二进制;openmls / rust-libp2p / bluer / axum / rusqlite。
- **Q7 Windows 范围(已定)**:全功能对等,含 BLE 近场;Windows 驱动 M2 紧随 Linux 验证后补齐;Android/Termux 远期可选。
- **Q8 开发环境(已定)**:Windows 原生开发,CI(GitHub Actions)出 Linux 产物。
- **Q9 UI 多语言(已定)**:目录驱动 i18n,首发 zh-CN + en-US 可扩展;界面语言纯客户端渲染,不进入协议/密文(见 docs/api.md §3);消息内容任意 UTF-8,无语言限制。

**剩余开放问题**:
- **Q5 许可证**:开源许可证选择(AGPL-3.0 / Apache-2.0 / MIT 等)。
- **Q6 打洞失败体验**:自建中继的配置入口在 UI 中的层级。
