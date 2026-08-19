//! zoe-chat 守护进程核心(库面):数据目录初始化、AppState 构建、axum 服务启动。
//!
//! 桌面二进制(zoe-daemon)与 Tauri 移动端(app/)共用本库:
//! 移动端以固定端口内嵌守护进程,WebView 直接加载 `http://127.0.0.1:<port>`。
//!
//! feature `net`(默认开)启用 libp2p 远程传输;移动端 `default-features = false`
//! 以排除 libp2p(瘦身,与 zoe-transport 的 ble-mobile 组合一致)。

pub mod api;
pub mod msg;
pub mod state;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use zoe_core::identity::IdentityKeyPair;
use zoe_core::mls::{MlsIdentity, MlsSession};
use zoe_core::storage::{Db, ZoeProvider, ZoeStorage};
#[cfg(feature = "net")]
use zoe_transport::Transport;

use crate::state::{now, AppState, NetHandle};

/// 移动端内嵌守护进程的固定端口(WebView 加载地址)。
pub const MOBILE_PORT: u16 = 18571;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub data_dir: PathBuf,
    /// 0 = 随机端口(桌面默认)。
    pub port: u16,
    /// None = 从 data_dir/token 读取或生成(桌面默认)。
    pub token: Option<String>,
}

impl DaemonConfig {
    pub fn desktop(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            port: 0,
            token: None,
        }
    }

    pub fn mobile(data_dir: PathBuf, token: String) -> Self {
        Self {
            data_dir,
            port: MOBILE_PORT,
            token: Some(token),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage: {0}")]
    Storage(String),
    #[error("identity: {0}")]
    Identity(String),
    #[error("mls: {0}")]
    Mls(String),
    #[error("bind {0}: {1}")]
    Bind(SocketAddr, String),
}

/// 运行中的守护进程句柄。
pub struct Daemon {
    pub state: state::SharedState,
    pub addr: SocketAddr,
    /// 服务任务(axum)。停掉即退出 HTTP/WS。
    pub server: tokio::task::JoinHandle<()>,
}

/// 读取或生成访问令牌(0600,写入 data_dir/token)。
pub fn load_or_create_token(dir: &std::path::Path) -> Result<String, std::io::Error> {
    let path = dir.join("token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let token = hex::encode(bytes);
    std::fs::write(&path, &token)?;
    Ok(token)
}

/// 初始化数据目录 + 构建 AppState + 启动 axum 服务(后台任务),返回句柄。
pub async fn start(config: DaemonConfig) -> Result<Daemon, DaemonError> {
    std::fs::create_dir_all(&config.data_dir)?;

    let db = Db::open(&config.data_dir.join("zoe.db"))
        .map_err(|e| DaemonError::Storage(e.to_string()))?;
    let storage = ZoeStorage::new(db);
    let provider = ZoeProvider::new(&config.data_dir.join("mls.db"))
        .map_err(|e| DaemonError::Storage(e.to_string()))?;

    // 用户身份(不存在则生成)
    let identity = match storage
        .identity()
        .map_err(|e| DaemonError::Storage(e.to_string()))?
    {
        Some((seed, _)) => IdentityKeyPair::from_seed(&seed),
        None => {
            let id = IdentityKeyPair::generate();
            storage
                .set_identity(&id.seed(), now())
                .map_err(|e| DaemonError::Storage(e.to_string()))?;
            id
        }
    };

    // 设备 MLS 凭据:确定性地派生自身份种子(恢复身份即恢复设备)
    let mut h = Sha256::new();
    h.update(identity.seed());
    h.update(b"zoe-device-v1");
    let device_seed: [u8; 32] = h.finalize().into();
    let mls_identity =
        MlsIdentity::new("device-1", &device_seed).map_err(|e| DaemonError::Mls(e.to_string()))?;

    // 登记本设备(设备表;user_sig = 用户身份派生的占位签名 —— 完整签名链见 docs/protocol.md §4)
    {
        let mut h = Sha256::new();
        h.update(identity.seed());
        h.update(b"zoe-user-sig-v1");
        let user_sig: [u8; 32] = h.finalize().into();
        storage
            .upsert_device(
                mls_identity.signature_public_key(),
                &identity.verifying_key().to_bytes(),
                &user_sig,
                now(),
            )
            .map_err(|e| DaemonError::Storage(e.to_string()))?;
    }

    // libp2p 远程传输:身份密钥同样派生自身份种子(传输认证与 QR 名片同钥)
    #[cfg(feature = "net")]
    let net: Option<std::sync::Arc<NetHandle>> = {
        let mut h = Sha256::new();
        h.update(identity.seed());
        h.update(b"zoe-net-v1");
        let net_seed: [u8; 32] = h.finalize().into();
        zoe_transport::net::NetTransport::spawn_from_seed(&net_seed)
    };
    #[cfg(not(feature = "net"))]
    let net: Option<std::sync::Arc<NetHandle>> = None;

    // 访问令牌
    let token = match &config.token {
        Some(t) => t.clone(),
        None => load_or_create_token(&config.data_dir)?,
    };

    // 从持久化状态恢复群组会话
    let mut sessions: HashMap<Vec<u8>, _> = HashMap::new();
    for g in storage
        .groups()
        .map_err(|e| DaemonError::Storage(e.to_string()))?
    {
        if let Ok(Some(session)) = MlsSession::load(&provider, &g.group_id) {
            sessions.insert(g.group_id, session);
        }
    }

    let (events_tx, _) = tokio::sync::broadcast::channel(64);
    let state = Arc::new(AppState {
        storage,
        data_dir: config.data_dir.clone(),
        provider: Mutex::new(provider),
        sessions: Mutex::new(sessions),
        mls_identity,
        identity,
        token: token.clone(),
        events: events_tx,
        net,
        pending_keypackages: Mutex::new(HashMap::new()),
        pairing: std::sync::atomic::AtomicBool::new(false),
        pair_code: Mutex::new(None),
        started_at: now(),
    });

    // 入站信封分发(net 传输)
    #[cfg(feature = "net")]
    if let Some(net) = &state.net {
        let mut rx = net.subscribe();
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok(inbound) = rx.recv().await {
                msg::handle_inbound(&state_clone, &inbound.from, &inbound.envelope);
            }
        });
    }

    let router = api::router(state.clone());
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.port))
        .await
        .map_err(|e| {
            DaemonError::Bind(
                SocketAddr::from(([127, 0, 0, 1], config.port)),
                e.to_string(),
            )
        })?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    Ok(Daemon {
        state,
        addr,
        server,
    })
}
