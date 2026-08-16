//! zoe-chat 守护进程入口。
//!
//! 用法:zoe-daemon [--data-dir PATH] [--port N] [--token STR]
//! 默认监听 127.0.0.1 随机端口;首次启动生成访问令牌写入 data-dir/token。

mod api;
mod msg;
mod state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use zoe_core::identity::IdentityKeyPair;
use zoe_core::mls::{MlsIdentity, MlsSession};
use zoe_core::storage::{Db, ZoeProvider, ZoeStorage};
use zoe_transport::net::NetTransport;
use zoe_transport::Transport;

use crate::state::{now, AppState};

struct Args {
    data_dir: PathBuf,
    port: u16,
    token: Option<String>,
}

fn parse_args() -> Args {
    let mut data_dir = PathBuf::from("zoe-data");
    let mut port: u16 = 0;
    let mut token = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                i += 1;
                data_dir = PathBuf::from(args.get(i).expect("--data-dir needs a value"));
            }
            "--port" => {
                i += 1;
                port = args
                    .get(i)
                    .expect("--port needs a value")
                    .parse()
                    .expect("invalid port");
            }
            "--token" => {
                i += 1;
                token = Some(args.get(i).expect("--token needs a value").clone());
            }
            other => panic!("unknown argument: {other}"),
        }
        i += 1;
    }
    Args {
        data_dir,
        port,
        token,
    }
}

fn main() {
    let args = parse_args();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(run(args));
}

fn load_or_create_token(dir: &Path) -> String {
    let path = dir.join("token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let token = hex::encode(bytes);
    std::fs::write(&path, &token).expect("write token file");
    println!("generated access token (stored in {})", path.display());
    token
}

async fn run(args: Args) {
    std::fs::create_dir_all(&args.data_dir).expect("create data dir");

    let db = Db::open(&args.data_dir.join("zoe.db")).expect("open zoe.db");
    let storage = ZoeStorage::new(db);
    let provider = ZoeProvider::new(&args.data_dir.join("mls.db")).expect("open mls.db");

    // 用户身份(不存在则生成)
    let identity = match storage.identity().expect("read identity") {
        Some((seed, _)) => IdentityKeyPair::from_seed(&seed),
        None => {
            let id = IdentityKeyPair::generate();
            storage
                .set_identity(&id.seed(), now())
                .expect("store identity");
            println!("new identity generated");
            id
        }
    };

    // 设备 MLS 凭据:确定性地派生自身份种子(恢复身份即恢复设备)
    let mut h = Sha256::new();
    h.update(identity.seed());
    h.update(b"zoe-device-v1");
    let device_seed: [u8; 32] = h.finalize().into();
    let mls_identity = MlsIdentity::new("device-1", &device_seed).expect("mls identity");

    // libp2p 远程传输:身份密钥同样派生自身份种子(传输认证与 QR 名片同钥)
    let mut h = Sha256::new();
    h.update(identity.seed());
    h.update(b"zoe-net-v1");
    let net_seed: [u8; 32] = h.finalize().into();
    let net = NetTransport::spawn_from_seed(&net_seed);
    if let Some(net) = &net {
        println!("net peer id: {}", net.local_peer_id());
    }

    // 访问令牌
    let token = match &args.token {
        Some(t) => t.clone(),
        None => load_or_create_token(&args.data_dir),
    };

    // 从持久化状态恢复群组会话
    let mut sessions: HashMap<Vec<u8>, _> = HashMap::new();
    for g in storage.groups().expect("list groups") {
        if let Ok(Some(session)) = MlsSession::load(&provider, &g.group_id) {
            sessions.insert(g.group_id, session);
        }
    }
    if !sessions.is_empty() {
        println!("restored {} group session(s)", sessions.len());
    }

    let (events_tx, _) = tokio::sync::broadcast::channel(64);
    let state = Arc::new(AppState {
        storage,
        provider: Mutex::new(provider),
        sessions: Mutex::new(sessions),
        mls_identity,
        identity,
        token: token.clone(),
        events: events_tx,
        net,
        pending_keypackages: Mutex::new(HashMap::new()),
        started_at: now(),
    });

    // 入站信封分发(net 传输)
    if let Some(net) = &state.net {
        let mut rx = net.subscribe();
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok(inbound) = rx.recv().await {
                msg::handle_inbound(&state_clone, &inbound.from, &inbound.envelope);
            }
        });
    }

    let router = api::router(state);
    let listener = TcpListener::bind(format!("127.0.0.1:{}", args.port))
        .await
        .expect("bind 127.0.0.1");
    let actual = listener.local_addr().expect("local addr");
    println!("zoe-chat daemon ready: http://{actual}");
    if args.token.is_none() {
        println!("access token: {token}");
    }
    println!("data dir: {}", args.data_dir.display());

    axum::serve(listener, router).await.expect("server error");
}
