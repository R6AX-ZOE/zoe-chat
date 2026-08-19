//! 守护进程状态:存储、openmls provider、会话表、事件总线、net 传输。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, oneshot};
use zoe_core::identity::IdentityKeyPair;
use zoe_core::mls::{MlsIdentity, MlsSession};
use zoe_core::storage::{ZoeProvider, ZoeStorage};

/// libp2p 远程传输句柄类型(feature `net` 启用时 = NetTransport;
/// 关闭时 = 占位类型,Transport 面一律返回不可用)。
#[cfg(feature = "net")]
pub use zoe_transport::net::NetTransport as NetHandle;
#[cfg(not(feature = "net"))]
#[derive(Clone, Debug)]
pub struct NetHandle;

pub struct AppState {
    pub storage: ZoeStorage,
    /// 数据目录(文件自动下载落盘目录 = <data_dir>/files)。
    pub data_dir: PathBuf,
    /// openmls 密码学状态(Connection 非 Sync,串行化访问)。
    pub provider: Mutex<ZoeProvider>,
    /// 群组会话(内存态,启动时从存储加载)。
    pub sessions: Mutex<HashMap<Vec<u8>, MlsSession>>,
    pub mls_identity: MlsIdentity,
    pub identity: IdentityKeyPair,
    pub token: String,
    /// WS 事件总线(JSON 文本)。
    pub events: broadcast::Sender<String>,
    /// libp2p 远程传输(M3;无 net feature 时为占位)。
    pub net: Option<Arc<NetHandle>>,
    /// 等待中的 KeyPackage 请求:peer_id → oneshot(邀请流程用)。
    pub pending_keypackages: Mutex<HashMap<String, oneshot::Sender<Vec<u8>>>>,
    /// 配对模式状态(protocol.md §1)。
    pub pairing: AtomicBool,
    pub pair_code: Mutex<Option<[u8; 8]>>,
    pub started_at: i64,
}

pub type SharedState = Arc<AppState>;

/// 恒时比较(防时序侧信道)。
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
