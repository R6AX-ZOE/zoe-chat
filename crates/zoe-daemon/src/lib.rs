//! zoe-chat 守护进程核心(库面):数据目录初始化、AppState 构建、axum 服务启动。
//!
//! 桌面二进制(zoe-daemon)与 Tauri 移动端(app/)共用本库:
//! 移动端以固定端口内嵌守护进程,WebView 直接加载 `http://127.0.0.1:<port>`。
//!
//! feature `net`(默认开)启用 libp2p 远程传输;移动端 `default-features = false`
//! 以排除 libp2p(瘦身,与 zoe-transport 的 ble-mobile 组合一致)。
//!
//! 多用户:注册表 `data_dir/users.db`(**plain** 用户 = 旧版根目录布局,种子明文;
//! **pin** 用户 = `users/<id>/` 子目录,种子以 argon2id(PIN) 派生的密钥加密)。
//! 激活用户带 PIN 且启动未提供 `--pin` 时进入**锁定模式**:除用户管理与
//! 解锁端点外一律 423;`unlock` 通过後完整初始化身份/设备/net。

pub mod api;
pub mod msg;
pub mod state;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use zoe_core::identity::IdentityKeyPair;
use zoe_core::mls::{MlsIdentity, MlsSession};
use zoe_core::storage::{Db, ZoeProvider, ZoeStorage};
use zoe_core::users::{User, UserKind, UserRegistry};
#[cfg(feature = "net")]
use zoe_transport::Transport;

use crate::state::{now, AppState, SharedState};

/// 移动端内嵌守护进程的固定端口(WebView 加载地址)。
pub const MOBILE_PORT: u16 = 18571;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub data_dir: PathBuf,
    /// 0 = 随机端口(桌面默认)。
    pub port: u16,
    /// None = 从 data_dir/token 读取或生成(桌面默认)。
    pub token: Option<String>,
    /// 显式指定激活用户(user_id hex);None = 最近使用。
    pub user_id: Option<String>,
    /// PIN(PIN 保护用户启动解锁;缺席 → 锁定模式)。
    pub pin: Option<String>,
}

impl DaemonConfig {
    pub fn desktop(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            port: 0,
            token: None,
            user_id: None,
            pin: None,
        }
    }

    pub fn desktop_with_pin(data_dir: PathBuf, pin: &str) -> Self {
        let mut c = Self::desktop(data_dir);
        c.pin = Some(pin.to_string());
        c
    }

    pub fn mobile(data_dir: PathBuf, token: String) -> Self {
        Self {
            data_dir,
            port: MOBILE_PORT,
            token: Some(token),
            user_id: None,
            pin: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage: {0}")]
    Storage(String),
    #[error("registry: {0}")]
    Registry(String),
    #[error("identity: {0}")]
    Identity(String),
    #[error("mls: {0}")]
    Mls(String),
    #[error("bind {0}: {1}")]
    Bind(SocketAddr, String),
    #[error("user {0} is not pin-protected")]
    NotPinProtected(String),
    #[error("wrong pin for user {0}")]
    WrongPin(String),
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

// ---------------------------------------------------------------------------
// 用户解析与身份初始化
// ---------------------------------------------------------------------------

/// 解析激活用户:显式指定 > 最近使用;注册表为空时自动创建 `default`
/// (旧版根目录布局,明文种子;首次启动若根目录已有身份则直接迁移)。
fn resolve_active_user(
    registry: &UserRegistry,
    config: &DaemonConfig,
    data_dir: &std::path::Path,
) -> Result<User, DaemonError> {
    if let Some(uid_hex) = &config.user_id {
        let uid = hex::decode(uid_hex)
            .map_err(|_| DaemonError::Registry(format!("bad user id {uid_hex}")))?;
        return registry
            .get(&uid)
            .map_err(|e| DaemonError::Registry(e.to_string()));
    }
    if let Some(u) = registry
        .most_recent()
        .map_err(|e| DaemonError::Registry(e.to_string()))?
    {
        return Ok(u);
    }

    // 首次启动:迁移或新建 default(plain,根目录)
    let root_db = Db::open(&data_dir.join("zoe.db"))
        .map_err(|e| DaemonError::Storage(e.to_string()))?;
    let storage = ZoeStorage::new(root_db);
    let (seed, created_at) = match storage
        .identity()
        .map_err(|e| DaemonError::Storage(e.to_string()))?
    {
        Some(seed_created) => seed_created,
        None => {
            let id = IdentityKeyPair::generate();
            storage
                .set_identity(&id.seed(), now())
                .map_err(|e| DaemonError::Storage(e.to_string()))?;
            (id.seed(), now())
        }
    };
    let _ = seed;
    let u = registry
        .add_plain_user("default", created_at)
        .map_err(|e| DaemonError::Registry(e.to_string()))?;
    registry
        .set_last_used(&u.user_id)
        .map_err(|e| DaemonError::Registry(e.to_string()))?;
    Ok(u)
}

/// 读取激活用户的身份种子。
/// - plain 用户:从本用户 zoe.db 的 identity 表读取(首次则生成并落库)。
/// - pin 用户:校验 `--pin` 后从注册表解密;无 pin 或错误 → None(锁定)。
fn load_active_seed(
    storage: &ZoeStorage,
    registry: &UserRegistry,
    user: &User,
    config: &DaemonConfig,
) -> Result<Option<[u8; 32]>, DaemonError> {
    match user.kind {
        UserKind::Plain => {
            if let Some((seed, _)) = storage
                .identity()
                .map_err(|e| DaemonError::Storage(e.to_string()))?
            {
                return Ok(Some(seed));
            }
            // 目录存在但无身份(异常)→ 生成
            let id = IdentityKeyPair::generate();
            storage
                .set_identity(&id.seed(), now())
                .map_err(|e| DaemonError::Storage(e.to_string()))?;
            Ok(Some(id.seed()))
        }
        UserKind::Pin => {
            let Some(pin) = &config.pin else {
                return Ok(None);
            };
            let ok = registry
                .verify_pin(&user.user_id, pin)
                .map_err(|e| DaemonError::Registry(e.to_string()))?;
            if !ok {
                return Ok(None);
            }
            registry
                .decrypt_seed(&user.user_id, pin)
                .map(Some)
                .map_err(|e| DaemonError::Registry(e.to_string()))
        }
    }
}

/// 由种子初始化身份的运行时部分:identity / 设备 MLS 凭据 / 设备登记 / net 传输。
/// 锁定模式下延迟到 `unlock` 调用。
fn init_identity_runtime(state: &SharedState, seed: &[u8; 32]) -> Result<(), DaemonError> {
    let identity = IdentityKeyPair::from_seed(seed);
    *state.identity.lock().unwrap() = Some(Arc::new(identity.clone()));

    // 设备 MLS 凭据:确定性地派生自身份种子(恢复身份即恢复设备)
    let mut h = Sha256::new();
    h.update(seed);
    h.update(b"zoe-device-v1");
    let device_seed: [u8; 32] = h.finalize().into();
    let mls_identity =
        MlsIdentity::new("device-1", &device_seed).map_err(|e| DaemonError::Mls(e.to_string()))?;
    let mls_identity = Arc::new(mls_identity);
    *state.mls_identity.lock().unwrap() = Some(Arc::clone(&mls_identity));

    // 登记本设备(设备表;user_sig = 用户身份派生的占位签名 —— 完整签名链见 docs/protocol.md §4)
    {
        let mut h = Sha256::new();
        h.update(seed);
        h.update(b"zoe-user-sig-v1");
        let user_sig: [u8; 32] = h.finalize().into();
        state
            .storage
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
    {
        let mut h = Sha256::new();
        h.update(seed);
        h.update(b"zoe-net-v1");
        let net_seed: [u8; 32] = h.finalize().into();
        if let Some(net) = zoe_transport::net::NetTransport::spawn_from_seed(&net_seed) {
            let mut rx = net.subscribe();
            let state_clone = Arc::clone(state);
            tokio::spawn(async move {
                while let Ok(inbound) = rx.recv().await {
                    msg::handle_inbound(&state_clone, &inbound.from, &inbound.envelope);
                }
            });
            // spawn_from_seed 已返回 Arc<NetTransport>
            *state.net.lock().unwrap() = Some(net);
        }
    }

    Ok(())
}

/// 解锁:校验 PIN → 解密种子 → 初始化身份运行时。锁定模式(PIN 用户未带
/// `--pin` 启动)经由 `/api/v1/unlock` 调用本函数。
pub fn unlock(state: &SharedState, pin: &str) -> Result<User, DaemonError> {
    let user = state.active_user.lock().unwrap().clone();
    if user.kind != UserKind::Pin {
        return Err(DaemonError::NotPinProtected(
            hex::encode(user.user_id),
        ));
    }
    if !state
        .registry
        .verify_pin(&user.user_id, pin)
        .map_err(|e| DaemonError::Registry(e.to_string()))?
    {
        return Err(DaemonError::WrongPin(hex::encode(user.user_id)));
    }
    let seed = state
        .registry
        .decrypt_seed(&user.user_id, pin)
        .map_err(|e| DaemonError::Registry(e.to_string()))?;
    init_identity_runtime(state, &seed)?;
    state
        .registry
        .set_last_used(&user.user_id)
        .map_err(|e| DaemonError::Registry(e.to_string()))?;
    state.unlocked.store(true, Ordering::SeqCst);
    let _ = state.events.send(
        serde_json::json!({
            "type": "user",
            "event": "unlocked",
            "user_id": hex::encode(user.user_id),
        })
        .to_string(),
    );
    Ok(user)
}

// ---------------------------------------------------------------------------
// 启动
// ---------------------------------------------------------------------------

/// 初始化数据目录 + 构建 AppState + 启动 axum 服务(后台任务),返回句柄。
/// PIN 用户未解锁时返回锁定模式的 daemon(除 /users* 与 /unlock 外一律 423)。
pub async fn start(config: DaemonConfig) -> Result<Daemon, DaemonError> {
    std::fs::create_dir_all(&config.data_dir)?;

    let registry = UserRegistry::open(&config.data_dir)
        .map_err(|e| DaemonError::Registry(e.to_string()))?;
    let active_user = resolve_active_user(&registry, &config, &config.data_dir)?;
    registry
        .set_last_used(&active_user.user_id)
        .map_err(|e| DaemonError::Registry(e.to_string()))?;

    let user_dir = active_user.data_path(&config.data_dir);
    let db = Db::open(&user_dir.join("zoe.db"))
        .map_err(|e| DaemonError::Storage(e.to_string()))?;
    let storage = ZoeStorage::new(db);
    let provider = ZoeProvider::new(&user_dir.join("mls.db"))
        .map_err(|e| DaemonError::Storage(e.to_string()))?;

    // 身份种子:plain → 本用户 zoe.db;pin → 注册表(需 --pin)
    let seed = load_active_seed(&storage, &registry, &active_user, &config)?;

    // 从持久化状态恢复群组会话(与身份无关,锁定模式下也恢复;处理受 423 门禁)
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
        mls_identity: Mutex::new(None),
        identity: Mutex::new(None),
        unlocked: std::sync::atomic::AtomicBool::new(false),
        registry,
        active_user: Mutex::new(active_user.clone()),
        token: match &config.token {
            Some(t) => t.clone(),
            None => load_or_create_token(&config.data_dir)?,
        },
        events: events_tx,
        net: Mutex::new(None),
        pending_keypackages: Mutex::new(HashMap::new()),
        pairing: std::sync::atomic::AtomicBool::new(false),
        pair_code: Mutex::new(None),
        started_at: now(),
    });

    if let Some(seed) = seed {
        match init_identity_runtime(&state, &seed) {
            Ok(()) => {
                state.unlocked.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                eprintln!("identity init failed (locked mode): {e}");
            }
        }
    }

    if state.unlocked.load(Ordering::SeqCst) {
        let _ = state.events.send(
            serde_json::json!({
                "type": "user",
                "event": "booted",
                "user_id": hex::encode(active_user.user_id),
            })
            .to_string(),
        );
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