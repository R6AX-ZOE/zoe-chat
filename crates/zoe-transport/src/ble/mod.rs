//! BLE GATT 网状覆盖网(docs/DESIGN.md §6.2)。
//!
//! 平台无关核心:
//! - `BleDriver`/`BleConn` trait —— 平台面(linux.rs 用 bluer;windows.rs 用
//!   btleplug + windows-rs),上层逻辑只依赖本面;
//! - 帧格式(docs/envelope.md §2.1,含 msg_id/ttl):
//!   [magic 0x5A | msg_id 8 | ttl 1 | chunk_idx 1 | total 2 | data ≤499];
//! - `MeshOverlay`:存储转发网状覆盖 —— 分片/重组、去重(hash 前缀,24h)、
//!   TTL 限跳、邻居管理;实现 `Transport` trait;
//! - `MockHub`/`MockDriver`:内存 mock,无硬件下的完整测试。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};
use zoe_core::envelope::Envelope;

use crate::{Availability, Inbound, Transport, TransportError};

#[cfg(all(feature = "ble-linux", target_os = "linux"))]
pub mod linux;
#[cfg(all(feature = "ble-windows", windows))]
pub mod windows;

pub const BLE_MAGIC: u8 = 0x5A;
pub const FRAME_HEADER_LEN: usize = 13; // magic(1)+msg_id(8)+ttl(1)+chunk_idx(1)+total(2)
pub const MAX_DATA_PER_FRAME: usize = 499;
pub const MAX_TOTAL_CHUNKS: u16 = 512;
pub const DEDUP_TTL: Duration = Duration::from_secs(24 * 3600);
pub const SCAN_INTERVAL: Duration = Duration::from_secs(5);
pub const REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(60);

/// zoe GATT 服务与特性 UUID(规范串见 docs/termux-ble.md §5):
/// 服务 7a5e0001-2e4c-4a31-9b6c-3c2a0e5f6a01
/// 写   7a5e0002-2e4c-4a31-9b6c-3c2a0e5f6a01(客户端→服务端)
/// 通知 7a5e0003-2e4c-4a31-9b6c-3c2a0e5f6a01(服务端→客户端)
pub const SERVICE_UUID: uuid::Uuid =
    uuid::Uuid::from_u128(0x7a5e_0001_2e4c_4a31_9b6c_3c2a_0e5f_6a01);
pub const WRITE_CHAR_UUID: uuid::Uuid =
    uuid::Uuid::from_u128(0x7a5e_0002_2e4c_4a31_9b6c_3c2a_0e5f_6a01);
pub const NOTIFY_CHAR_UUID: uuid::Uuid =
    uuid::Uuid::from_u128(0x7a5e_0003_2e4c_4a31_9b6c_3c2a_0e5f_6a01);

// ---------------------------------------------------------------------------
// 平台面
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BleAddr(pub Vec<u8>);

impl BleAddr {
    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self, String> {
        Ok(Self(hex::decode(s).map_err(|e| e.to_string())?))
    }

    /// 解析 "AA:BB:CC:DD:EE:FF" → 6 字节 MAC(跨驱动统一表示,Linux/Windows 一致)。
    pub fn from_mac_str(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 6 {
            return Err(format!("bad mac: {s}"));
        }
        let mut out = Vec::with_capacity(6);
        for p in parts {
            out.push(u8::from_str_radix(p, 16).map_err(|e| e.to_string())?);
        }
        Ok(Self(out))
    }

    /// 6 字节 MAC → "AA:BB:CC:DD:EE:FF"(非 6 字节时回退 hex)。
    pub fn to_mac(&self) -> String {
        if self.0.len() == 6 {
            self.0
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(":")
        } else {
            self.to_hex()
        }
    }
}

#[derive(Debug)]
pub struct BleError(pub String);

impl std::fmt::Display for BleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ble: {}", self.0)
    }
}

impl std::error::Error for BleError {}

pub struct BlePeer {
    pub addr: BleAddr,
    pub name: String,
    /// 广播载荷中携带了 zoe 服务 UUID(扫描结果中快速识别 zoe 设备)。
    pub zoe: bool,
}

/// 一条已建立的 GATT 连接。
pub trait BleConn: Send + 'static {
    fn peer_addr(&self) -> BleAddr;
    fn write(&self, frame: &[u8]) -> impl Future<Output = Result<(), BleError>> + Send;
    fn read(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, BleError>> + Send;
}

/// 平台蓝牙驱动(广告、扫描、连接、被连接)。
pub trait BleDriver: Send + Sync + 'static {
    type Conn: BleConn;
    fn driver_name(&self) -> &'static str;
    fn start_advertising(&self, name: &str) -> impl Future<Output = Result<(), BleError>> + Send;
    fn stop_advertising(&self) -> impl Future<Output = Result<(), BleError>> + Send;
    fn scan(
        &self,
        timeout: Duration,
    ) -> impl Future<Output = Result<Vec<BlePeer>, BleError>> + Send;
    fn connect(&self, addr: &BleAddr) -> impl Future<Output = Result<Self::Conn, BleError>> + Send;
    fn listen(&self) -> impl Future<Output = Result<mpsc::Receiver<Self::Conn>, BleError>> + Send;
}

// ---------------------------------------------------------------------------
// 帧分片/重组
// ---------------------------------------------------------------------------

pub fn frame_chunks(msg_id: [u8; 8], ttl: u8, data: &[u8]) -> Result<Vec<Vec<u8>>, BleError> {
    let total = data.len().div_ceil(MAX_DATA_PER_FRAME) as u16;
    if total == 0 || total > MAX_TOTAL_CHUNKS {
        return Err(BleError(format!(
            "envelope too large: {} bytes",
            data.len()
        )));
    }
    let mut out = Vec::with_capacity(total as usize);
    for (i, chunk) in data.chunks(MAX_DATA_PER_FRAME).enumerate() {
        let mut f = Vec::with_capacity(FRAME_HEADER_LEN + chunk.len());
        f.push(BLE_MAGIC);
        f.extend_from_slice(&msg_id);
        f.push(ttl);
        f.push(i as u8);
        f.extend_from_slice(&total.to_be_bytes());
        f.extend_from_slice(chunk);
        out.push(f);
    }
    Ok(out)
}

pub struct FrameHeader {
    pub msg_id: [u8; 8],
    pub ttl: u8,
    pub chunk_idx: u8,
    pub total: u16,
}

pub fn parse_frame(frame: &[u8]) -> Result<(FrameHeader, &[u8]), BleError> {
    if frame.len() < FRAME_HEADER_LEN || frame[0] != BLE_MAGIC {
        return Err(BleError("bad frame".to_string()));
    }
    let mut msg_id = [0u8; 8];
    msg_id.copy_from_slice(&frame[1..9]);
    let ttl = frame[9];
    let chunk_idx = frame[10];
    let total = u16::from_be_bytes([frame[11], frame[12]]);
    if total == 0 || total > MAX_TOTAL_CHUNKS {
        return Err(BleError("bad total".to_string()));
    }
    Ok((
        FrameHeader {
            msg_id,
            ttl,
            chunk_idx,
            total,
        },
        &frame[FRAME_HEADER_LEN..],
    ))
}

// ---------------------------------------------------------------------------
// MeshOverlay:存储转发网状覆盖
// ---------------------------------------------------------------------------

enum RouterCmd {
    /// 发送分片到指定邻居(None = 广播到全部)。
    Send {
        to: Option<String>,
        frames: Vec<Vec<u8>>,
    },
}

struct Reassembly {
    total: u16,
    chunks: HashMap<u8, Vec<u8>>,
    ttl: u8,
    last: Instant,
}

/// 网状覆盖传输端:把任意 BleDriver 变成存储转发 mesh。
pub struct MeshOverlay<D: BleDriver> {
    driver: D,
    node_name: String,
    ttl: u8,
    outbound: broadcast::Sender<Inbound>,
    neighbors: Mutex<HashSet<String>>,
    dedup: Mutex<HashMap<[u8; 8], Instant>>,
    router_cmd: mpsc::Sender<RouterCmd>,
    new_conn_tx: mpsc::Sender<(String, mpsc::Sender<Vec<Vec<u8>>>)>,
    frame_tx: mpsc::Sender<(String, Vec<u8>)>,
}

impl<D: BleDriver> MeshOverlay<D> {
    pub fn spawn(driver: D, node_name: &str, ttl: u8) -> Arc<Self> {
        let (outbound_tx, _) = broadcast::channel(256);
        let (router_tx, router_rx) = mpsc::channel(64);
        let (new_conn_tx, new_conn_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = mpsc::channel(1024);

        let overlay = Arc::new(Self {
            driver,
            node_name: node_name.to_string(),
            ttl,
            outbound: outbound_tx,
            neighbors: Mutex::new(HashSet::new()),
            dedup: Mutex::new(HashMap::new()),
            router_cmd: router_tx,
            new_conn_tx: new_conn_tx.clone(),
            frame_tx: frame_tx.clone(),
        });

        // 路由核心:连接写通道表、重组、去重、转发、发送
        let o = Arc::clone(&overlay);
        tokio::spawn(async move {
            o.router(new_conn_rx, frame_rx, router_rx).await;
        });

        // 被连接方:接受入站连接
        let o = Arc::clone(&overlay);
        tokio::spawn(async move {
            o.accept_loop().await;
        });

        // 主动方:周期扫描并连接新邻居
        let o = Arc::clone(&overlay);
        tokio::spawn(async move {
            o.scan_loop().await;
        });

        overlay
    }

    /// 进入配对模式(开始广告)。
    pub async fn start_pairing(&self) -> Result<(), BleError> {
        self.driver.start_advertising(&self.node_name).await
    }

    pub async fn stop_pairing(&self) -> Result<(), BleError> {
        self.driver.stop_advertising().await
    }

    /// 主动连接指定地址的邻居。
    pub async fn connect_to(&self, addr: &BleAddr) -> Result<(), BleError> {
        let conn = self.driver.connect(addr).await?;
        self.spawn_conn_task(conn);
        Ok(())
    }

    /// 单连接任务:select! 交错读/写(读写互不阻塞)。
    /// 连接建立时向路由核心注册写通道。
    fn spawn_conn_task(&self, conn: D::Conn) {
        let new_conn_tx = self.new_conn_tx.clone();
        let frame_tx = self.frame_tx.clone();
        tokio::spawn(async move {
            let addr = conn.peer_addr().to_hex();
            let (write_tx, mut write_rx) = mpsc::channel(64);
            if new_conn_tx.send((addr.clone(), write_tx)).await.is_err() {
                return;
            }
            let mut conn = conn;
            loop {
                tokio::select! {
                    maybe = write_rx.recv() => {
                        match maybe {
                            Some(frames) => {
                                for f in frames {
                                    if conn.write(&f).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            None => return,
                        }
                    }
                    data = conn.read() => {
                        match data {
                            Ok(Some(f)) => {
                                if frame_tx.send((addr.clone(), f)).await.is_err() {
                                    return;
                                }
                            }
                            Ok(None) | Err(_) => return,
                        }
                    }
                }
            }
        });
    }

    async fn accept_loop(&self) {
        let Ok(mut rx) = self.driver.listen().await else {
            eprintln!("ble: listen unavailable");
            return;
        };
        loop {
            match rx.recv().await {
                Some(conn) => self.spawn_conn_task(conn),
                None => return,
            }
        }
    }

    async fn scan_loop(&self) {
        loop {
            tokio::time::sleep(SCAN_INTERVAL).await;
            let peers = match self.driver.scan(SCAN_INTERVAL).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("ble scan: {e}");
                    continue;
                }
            };
            let known = self.neighbors.lock().unwrap().clone();
            for p in peers {
                let key = p.addr.to_hex();
                if known.contains(&key) {
                    continue;
                }
                match self.driver.connect(&p.addr).await {
                    Ok(conn) => self.spawn_conn_task(conn),
                    Err(e) => eprintln!("ble connect {}: {e}", p.name),
                }
            }
        }
    }

    async fn router(
        self: &Arc<Self>,
        mut new_conns: mpsc::Receiver<(String, mpsc::Sender<Vec<Vec<u8>>>)>,
        mut frames: mpsc::Receiver<(String, Vec<u8>)>,
        mut cmds: mpsc::Receiver<RouterCmd>,
    ) {
        // 连接写通道表:addr → write channel(写由连接任务执行)
        let mut conns: HashMap<String, mpsc::Sender<Vec<Vec<u8>>>> = HashMap::new();
        let mut reassembly: HashMap<[u8; 8], Reassembly> = HashMap::new();

        loop {
            tokio::select! {
                maybe = new_conns.recv() => {
                    let Some((addr, write_tx)) = maybe else { break };
                    conns.insert(addr.clone(), write_tx);
                    self.neighbors.lock().unwrap().insert(addr);
                }
                maybe = frames.recv() => {
                    let Some((from, frame)) = maybe else { continue };
                    let Ok((hdr, data)) = parse_frame(&frame) else { continue };
                    // 清理超时的重组缓冲
                    reassembly.retain(|_, r| r.last.elapsed() < REASSEMBLY_TIMEOUT);
                    let entry = reassembly.entry(hdr.msg_id).or_insert_with(|| Reassembly {
                        total: hdr.total,
                        chunks: HashMap::new(),
                        ttl: hdr.ttl,
                        last: Instant::now(),
                    });
                    entry.last = Instant::now();
                    entry.chunks.insert(hdr.chunk_idx, data.to_vec());
                    if (entry.chunks.len() as u16) < entry.total {
                        continue;
                    }
                    let re = reassembly.remove(&hdr.msg_id).unwrap();
                    let mut payload = Vec::new();
                    let mut complete = true;
                    for i in 0..re.total {
                        match re.chunks.get(&(i as u8)) {
                            Some(c) => payload.extend_from_slice(c),
                            None => { complete = false; break; }
                        }
                    }
                    if !complete || payload.len() < 40 {
                        continue;
                    }
                    let Ok(env) = Envelope::decode(&payload) else { continue };
                    let msg_id: [u8; 8] = env.hash[..8].try_into().unwrap();
                    {
                        let mut seen = self.dedup.lock().unwrap();
                        if seen.len() > 8192 {
                            seen.retain(|_, t| t.elapsed() < DEDUP_TTL);
                        }
                        if seen.contains_key(&msg_id) {
                            continue;
                        }
                        seen.insert(msg_id, Instant::now());
                    }
                    let _ = self.outbound.send(Inbound { from: from.clone(), envelope: env });
                    // 存储转发(ttl-1,排除来源邻居)
                    if re.ttl > 0 {
                        if let Ok(out_frames) = frame_chunks(msg_id, re.ttl - 1, &payload) {
                            let others: Vec<_> = conns
                                .iter()
                                .filter(|(a, _)| *a != &from)
                                .map(|(a, c)| (a.clone(), c.clone()))
                                .collect();
                            for (_, c) in others {
                                let _ = c.send(out_frames.clone()).await;
                            }
                        }
                    }
                }
                maybe = cmds.recv() => {
                    let Some(RouterCmd::Send { to, frames }) = maybe else { break };
                    let targets: Vec<_> = match &to {
                        Some(t) => conns.get(t).map(|c| vec![(t.clone(), c.clone())]).unwrap_or_default(),
                        None => conns.iter().map(|(a, c)| (a.clone(), c.clone())).collect(),
                    };
                    for (_, c) in targets {
                        let _ = c.send(frames.clone()).await;
                    }
                }
            }
        }
    }
}

impl<D: BleDriver> Transport for MeshOverlay<D> {
    fn name(&self) -> &'static str {
        "ble"
    }

    fn availability(&self) -> Availability {
        Availability::Up
    }

    fn peers(&self) -> Vec<String> {
        self.neighbors.lock().unwrap().iter().cloned().collect()
    }

    fn send(
        &self,
        to: &str,
        envelope: Envelope,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send>>
    {
        let msg_id: [u8; 8] = envelope.hash[..8].try_into().unwrap();
        let payload = envelope.encode();
        let frames = match frame_chunks(msg_id, self.ttl, &payload) {
            Ok(f) => f,
            Err(e) => {
                return Box::pin(async move { Err(TransportError::Io(e.to_string())) });
            }
        };
        let to = if to.is_empty() || to == "*" {
            None
        } else {
            Some(to.to_string())
        };
        let cmd = self.router_cmd.clone();
        Box::pin(async move {
            cmd.send(RouterCmd::Send { to, frames })
                .await
                .map_err(|_| TransportError::Io("ble router stopped".to_string()))
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<Inbound> {
        self.outbound.subscribe()
    }
}

// ---------------------------------------------------------------------------
// Mock 驱动(内存)
// ---------------------------------------------------------------------------

pub struct MockHub {
    nodes: Mutex<HashMap<String, Arc<MockDriver>>>,
}

impl MockHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            nodes: Mutex::new(HashMap::new()),
        })
    }

    pub fn register(self: &Arc<Self>, addr: &str) -> Arc<MockDriver> {
        let d = Arc::new(MockDriver {
            hub: Arc::clone(self),
            addr: addr.to_string(),
            name: Mutex::new(String::new()),
            advertising: AtomicBool::new(false),
            incoming: Mutex::new(None),
            pending_incoming: Mutex::new(Vec::new()),
        });
        self.nodes
            .lock()
            .unwrap()
            .insert(addr.to_string(), Arc::clone(&d));
        d
    }
}

pub struct MockDriver {
    hub: Arc<MockHub>,
    addr: String,
    name: Mutex<String>,
    advertising: AtomicBool,
    incoming: Mutex<Option<mpsc::Sender<MockConn>>>,
    /// listen() 尚未调用时到达的入站连接排队于此(消除注册竞态)。
    pending_incoming: Mutex<Vec<MockConn>>,
}

pub struct MockConn {
    peer: String,
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl BleConn for MockConn {
    fn peer_addr(&self) -> BleAddr {
        BleAddr(self.peer.as_bytes().to_vec())
    }

    async fn write(&self, frame: &[u8]) -> Result<(), BleError> {
        self.tx
            .send(frame.to_vec())
            .await
            .map_err(|_| BleError("mock peer gone".to_string()))
    }

    async fn read(&mut self) -> Result<Option<Vec<u8>>, BleError> {
        Ok(self.rx.recv().await)
    }
}

impl BleDriver for MockDriver {
    type Conn = MockConn;

    fn driver_name(&self) -> &'static str {
        "mock"
    }

    async fn start_advertising(&self, name: &str) -> Result<(), BleError> {
        *self.name.lock().unwrap() = name.to_string();
        self.advertising.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stop_advertising(&self) -> Result<(), BleError> {
        self.advertising.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn scan(&self, _timeout: Duration) -> Result<Vec<BlePeer>, BleError> {
        let nodes = self.hub.nodes.lock().unwrap();
        let mut out = Vec::new();
        for (addr, d) in nodes.iter() {
            if *addr == self.addr || !d.advertising.load(Ordering::SeqCst) {
                continue;
            }
            out.push(BlePeer {
                addr: BleAddr(addr.as_bytes().to_vec()),
                name: d.name.lock().unwrap().clone(),
                zoe: false,
            });
        }
        Ok(out)
    }

    async fn connect(&self, addr: &BleAddr) -> Result<Self::Conn, BleError> {
        let target =
            String::from_utf8(addr.0.clone()).map_err(|_| BleError("bad mock addr".to_string()))?;
        let (a_tx, b_rx) = mpsc::channel(128);
        let (b_tx, a_rx) = mpsc::channel(128);
        // 对端入站:已监听则直投,否则入队(锁内 try_send,避免跨 await 持锁)
        {
            let nodes = self.hub.nodes.lock().unwrap();
            let other = nodes
                .get(&target)
                .ok_or_else(|| BleError(format!("no such node {target}")))?;
            let conn = MockConn {
                peer: self.addr.clone(),
                tx: b_tx,
                rx: b_rx,
            };
            let incoming = other.incoming.lock().unwrap();
            match incoming.as_ref() {
                Some(tx) => {
                    let _ = tx.try_send(conn);
                }
                None => other.pending_incoming.lock().unwrap().push(conn),
            }
        }
        Ok(MockConn {
            peer: target,
            tx: a_tx,
            rx: a_rx,
        })
    }

    async fn listen(&self) -> Result<mpsc::Receiver<Self::Conn>, BleError> {
        let (tx, rx) = mpsc::channel(32);
        let pending = std::mem::take(&mut *self.pending_incoming.lock().unwrap());
        for c in pending {
            let _ = tx.try_send(c);
        }
        *self.incoming.lock().unwrap() = Some(tx);
        Ok(rx)
    }
}

impl BleDriver for Arc<MockDriver> {
    type Conn = MockConn;

    fn driver_name(&self) -> &'static str {
        "mock"
    }

    async fn start_advertising(&self, name: &str) -> Result<(), BleError> {
        self.as_ref().start_advertising(name).await
    }

    async fn stop_advertising(&self) -> Result<(), BleError> {
        self.as_ref().stop_advertising().await
    }

    async fn scan(&self, timeout: Duration) -> Result<Vec<BlePeer>, BleError> {
        self.as_ref().scan(timeout).await
    }

    async fn connect(&self, addr: &BleAddr) -> Result<Self::Conn, BleError> {
        self.as_ref().connect(addr).await
    }

    async fn listen(&self) -> Result<mpsc::Receiver<Self::Conn>, BleError> {
        self.as_ref().listen().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(seq: u64) -> Envelope {
        Envelope::new(0, 1, b"g1".to_vec(), 0, 0, seq, vec![0x42; 64])
    }

    /// Mock 地址 = 节点名字符串字节,hex 表示。
    fn hex_of(name: &str) -> String {
        hex::encode(name.as_bytes())
    }

    #[tokio::test]
    async fn two_nodes_direct_delivery() {
        let hub = MockHub::new();
        let a_driver = hub.register("A");
        let b_driver = hub.register("B");

        let a = MeshOverlay::spawn(a_driver, "node-a", 2);
        let b = MeshOverlay::spawn(b_driver, "node-b", 2);
        let mut b_rx = b.subscribe();

        a.start_pairing().await.unwrap();
        // B 扫描发现 A 并连接
        let peers = b.driver.scan(Duration::from_millis(100)).await.unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "node-a");
        b.connect_to(&peers[0].addr).await.unwrap();

        // 等待邻居表建立
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !b.peers().contains(&hex_of("A")) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(b.peers().contains(&hex_of("A")), "B 未发现 A 邻居");

        let env = env_with(1);
        a.send("*", env.clone()).await.unwrap();
        let inbound = tokio::time::timeout(Duration::from_secs(5), b_rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");
        assert_eq!(inbound.envelope, env);
        assert_eq!(inbound.from, hex_of("A"));
    }

    #[tokio::test]
    async fn three_nodes_store_and_forward() {
        let hub = MockHub::new();
        let a_driver = hub.register("A");
        let b_driver = hub.register("B");
        let c_driver = hub.register("C");

        let a = MeshOverlay::spawn(a_driver, "node-a", 2);
        let b = MeshOverlay::spawn(b_driver, "node-b", 2);
        let c = MeshOverlay::spawn(c_driver, "node-c", 2);
        let mut b_rx = b.subscribe();
        let mut c_rx = c.subscribe();

        // 拓扑:A—B—C(B 同时连接 A 与 C)
        a.start_pairing().await.unwrap();
        b.start_pairing().await.unwrap();
        let pa = b.driver.scan(Duration::from_millis(100)).await.unwrap();
        b.connect_to(&pa[0].addr).await.unwrap();
        // C 连接 B(直接按地址)
        c.connect_to(&BleAddr::from_hex(&hex_of("B")).unwrap())
            .await
            .unwrap();

        // 等 B 有 2 个邻居
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while b.peers().len() < 2 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(b.peers().len(), 2, "B 应有 2 个邻居: {:?}", b.peers());

        // A 发 → B 收 → B 转发 → C 收(2 跳)
        let env = env_with(7);
        a.send("*", env.clone()).await.unwrap();
        let got_b = tokio::time::timeout(Duration::from_secs(5), b_rx.recv())
            .await
            .expect("B timeout");
        assert_eq!(got_b.unwrap().envelope, env);
        let got_c = tokio::time::timeout(Duration::from_secs(5), c_rx.recv())
            .await
            .expect("C timeout");
        assert_eq!(got_c.unwrap().envelope, env);
    }

    #[tokio::test]
    async fn dedup_drops_duplicate_envelope() {
        let hub = MockHub::new();
        let a = MeshOverlay::spawn(hub.register("A"), "node-a", 1);
        let b = MeshOverlay::spawn(hub.register("B"), "node-b", 1);
        let mut b_rx = b.subscribe();

        a.start_pairing().await.unwrap();
        let peers = b.driver.scan(Duration::from_millis(100)).await.unwrap();
        b.connect_to(&peers[0].addr).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !b.peers().contains(&hex_of("A")) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            b.peers().contains(&hex_of("A")),
            "B 未发现 A 邻居: {:?}",
            b.peers()
        );

        let env = env_with(9);
        a.send("*", env.clone()).await.unwrap();
        a.send("*", env.clone()).await.unwrap(); // 同一条,应被去重

        let first = tokio::time::timeout(Duration::from_secs(5), b_rx.recv())
            .await
            .expect("first timeout");
        assert_eq!(first.unwrap().envelope, env);
        // 第二条不应到达(超时)
        assert!(
            tokio::time::timeout(Duration::from_millis(500), b_rx.recv())
                .await
                .is_err(),
            "重复信封未被去重"
        );
    }

    #[test]
    fn frame_roundtrip() {
        let data: Vec<u8> = (0..1500).map(|i| (i % 251) as u8).collect();
        let frames = frame_chunks([7; 8], 3, &data).unwrap();
        assert!(frames.len() > 1);
        let mut collected = HashMap::new();
        for f in &frames {
            let (hdr, d) = parse_frame(f).unwrap();
            assert_eq!(hdr.msg_id, [7; 8]);
            assert_eq!(hdr.ttl, 3);
            collected.insert(hdr.chunk_idx, d.to_vec());
        }
        let mut out = Vec::new();
        for i in 0..collected.len() as u8 {
            out.extend_from_slice(&collected[&i]);
        }
        assert_eq!(out, data);
    }

    #[test]
    fn frame_rejects_garbage() {
        assert!(parse_frame(&[0x00; 20]).is_err());
        assert!(parse_frame(&[BLE_MAGIC, 1, 2, 3]).is_err());
        assert!(parse_frame(&[]).is_err());
    }
}
