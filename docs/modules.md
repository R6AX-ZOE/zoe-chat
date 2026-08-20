# 模块布局、trait 与测试策略 v0.1

## 1. Cargo workspace

```
zoe-chat/
├─ Cargo.toml                  # workspace(edition 2021, profile: lto + opt-level=3)
├─ crates/
│  ├─ zoe-core/                # 纯逻辑,无 I/O 平台依赖
│  │  ├─ src/identity.rs       # Ed25519 身份、设备凭据、指纹、助记词(BIP39)
│  │  ├─ src/users.rs          # 多用户注册表(UserRegistry, users.db):PIN 校验(argon2id)、种子加密解密、user CRUD/activate
│  │  ├─ src/mls.rs            # openmls 封装:建群/加入/Proposal/Commit/消息加解密
│  │  ├─ src/envelope.rs       # 信封编解码 + 分片帧(见 docs/envelope.md)
│  │  ├─ src/storage.rs        # rusqlite schema 与访问层(见 docs/storage.md)
│  │  └─ src/ordering.rs       # 协调者排序状态机(见 docs/protocol.md §3)
│  ├─ zoe-transport/           # 传输抽象 + 实现
│  │  ├─ src/lib.rs            # Transport trait、多路径路由、去重缓存
│  │  ├─ src/loopback.rs       # M0 开发用内存通道
│  │  ├─ src/ble/mod.rs        # BleDriver trait(见 §3)
│  │  ├─ src/ble/linux.rs      # bluer(BlueZ D-Bus)    [feature=ble-linux]
│  │  ├─ src/ble/windows.rs    # btleplug + windows-rs  [feature=ble-windows]
│  │  ├─ src/net.rs            # rust-libp2p:mDNS + DCUtR + 可选自建 relay
│  │  └─ src/sigmesh.rs        # SIG Mesh 适配(Phase 2) [feature=sigmesh]
│  ├─ zoe-daemon/              # axum HTTP/WS + 事件总线 + 进程编排(拆 lib + bin)
│  │  ├─ src/lib.rs            # start(DaemonConfig):库化守护进程(桌面 bin 与 Tauri 移动端共用)
│  │  ├─ src/api.rs            # 端点实现(见 docs/api.md;内嵌 webui/dist wasm 产物;含锁定门禁/users/unlock/set-pin)
│  │  ├─ src/state.rs          # 运行时状态:激活用户/身份/MLS/net、unlock 门禁查询
│  │  └─ src/events.rs         # WS 推送
│  └─ zoe-cli/                 # 调试 CLI:身份初始化、配对、发消息、状态
├─ webui/                      # Rust Web UI(独立 crate,Leptos CSR → wasm;无 npm/tsc/vite,详见 docs/webui.md)
│  ├─ Cargo.toml               # zoe-webui:leptos 0.7(csr)/gloo-net/gloo-timers;自带 Cargo.lock(提交入库)
│  ├─ src/lib.rs               # wasm 入口(#[wasm_bindgen(start)] + mount_to(#app))
│  ├─ src/app.rs               # 组件:登录/会话列表/消息线程/群组详情/设置(配对·设备·对端·备份恢复·网络·传输·用户管理)/锁定屏(PIN 解锁)
│  ├─ src/api.rs               # HTTP/WS 客户端(DTO;响应一律显式 Value;无令牌;users/unlock/create_user/set_pin/activate_user)
│  ├─ src/i18n.rs              # 键驱动词典(zh-CN/en-US 各 145 键,单测校验键集合一致)
│  ├─ src/theme.rs             # 深色/浅色:data-theme + prefers-color-scheme + 手动切换
│  ├─ src/icons.rs             # 自绘 SVG 图标(24×24,stroke 1.5,圆滑路径;无 emoji)
│  ├─ static/                  # 外壳:index.html(wasm 加载器)+ styles.css(CSS 变量主题,WCAG AA)
│  ├─ scripts/build.sh|.ps1    # cargo build --target wasm32 + wasm-bindgen → dist/(bindgen 版本自锁 Cargo.lock)
│  └─ dist/                    # 构建产物(提交入库;zoe-daemon 编译期内嵌,CI 校验与源码一致)
├─ docs/                       # 本目录规格文档
└─ .github/workflows/ci.yml    # windows-latest + ubuntu-latest:cargo test + clippy + fmt + webui wasm 校验
```

依赖选型(锁版本):`openmls`、`ed25519-dalek`、`sha2`、`bip39`、`rusqlite`(bundled)、`tokio`、`axum`、`rust-libp2p`、`bluer`、`btleplug`、`windows`、`argon2`、`serde`/`serde_json`、`tracing`。

## 2. 关键 trait

```rust
// zoe-transport:所有传输的公共面
pub trait Transport: Send + Sync {
    fn name(&self) -> &'static str;                       // "ble" | "lan" | "net"
    fn availability(&self) -> Availability;               // Up | Down | Degraded
    async fn send(&self, to: &PeerAddr, env: Envelope) -> Result<(), TransportError>;
    fn subscribe(&self) -> mpsc::Receiver<Inbound>;       // 入站信封流
    fn peers(&self) -> Vec<PeerAddr>;                     // 当前可达邻居
}

// zoe-transport/ble:BleDriver 平台面(唯一平台相关)
pub trait BleDriver: Send + Sync {
    async fn start_advertising(&self, name: &str, pair_code: [u8; 8]) -> Result<()>;
    async fn stop_advertising(&self) -> Result<()>;
    async fn scan(&self, timeout: Duration) -> Result<Vec<BlePeer>>;  // 名称+地址
    async fn connect(&self, addr: &BleAddr) -> Result<BleConn>;       // MTU 协商≥512
    async fn listen(&self) -> Result<mpsc::Receiver<BleConn>>;        // 被连接
}
pub struct BleConn { /* gatt read/write/notify, mtu */ }

// zoe-core/mls:openmls 封装面
pub struct MlsSession { /* 每群组一个 MlsGroup */ }
impl MlsSession {
    fn create_group(...) -> Self;
    fn join(welcome: &Welcome, kp: &KeyPackage) -> Result<Self>;
    fn propose(&mut self, op: GroupOp) -> Result<ProposalOut>;
    fn commit(&mut self, proposals: Vec<ProposalIn>) -> Result<CommitOut>; // 协调者
    fn handle_commit(&mut self, commit: CommitIn) -> Result<()>;
    fn encrypt(&mut self, msg: &[u8]) -> Result<PrivateMessage>;
    fn decrypt(&mut self, msg: &PrivateMessage) -> Result<Vec<u8>>;
    fn export_state(&self) -> Vec<u8>;  // 落库 blob(M0)
}
```

## 3. openmls 集成要点

- **cipher suite**(轻量默认):`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`。
- **ratchet tree**:建群启用 `use_ratchet_tree_extension(true)`,Welcome 携带完整树(无服务器场景下加入者无法带外取树的必需);体积随成员数增长。
- **commit 语义(openmls 0.8)**:提交方生成 commit 后须 `merge_pending_commit`,接收方处理 commit 后须 `merge_staged_commit`,epoch 才会推进——`zoe-core::mls` 封装已自动处理;`add_members` 返回 `(commit, welcome, group_info)`,协调者将 commit 广播给既有成员、welcome 发给新成员。
- 凭据:每设备 `BasicCredential` + 设备签名钥(`SignatureKeyPair`,ED25519),凭据与签名钥经 `CredentialWithKey` 绑定;设备公钥由用户身份私钥签名(委托链,加入时一次性验证)。
- 版本:**锁定 openmls 0.8.1**;`meta.openmls_version` 记录,升级需迁移测试(参考实现 API 变动频繁)。
- **持久化(M1)**:M0 为内存态(不落库);M1 实现 `StorageProvider`(rusqlite 后端),按 openmls 的条目粒度持久化群组状态与 KeyPackage。
- **限制**:单进程单用户;每群组一个 `MlsGroup` 实例常驻内存(群组数×~KB 级,轻量目标内)。

## 4. 测试策略

- **单元**:信封编解码(往返、截断、超长、未知字段);指纹/助记词向量;storage schema 迁移。
- **i18n**:CI 校验各语言键集合一致(webui/src/i18n.rs 单测,zh-CN/en-US 各 145 键);占位符与 `t()` 调用参数匹配。
- **webui(Rust/Leptos)**:原生目标 `cargo test --manifest-path webui/Cargo.toml`(i18n 键集合等纯逻辑);wasm 产物构建 + `git diff --exit-code webui/dist` 校验提交的 dist 与源码一致(CI 双 workflow 执行)。**源码必须保持 UTF-8**(PS 5.1 默认编码读写会损坏非 ASCII,见 docs/tauri-mobile.md 坑 18)。
- **集成(核心)**:双节点 loopback 全流程测试——配对 → 建群 → 互发 → update → 加人 → 移除 → 离线重放;乱序/丢包注入(fuzz 调度器)。
- **协调者故障测试**:协调者离线期间 Proposal 排队,恢复后重放,epoch 一致性。
- **多传输测试**:同一 Envelope 经 loopback 与 fake-BLE 双路径投递,去重正确。
- **CI**:windows-latest + ubuntu-latest 全量 `cargo test`;Linux 产物构建验证;openmls 升级时跑全量回归。
- **形式化(远期,M4)**:协议流程用 Tamarin 建模验证排序协议不变式(先于 DMLS 评估)。
