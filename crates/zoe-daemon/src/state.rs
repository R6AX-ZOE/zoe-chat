//! 守护进程状态:存储、openmls provider、会话表、事件总线、net 传输。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, oneshot};
use zoe_core::identity::IdentityKeyPair;
use zoe_core::mls::{MlsIdentity, MlsSession};
use zoe_core::storage::{ZoeProvider, ZoeStorage};
use zoe_transport::net::NetTransport;

pub struct AppState {
    pub storage: ZoeStorage,
    /// openmls 密码学状态(Connection 非 Sync,串行化访问)。
    pub provider: Mutex<ZoeProvider>,
    /// 群组会话(内存态,启动时从存储加载)。
    pub sessions: Mutex<HashMap<Vec<u8>, MlsSession>>,
    pub mls_identity: MlsIdentity,
    pub identity: IdentityKeyPair,
    pub token: String,
    /// WS 事件总线(JSON 文本)。
    pub events: broadcast::Sender<String>,
    /// libp2p 远程传输(M3)。
    pub net: Option<Arc<NetTransport>>,
    /// 等待中的 KeyPackage 请求:peer_id → oneshot(邀请流程用)。
    pub pending_keypackages: Mutex<HashMap<String, oneshot::Sender<Vec<u8>>>>,
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
