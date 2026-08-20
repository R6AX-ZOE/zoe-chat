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
    /// 等待中的 KeyPackage 请求:peer_id → oneshot(邀请流程用)。
    pub pending_keypackages: Mutex<HashMap<String, oneshot::Sender<Vec<u8>>>>,
    /// 配对模式状态(protocol.md §1)。
    pub pairing: AtomicBool,
    pub pair_code: Mutex<Option<[u8; 8]>>,
    pub started_at: i64,
}

pub type SharedState = Arc<AppState>;

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

/// 激活用户是否 PIN 保护。
pub fn active_kind(state: &SharedState) -> UserKind {
    active_user(state).kind
}

/// libp2p 传输句柄(未装配/解锁时为 None)。
pub fn net_handle(state: &SharedState) -> Option<Arc<NetHandle>> {
    state.net.lock().unwrap().clone()
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

    async fn send(
        &self,
        _to: &str,
        _envelope: zoe_core::envelope::Envelope,
    ) -> Result<(), zoe_transport::TransportError> {
        Err(zoe_transport::TransportError::Io(
            "net transport not built (feature `net` disabled)".to_string(),
        ))
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<zoe_transport::Inbound> {
        let (tx, _) = tokio::sync::broadcast::channel(1);
        tx.subscribe()
    }
}
