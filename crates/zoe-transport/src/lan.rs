//! 局域网传输:UDP 组播信标发现 + TCP 信封流(feature `lan`,桌面/移动端共用)。
//!
//! 设计:
//! - 发现:每 5s 向组播组 `239.255.99.87:28571` 发信标 JSON
//!   `{"id":"<peer_id>","port":28572,"ts":<unix>}`;收到信标即登记 peer
//!   (15s 未刷新过期)。Android 上接收组播需要 `WifiManager.MulticastLock`
//!   (见 android/gen-patches/MainActivity.kt)。
//! - 数据面:对称协议 —— 每方连上后先写 hello(JSON id,长度前缀),再读对方
//!   hello 拿到 peer_id 并登记;随后双向长度前缀帧(u32 LE)承载 Envelope。
//!   每 peer 一个持久写任务 + 连接槽位(写半只注册一次,重复连接只读),
//!   出站帧经队列串行写出;入站按信封 hash 去重(24h,与 MeshOverlay 同范式)。
//! - 身份:由 32B 种子派生 Ed25519 密钥,peer_id = 公钥 hex。
//! - 仅 tokio + std + zoe-core(Ed25519);新增依赖仅 serde_json。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{broadcast, mpsc, Mutex as TokioMutex, Notify};
use zoe_core::envelope::Envelope;
use zoe_core::identity::IdentityKeyPair;

use crate::{Availability, Inbound, Transport, TransportError};

/// 组播组 / 信标端口(与 TCP 数据端口分离)。
pub const LAN_GROUP: &str = "239.255.99.87";
pub const LAN_BEACON_PORT: u16 = 28571;
/// 数据监听端口(固定,便于直连拨号)。
pub const LAN_TCP_PORT: u16 = 28572;
/// 信标周期 / peer 过期时间。
pub const BEACON_INTERVAL: Duration = Duration::from_secs(5);
pub const PEER_EXPIRE: Duration = Duration::from_secs(15);
/// 每 peer 待发送队列上限。
pub const MAX_PENDING_PER_PEER: usize = 256;
/// 信封序列化上限(16 MiB 载荷 + 余量)。
const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024 + 1024;
/// hello 报文上限。
const MAX_HELLO_BYTES: u32 = 1024;
/// 入站去重 TTL / 上限(与 MeshOverlay 同范式)。
const DEDUP_TTL: Duration = Duration::from_secs(24 * 3600);
const DEDUP_CAP: usize = 8192;
/// 拨号去重窗口。
const DIAL_WINDOW: Duration = Duration::from_secs(10);

struct PeerState {
    addr: SocketAddr,
    last_seen: Instant,
}

/// 每 peer 的连接写槽:任何时刻至多一条连接的写半挂在槽上,由持久写任务取用。
struct PeerWriter {
    conn: TokioMutex<Option<tokio::net::tcp::OwnedWriteHalf>>,
    waker: Notify,
}

struct LanInner {
    peer_id: String,
    inbound: broadcast::Sender<Inbound>,
    peers: Mutex<HashMap<String, PeerState>>,
    /// 每 peer 出站帧队列 + 写任务配套。
    channels: Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>,
    writers: Mutex<HashMap<String, Arc<PeerWriter>>>,
    /// 拨号去重。
    dialing: Mutex<HashMap<String, Instant>>,
    /// 入站去重(hash[..8] hex → Instant)。
    dedup: Mutex<HashMap<String, Instant>>,
}

/// 局域网传输句柄(跨线程共享)。
pub struct LanTransport {
    inner: Arc<LanInner>,
}

impl LanTransport {
    /// 由 32 字节种子派生身份并启动传输(后台任务持有 socket)。
    pub fn spawn_from_seed(seed: &[u8; 32]) -> Arc<Self> {
        let key = IdentityKeyPair::from_seed(seed);
        let peer_id = hex::encode(key.verifying_key().to_bytes());
        let (inbound_tx, _) = broadcast::channel(512);
        let inner = Arc::new(LanInner {
            peer_id: peer_id.clone(),
            inbound: inbound_tx,
            peers: Mutex::new(HashMap::new()),
            channels: Mutex::new(HashMap::new()),
            writers: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashMap::new()),
            dedup: Mutex::new(HashMap::new()),
        });
        let t = Arc::new(Self {
            inner: Arc::clone(&inner),
        });
        tokio::spawn(run(peer_id, Arc::clone(&inner)));
        t
    }

    pub fn local_peer_id(&self) -> String {
        self.inner.peer_id.clone()
    }
}

impl Transport for LanTransport {
    fn name(&self) -> &'static str {
        "lan"
    }

    fn availability(&self) -> Availability {
        Availability::Up
    }

    fn peers(&self) -> Vec<String> {
        let mut peers = self.inner.peers.lock().unwrap();
        peers.retain(|_, p| p.last_seen.elapsed() < PEER_EXPIRE);
        peers.keys().cloned().collect()
    }

    fn dial(
        &self,
        addr: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send>>
    {
        let inner = Arc::clone(&self.inner);
        let addr = addr.to_string();
        Box::pin(async move {
            let addr: SocketAddr = addr
                .parse()
                .map_err(|e| TransportError::Io(format!("invalid addr {addr}: {e}")))?;
            spawn_outbound(&inner, addr);
            Ok(())
        })
    }

    fn send(
        &self,
        to: &str,
        envelope: Envelope,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send>>
    {
        let inner = Arc::clone(&self.inner);
        let to = to.to_string();
        Box::pin(async move {
            let addr = {
                let mut peers = inner.peers.lock().unwrap();
                peers.retain(|_, p| p.last_seen.elapsed() < PEER_EXPIRE);
                peers.get(&to).map(|p| p.addr)
            };
            let Some(addr) = addr else {
                return Err(TransportError::UnknownPeer(to.to_string()));
            };
            let bytes = encode_env(&envelope);
            let mut channels = inner.channels.lock().unwrap();
            let tx = match channels.get(&to) {
                Some(tx) => tx.clone(),
                None => {
                    let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
                    channels.insert(to.clone(), tx.clone());
                    let pw = {
                        let mut writers = inner.writers.lock().unwrap();
                        writers
                            .entry(to.clone())
                            .or_insert_with(|| {
                                Arc::new(PeerWriter {
                                    conn: TokioMutex::new(None),
                                    waker: Notify::new(),
                                })
                            })
                            .clone()
                    };
                    let inner_w = Arc::clone(&inner);
                    tokio::spawn(peer_writer_task(inner_w, pw, rx));
                    tx
                }
            };
            drop(channels);
            let _ = tx.try_send(bytes);
            ensure_dialing(&inner, &to, addr);
            Ok(())
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<Inbound> {
        self.inner.inbound.subscribe()
    }
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 长度前缀帧:u32 LE 长度 + 载荷。
fn frame(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 4);
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

fn encode_env(env: &Envelope) -> Vec<u8> {
    frame(&env.encode())
}

/// 拨号去重:DIAL_WINDOW 内同一 peer 只起一条出站。
fn ensure_dialing(inner: &Arc<LanInner>, peer_id: &str, addr: SocketAddr) {
    let mut dialing = inner.dialing.lock().unwrap();
    let now = Instant::now();
    dialing.retain(|_, t| t.elapsed() < DIAL_WINDOW);
    if dialing.contains_key(peer_id) {
        return;
    }
    dialing.insert(peer_id.to_string(), now);
    spawn_outbound(inner, addr);
}

fn spawn_outbound(inner: &Arc<LanInner>, addr: SocketAddr) {
    let inner = Arc::clone(inner);
    tokio::spawn(async move {
        if let Err(e) = outbound_conn(&inner, addr).await {
            eprintln!("[lan] outbound {addr}: {e}");
        }
    });
}

async fn outbound_conn(inner: &Arc<LanInner>, addr: SocketAddr) -> Result<(), TransportError> {
    let stream = TcpStream::connect(addr)
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;
    conn_loop(inner, stream, &inner.peer_id).await
}

async fn inbound_conn(inner: &Arc<LanInner>, stream: TcpStream) -> Result<(), TransportError> {
    conn_loop(inner, stream, &inner.peer_id).await
}

/// 持久写任务:从队列取帧,经连接槽写出;无连接时等待连接注册。
async fn peer_writer_task(
    inner: Arc<LanInner>,
    pw: Arc<PeerWriter>,
    mut rx: mpsc::Receiver<Vec<u8>>,
) {
    while let Some(bytes) = rx.recv().await {
        loop {
            let mut slot = pw.conn.lock().await;
            match slot.as_mut() {
                Some(half) => match half.write_all(&bytes).await {
                    Ok(()) => break,
                    Err(_) => {
                        *slot = None;
                        pw.waker.notify_one();
                    }
                },
                None => {
                    drop(slot);
                    pw.waker.notified().await;
                }
            }
        }
    }
    let mut slot = pw.conn.lock().await;
    *slot = None;
    pw.waker.notify_one();
    let _ = inner;
}

/// 双向连接处理(出站/入站共用):对称 hello → 登记 → 读写。
async fn conn_loop(
    inner: &Arc<LanInner>,
    stream: TcpStream,
    my_id: &str,
) -> Result<(), TransportError> {
    let peer_addr = stream
        .peer_addr()
        .map_err(|e| TransportError::Io(e.to_string()))?;
    let (mut read_half, mut write_half) = stream.into_split();

    // 1) 对称 hello:先写自己的,再读对方的
    write_half
        .write_all(&frame(format!("{{\"id\":\"{my_id}\"}}").as_bytes()))
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;

    let hello_len = read_u32(&mut read_half).await?;
    if hello_len > MAX_HELLO_BYTES {
        return Err(TransportError::Io("bad hello".into()));
    }
    let mut hello_buf = vec![0u8; hello_len as usize];
    read_exact_bytes(&mut read_half, &mut hello_buf).await?;
    let peer_id: String = serde_json::from_slice(&hello_buf)
        .ok()
        .and_then(|v: serde_json::Value| v.get("id").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_default();
    if peer_id.is_empty() || peer_id == my_id {
        return Ok(());
    }
    inner.peers.lock().unwrap().insert(
        peer_id.clone(),
        PeerState {
            addr: peer_addr,
            last_seen: Instant::now(),
        },
    );

    // 2) 写侧:注册到 peer 写槽(仅首条连接持有;重复连接丢弃写半只读)
    let pw = {
        let mut writers = inner.writers.lock().unwrap();
        writers
            .entry(peer_id.clone())
            .or_insert_with(|| {
                Arc::new(PeerWriter {
                    conn: TokioMutex::new(None),
                    waker: Notify::new(),
                })
            })
            .clone()
    };
    {
        let mut slot = pw.conn.lock().await;
        if slot.is_none() {
            *slot = Some(write_half);
            pw.waker.notify_one();
        }
    }

    // 3) 读循环:收帧 → 去重 → inbound
    loop {
        let len = match read_u32(&mut read_half).await {
            Ok(l) if l <= MAX_FRAME_BYTES => l as usize,
            _ => break,
        };
        let mut buf = vec![0u8; len];
        if read_exact_bytes(&mut read_half, &mut buf).await.is_err() {
            break;
        }
        let Ok(env) = Envelope::decode(&buf) else {
            continue;
        };
        let key = hex::encode(&env.hash[..8]);
        {
            let mut dedup = inner.dedup.lock().unwrap();
            if dedup.len() > DEDUP_CAP {
                dedup.retain(|_, t| t.elapsed() < DEDUP_TTL);
            }
            if dedup.contains_key(&key) {
                continue;
            }
            dedup.insert(key, Instant::now());
        }
        let _ = inner.inbound.send(Inbound {
            from: peer_id.clone(),
            envelope: env,
        });
    }

    // 4) 清理写槽(若仍是我们这把)
    {
        let mut slot = pw.conn.lock().await;
        *slot = None;
        pw.waker.notify_one();
    }
    Ok(())
}

async fn read_u32(s: &mut tokio::net::tcp::OwnedReadHalf) -> Result<u32, TransportError> {
    let mut b = [0u8; 4];
    read_exact_bytes(s, &mut b).await?;
    Ok(u32::from_le_bytes(b))
}

async fn read_exact_bytes(
    s: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut [u8],
) -> Result<(), TransportError> {
    let mut read = 0usize;
    while read < buf.len() {
        let n = s
            .read(&mut buf[read..])
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        if n == 0 {
            return Err(TransportError::Io("eof".into()));
        }
        read += n;
    }
    Ok(())
}

/// 后台主循环:组播信标收发 + TCP 数据监听。
async fn run(peer_id: String, inner: Arc<LanInner>) {
    let beacon = match UdpSocket::bind(("0.0.0.0", LAN_BEACON_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[lan] beacon bind: {e}");
            return;
        }
    };
    let _ = beacon.join_multicast_v4(LAN_GROUP.parse().unwrap(), "0.0.0.0".parse().unwrap());
    let _ = beacon.set_multicast_loop_v4(true);

    let listener = match TcpListener::bind(("0.0.0.0", LAN_TCP_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[lan] tcp listen {LAN_TCP_PORT}: {e}");
            return;
        }
    };

    let mut buf = [0u8; 2048];
    let mut next_beacon = tokio::time::Instant::now();
    loop {
        tokio::select! {
            n = beacon.recv_from(&mut buf) => {
                let Ok((n, src)) = n else { break };
                if let Ok(msg) = std::str::from_utf8(&buf[..n]) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) {
                        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                        if !id.is_empty() && id != peer_id {
                            let port = v.get("port").and_then(|x| x.as_u64()).unwrap_or(LAN_TCP_PORT as u64) as u16;
                            let addr = SocketAddr::new(src.ip(), port);
                            inner.peers.lock().unwrap().insert(
                                id.to_string(),
                                PeerState { addr, last_seen: Instant::now() },
                            );
                        }
                    }
                }
            }
            conn = listener.accept() => {
                if let Ok((stream, _peer)) = conn {
                    let inner = Arc::clone(&inner);
                    tokio::spawn(async move {
                        if let Err(e) = inbound_conn(&inner, stream).await {
                            eprintln!("[lan] inbound: {e}");
                        }
                    });
                }
            }
            _ = tokio::time::sleep_until(next_beacon) => {
                next_beacon = tokio::time::Instant::now() + BEACON_INTERVAL;
                let msg = format!(
                    "{{\"id\":\"{peer_id}\",\"port\":{LAN_TCP_PORT},\"ts\":{}}}",
                    now_ts()
                );
                let _ = beacon
                    .send_to(msg.as_bytes(), format!("{LAN_GROUP}:{LAN_BEACON_PORT}"))
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(seq: u64, payload_len: usize) -> Envelope {
        Envelope::new(0, 1, b"g1".to_vec(), 0, 0, seq, vec![0x42; payload_len])
    }

    fn seed(i: u8) -> [u8; 32] {
        let mut s = [0u8; 32];
        s[0] = i;
        s
    }

    #[tokio::test]
    async fn frame_roundtrip() {
        let env = env_with(1, 64);
        let bytes = encode_env(&env);
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut idx = 0usize;
        while idx + 4 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[idx..idx + 4].try_into().unwrap()) as usize;
            frames.push(bytes[idx + 4..idx + 4 + len].to_vec());
            idx += 4 + len;
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(Envelope::decode(&frames[0]).unwrap(), env);
    }

    #[tokio::test]
    async fn identity_from_seed() {
        let a = LanTransport::spawn_from_seed(&seed(1));
        let b = LanTransport::spawn_from_seed(&seed(2));
        assert_eq!(a.local_peer_id().len(), 64);
        assert_ne!(a.local_peer_id(), b.local_peer_id());
    }

    #[tokio::test]
    async fn unknown_peer_rejected() {
        let a = LanTransport::spawn_from_seed(&seed(3));
        let env = env_with(1, 16);
        assert!(matches!(
            a.send("0000", env).await,
            Err(TransportError::UnknownPeer(_))
        ));
    }
}
