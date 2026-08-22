//! 守护进程状态:存储、openmls provider、会话表、事件总线、net 传输、多用户锁。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, oneshot};
use zoe_core::identity::IdentityKeyPair;
use zoe_core::mls::{MlsIdentity, MlsSession};
use zoe_core::storage::{ZoeProvider, ZoeStorage};
use zoe_core::users::{User, UserKind, UserRegistry};

/// libp2p 远程传输句柄类型(feature `net` 启用时 = NetTransport;
/// 关闭时 = 占位类型,Transport 面一律返回不可用)。
#[cfg(feature = "net")]
pub use zoe_transport::net::NetTransport as NetHandle;
#[cfg(not(feature = "net"))]
#[derive(Clone, Debug)]
pub struct NetHandle;

/// 局域网传输句柄(feature `lan` 时 = LanTransport;未启用 = 空类型)。
#[cfg(feature = "lan")]
pub use zoe_transport::lan::LanTransport as LanHandle;
#[cfg(not(feature = "lan"))]
#[derive(Clone, Debug)]
pub struct LanHandle;

/// SIG Mesh 洪泛介质(宿主注入:移动端 = BLE GATT 桥;缺省 = FloodHub mock)。
#[cfg(feature = "sigmesh")]
pub use zoe_transport::sigmesh::SigMeshNet;

pub struct AppState {
    pub storage: ZoeStorage,
    /// 数据目录(文件自动下载落盘目录 = <data_dir>/files;用户注册表 users.db 所在根)。
    pub data_dir: PathBuf,
    /// openmls 密码学状态(Connection 非 Sync,串行化访问)。
    pub provider: Mutex<ZoeProvider>,
    /// 群组会话(内存态,启动时从存储加载)。
    pub sessions: Mutex<HashMap<Vec<u8>, MlsSession>>,
    /// 设备 MLS 凭据;仅解锁后存在(Some)。423 门禁保证未解锁不触达。
    pub mls_identity: Mutex<Option<Arc<MlsIdentity>>>,
    /// 当前用户身份;仅解锁后存在(Some)。
    pub identity: Mutex<Option<Arc<IdentityKeyPair>>>,
    /// 解锁标志(false = 锁定模式,仅 /users* 与 /unlock 可用)。
    pub unlocked: AtomicBool,
    /// 多用户注册表(data_dir/users.db)。
    pub registry: UserRegistry,
    /// 当前激活用户(启动时解析;切换 = /users/:id/activate 自重启,v1 约定)。
    pub active_user: Mutex<User>,
    /// 移动端内嵌模式(禁用户切换/自重启)。
    pub mobile: bool,
    /// WS 事件总线(JSON 文本)。
    pub events: broadcast::Sender<String>,
    /// libp2p 远程传输(M3;无 net feature 时为占位)。
    /// Mutex 包装以便解锁后按需装配。
    pub net: Mutex<Option<Arc<NetHandle>>>,
    /// 局域网传输句柄(feature `lan`;未启用为 None)。
    pub lan: Mutex<Option<Arc<LanHandle>>>,
    /// SIG Mesh 洪泛堆栈(transport 句柄;feature `sigmesh`;未启用为 None)。
    pub sigmesh: Mutex<Option<Arc<dyn zoe_transport::Transport + Send + Sync>>>,
    /// SIG Mesh 洪泛介质(宿主注入:移动端 = BLE GATT 桥;None = FloodHub mock)。
    #[cfg(feature = "sigmesh")]
    pub sigmesh_net: Mutex<Option<Arc<dyn SigMeshNet>>>,
    /// 等待中的 KeyPackage 请求:peer_id → oneshot(邀请流程用)。
    pub pending_keypackages: Mutex<HashMap<String, oneshot::Sender<Vec<u8>>>>,
    /// 连接/握手时收到的身份公告(zoe hex, 网络 id),但联系人尚未导入名片:
    /// import_card 时回填 net_peer_id(容量上限 128,FIFO)。
    pub pending_identity: Mutex<Vec<(String, String)>>,
    /// 配对模式状态(protocol.md §1)。
    pub pairing: AtomicBool,
    pub pair_code: Mutex<Option<[u8; 8]>>,
    /// 移动端 BLE 桥状态(内嵌守护进程由宿主 Tauri 应用轮询回写;
    /// 桌面无桥,恒 false → transports 如实上报 down)。
    pub ble_up: AtomicBool,
    /// 系统级命令钩子(仅内嵌宿主注册;桌面 None)。
    pub system_hook: Option<SystemHook>,
    pub started_at: i64,
}

pub type SharedState = Arc<AppState>;

/// 系统级命令钩子(宿主应用注册;`"restart"` → 重建进程/冷启动)。
pub type SystemHook = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

// ---------------------------------------------------------------------------
// 解锁访问助手
// ---------------------------------------------------------------------------

/// 常时比较(防时序侧信道)。
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 是否已解锁(身份可用)。
pub fn is_unlocked(state: &SharedState) -> bool {
    state.unlocked.load(Ordering::SeqCst)
}

/// 当前身份;未解锁返回 None。调用方须已 423 门禁(防御性再查一次)。
pub fn identity(state: &SharedState) -> Option<Arc<IdentityKeyPair>> {
    if !is_unlocked(state) {
        return None;
    }
    state.identity.lock().unwrap().clone()
}

/// 设备 MLS 凭据;未解锁返回 None。
pub fn mls_identity(state: &SharedState) -> Option<Arc<MlsIdentity>> {
    if !is_unlocked(state) {
        return None;
    }
    state.mls_identity.lock().unwrap().clone()
}

/// 当前激活用户信息。
pub fn active_user(state: &SharedState) -> User {
    state.active_user.lock().unwrap().clone()
}

/// 回写移动端 BLE 桥状态(宿主应用轮询调用);变化时向 WS 发 `transport` 事件,
/// 使 Web UI 传输状态点即时刷新(与桌面 libp2p 变化通知语义一致)。
pub fn set_ble_up(state: &SharedState, up: bool) {
    let prev = state.ble_up.swap(up, Ordering::SeqCst);
    if prev == up {
        return;
    }
    let _ = state.events.send(
        serde_json::json!({
            "type": "transport",
            "event": "changed",
            "ble": up,
        })
        .to_string(),
    );
}

/// 激活用户是否 PIN 保护。
pub fn active_kind(state: &SharedState) -> UserKind {
    active_user(state).kind
}

/// libp2p 传输句柄(未装配/解锁时为 None)。
pub fn net_handle(state: &SharedState) -> Option<Arc<NetHandle>> {
    state.net.lock().unwrap().clone()
}

/// 局域网传输句柄(未启用/未解锁时 None)。
pub fn lan_handle(state: &SharedState) -> Option<Arc<LanHandle>> {
    state.lan.lock().unwrap().clone()
}

/// SIG Mesh 堆栈句柄(未启用/未解锁时 None)。
pub fn sigmesh_handle(
    state: &SharedState,
) -> Option<Arc<dyn zoe_transport::Transport + Send + Sync>> {
    state.sigmesh.lock().unwrap().clone()
}

// 无 net feature 时,占位 NetHandle 的 Transport 面一律不可用(失败安全,
// 上层代码无需 cfg 分支)。
#[cfg(not(feature = "net"))]
impl zoe_transport::Transport for NetHandle {
    fn name(&self) -> &'static str {
        "net-unavailable"
    }

    fn availability(&self) -> zoe_transport::Availability {
        zoe_transport::Availability::Down
    }

    fn peers(&self) -> Vec<String> {
        Vec::new()
    }

    fn send(
        &self,
        _to: &str,
        _envelope: zoe_core::envelope::Envelope,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), zoe_transport::TransportError>> + Send>,
    > {
        Box::pin(async {
            Err(zoe_transport::TransportError::Io(
                "net transport not built (feature `net` disabled)".to_string(),
            ))
        })
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<zoe_transport::Inbound> {
        let (tx, _) = tokio::sync::broadcast::channel(1);
        tx.subscribe()
    }
}

#[cfg(not(feature = "lan"))]
impl zoe_transport::Transport for LanHandle {
    fn name(&self) -> &'static str {
        "lan-unavailable"
    }

    fn availability(&self) -> zoe_transport::Availability {
        zoe_transport::Availability::Down
    }

    fn peers(&self) -> Vec<String> {
        Vec::new()
    }

    fn send(
        &self,
        _to: &str,
        _envelope: zoe_core::envelope::Envelope,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), zoe_transport::TransportError>> + Send>,
    > {
        Box::pin(async {
            Err(zoe_transport::TransportError::Io(
                "lan transport not built (feature `lan` disabled)".to_string(),
            ))
        })
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<zoe_transport::Inbound> {
        let (tx, _) = tokio::sync::broadcast::channel(1);
        tx.subscribe()
    }
}
