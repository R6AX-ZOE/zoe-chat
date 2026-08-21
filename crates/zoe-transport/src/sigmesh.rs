//! SIG Bluetooth Mesh 适配(docs/DESIGN.md §6.3、docs/envelope.md §2.2)。
//!
//! 分层:
//! - 本模块 = **应用层**:帧编解码(6B 头 + ≤5B 数据,单包 ≤11B)、分片
//!   (5B/片,total ≤255)、重组(msg_id)、去重(4B 前缀,24h)、尽力重传;
//! - 泛洪网络 = **网络层**(真机 = SIG Mesh 栈):TTL 限跳、网络层去重、多跳
//!   转发。经 `SigMeshNet` trait 抽象,测试用 `FloodHub` mock(拓扑 + TTL + 去重)。
//!
//! 载荷上限:total ≤255 × 5B = 1275B。更大载荷(如大群组 Welcome)应回退
//! GATT 覆盖网投递 —— 本适配不做跨包分片(SIG Mesh 规范无此语义)。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};
use zoe_core::envelope::Envelope;

use crate::{Availability, Inbound, Transport, TransportError};

/// 单包应用载荷上限(SIG Mesh 规范硬约束)。
pub const SM_FRAME_MAX_LEN: usize = 11;
/// 帧头:msg_id(4)+chunk_idx(1)+total(1)。
pub const SM_HEADER_LEN: usize = 6;
/// 每片数据字节数。
pub const SM_CHUNK_DATA: usize = 5;
/// 总片数上限(total 为 u8)。
pub const SM_MAX_TOTAL: u8 = 255;
/// 载荷上限 = total × 每片。
pub const SM_MAX_PAYLOAD: usize = SM_MAX_TOTAL as usize * SM_CHUNK_DATA;
/// 去重集保留时长(与应用层一致的 24h)。
pub const SM_DEDUP_TTL: Duration = Duration::from_secs(24 * 3600);
/// 重组超时(超时后丢弃残片,防内存泄漏)。
pub const SM_REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(60);
/// 网络层默认 TTL(泛洪限跳)。
pub const FLOOD_DEFAULT_TTL: u8 = 3;

#[derive(Debug, Clone)]
pub struct SigMeshError(pub String);

impl std::fmt::Display for SigMeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sigmesh: {}", self.0)
    }
}

impl std::error::Error for SigMeshError {}

// ---------------------------------------------------------------------------
// 帧格式(docs/envelope.md §2.2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigMeshFrame {
    /// Envelope hash 前 4 字节(去重键 + 重组依据)。
    pub msg_id: [u8; 4],
    pub chunk_idx: u8,
    pub total: u8,
    pub data: Vec<u8>,
}

pub fn encode_frame(frame: &SigMeshFrame) -> Vec<u8> {
    let mut out = Vec::with_capacity(SM_HEADER_LEN + frame.data.len());
    out.extend_from_slice(&frame.msg_id);
    out.push(frame.chunk_idx);
    out.push(frame.total);
    out.extend_from_slice(&frame.data);
    out
}

pub fn decode_frame(pdu: &[u8]) -> Result<SigMeshFrame, SigMeshError> {
    if pdu.len() < SM_HEADER_LEN || pdu.len() > SM_FRAME_MAX_LEN {
        return Err(SigMeshError("bad frame length".to_string()));
    }
    let mut msg_id = [0u8; 4];
    msg_id.copy_from_slice(&pdu[0..4]);
    let chunk_idx = pdu[4];
    let total = pdu[5];
    if total == 0 {
        return Err(SigMeshError("bad total".to_string()));
    }
    if chunk_idx >= total {
        return Err(SigMeshError("chunk_idx out of range".to_string()));
    }
    Ok(SigMeshFrame {
        msg_id,
        chunk_idx,
        total,
        data: pdu[SM_HEADER_LEN..].to_vec(),
    })
}

/// 把载荷切分为 SIG Mesh 帧(每帧 ≤11B,data ≤5B)。
pub fn sigmesh_chunks(msg_id: [u8; 4], data: &[u8]) -> Result<Vec<Vec<u8>>, SigMeshError> {
    let total_usize = data.len().div_ceil(SM_CHUNK_DATA);
    if total_usize == 0 || total_usize > SM_MAX_TOTAL as usize {
        return Err(SigMeshError(format!(
            "payload too large for SIG Mesh (max {SM_MAX_PAYLOAD}B): {}B",
            data.len()
        )));
    }
    let total = total_usize as u8;
    Ok(data
        .chunks(SM_CHUNK_DATA)
        .enumerate()
        .map(|(i, chunk)| {
            encode_frame(&SigMeshFrame {
                msg_id,
                chunk_idx: i as u8,
                total,
                data: chunk.to_vec(),
            })
        })
        .collect())
}

// ---------------------------------------------------------------------------
// 网络层抽象(SigMeshNet):真机 = SIG Mesh 栈,测试 = FloodHub mock
// ---------------------------------------------------------------------------

/// 泛洪网络(网络层)接口。本模块只把完整 PDU 交给它,由它负责
/// TTL 限跳、网络层去重与多跳转发。
/// 洪泛网格(洪泛转发)抽象:真实介质 = SIG Mesh 网络(Android 经 BLE GATT 桥),
/// 测试/缺介质时 = FloodHub mock。
/// 注:方法签名保证对象安全(BoxFuture),daemon 才能以 `Arc<dyn SigMeshNet>` 持有。
pub trait SigMeshNet: Send + Sync + 'static {
    fn node_id(&self) -> String;
    /// 当前可达邻居(洪泛下一跳,非对端必达路径)。
    fn neighbors(&self) -> Vec<String>;
    fn broadcast(
        &self,
        pdu: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SigMeshError>> + Send>>;
    fn subscribe(&self) -> broadcast::Receiver<Vec<u8>>;
}

impl<T: SigMeshNet + ?Sized> SigMeshNet for Arc<T> {
    fn node_id(&self) -> String {
        self.as_ref().node_id()
    }

    fn neighbors(&self) -> Vec<String> {
        self.as_ref().neighbors()
    }

    fn broadcast(
        &self,
        pdu: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SigMeshError>> + Send>> {
        self.as_ref().broadcast(pdu)
    }

    fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.as_ref().subscribe()
    }
}

// ---------------------------------------------------------------------------
// SigMeshStack:分片/重组/去重/重传,统一暴露 Transport trait
// ---------------------------------------------------------------------------

struct Reassembly {
    total: u8,
    chunks: HashMap<u8, Vec<u8>>,
    last: Instant,
}

/// SIG Mesh 传输端:把任意 SigMeshNet 变成 zoe Transport。
pub struct SigMeshStack<N: SigMeshNet> {
    net: N,
    outbound: broadcast::Sender<Inbound>,
    dedup: Mutex<HashMap<[u8; 4], Instant>>,
    pdu_tx: mpsc::Sender<Vec<u8>>,
    retries: u8,
    retry_interval: Duration,
}

impl<N: SigMeshNet> SigMeshStack<N> {
    /// `retries` = 每轮全量重发次数(0 = 不重发),`retry_interval` = 轮间间隔。
    pub fn spawn(net: N, retries: u8, retry_interval: Duration) -> Arc<Self> {
        let (outbound_tx, _) = broadcast::channel(256);
        let (pdu_tx, pdu_rx) = mpsc::channel(1024);
        let stack = Arc::new(Self {
            net,
            outbound: outbound_tx,
            dedup: Mutex::new(HashMap::new()),
            pdu_tx,
            retries,
            retry_interval,
        });

        let s = Arc::clone(&stack);
        tokio::spawn(async move {
            s.reassembly_loop(pdu_rx).await;
        });

        // 入站 PDU 订阅 → 重组核心
        let s = Arc::clone(&stack);
        let mut rx = s.net.subscribe();
        tokio::spawn(async move {
            while let Ok(pdu) = rx.recv().await {
                if s.pdu_tx.send(pdu).await.is_err() {
                    break;
                }
            }
        });

        stack
    }

    async fn reassembly_loop(self: &Arc<Self>, mut pdus: mpsc::Receiver<Vec<u8>>) {
        let mut reassembly: HashMap<[u8; 4], Reassembly> = HashMap::new();
        while let Some(pdu) = pdus.recv().await {
            let Ok(frame) = decode_frame(&pdu) else {
                continue;
            };
            // 清理超时残片
            reassembly.retain(|_, r| r.last.elapsed() < SM_REASSEMBLY_TIMEOUT);
            // 已完成的 msg_id 直接丢弃(应用层去重,24h)
            {
                let mut seen = self.dedup.lock().unwrap();
                if seen.len() > 8192 {
                    seen.retain(|_, t| t.elapsed() < SM_DEDUP_TTL);
                }
                if seen.contains_key(&frame.msg_id) {
                    continue;
                }
            }
            let entry = reassembly
                .entry(frame.msg_id)
                .or_insert_with(|| Reassembly {
                    total: frame.total,
                    chunks: HashMap::new(),
                    last: Instant::now(),
                });
            entry.last = Instant::now();
            entry.chunks.insert(frame.chunk_idx, frame.data);
            if (entry.chunks.len() as u8) < entry.total {
                continue;
            }
            let re = reassembly.remove(&frame.msg_id).unwrap();
            let mut payload = Vec::new();
            for i in 0..re.total {
                match re.chunks.get(&i) {
                    Some(c) => payload.extend_from_slice(c),
                    None => {
                        payload.clear();
                        break;
                    }
                }
            }
            let Ok(env) = Envelope::decode(&payload) else {
                continue;
            };
            self.dedup
                .lock()
                .unwrap()
                .insert(frame.msg_id, Instant::now());
            let _ = self.outbound.send(Inbound {
                from: self.net.node_id(),
                envelope: env,
            });
        }
    }
}

impl<N: SigMeshNet> SigMeshStack<N> {
    async fn send_impl(&self, to: &str, envelope: Envelope) -> Result<(), TransportError> {
        if !to.is_empty() && to != "*" && !self.net.neighbors().iter().any(|n| n == to) {
            return Err(TransportError::UnknownPeer(to.to_string()));
        }
        let payload = envelope.encode();
        let msg_id: [u8; 4] = envelope.hash[..4].try_into().unwrap();
        let frames =
            sigmesh_chunks(msg_id, &payload).map_err(|e| TransportError::Io(e.to_string()))?;
        // 尽力重传:每轮广播全部分片,轮间退避
        for round in 0..=self.retries {
            for f in &frames {
                self.net
                    .broadcast(f.clone())
                    .await
                    .map_err(|e| TransportError::Io(e.to_string()))?;
            }
            if round < self.retries {
                tokio::time::sleep(self.retry_interval).await;
            }
        }
        Ok(())
    }
}

impl<N: SigMeshNet> Transport for SigMeshStack<N> {
    fn name(&self) -> &'static str {
        "sigmesh"
    }

    fn availability(&self) -> Availability {
        Availability::Up
    }

    fn peers(&self) -> Vec<String> {
        self.net.neighbors()
    }

    fn send(
        &self,
        to: &str,
        envelope: Envelope,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>>
    {
        let to = to.to_string();
        Box::pin(async move { self.send_impl(&to, envelope).await })
    }

    fn subscribe(&self) -> broadcast::Receiver<Inbound> {
        self.outbound.subscribe()
    }
}

// ---------------------------------------------------------------------------
// FloodHub mock:内存泛洪网络(网络层 TTL + 去重 + 多跳转发)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FloodPdu {
    from: String,
    pdu: Vec<u8>,
    ttl: u8,
}

/// 节点注册表 + 链路拓扑(无向)。链路只表示"一跳可达"。
pub struct FloodHub {
    nodes: Mutex<HashMap<String, mpsc::Sender<FloodPdu>>>,
    links: Mutex<HashMap<String, HashSet<String>>>,
}

impl FloodHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            nodes: Mutex::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
        })
    }

    /// 注册节点,返回网络接口(SigMeshNet 实现)。
    pub fn register(self: &Arc<Self>, id: &str) -> FloodNode {
        let (tx, rx) = mpsc::channel(1024);
        let node = FloodNode::new(Arc::clone(self), id.to_string(), rx);
        self.nodes.lock().unwrap().insert(id.to_string(), tx);
        node
    }

    /// 添加一跳链路(无向)。
    pub fn link(&self, a: &str, b: &str) {
        let mut links = self.links.lock().unwrap();
        links
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
        links
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string());
    }

    /// 转发:node 的邻居(排除 sender)各投一份(ttl-1)。
    fn route(&self, node: &str, sender: &str, pdu: Vec<u8>, ttl: u8) {
        if ttl == 0 {
            return;
        }
        let links = self.links.lock().unwrap();
        let nodes = self.nodes.lock().unwrap();
        let Some(neighbors) = links.get(node) else {
            return;
        };
        for n in neighbors {
            if n == sender {
                continue;
            }
            if let Some(q) = nodes.get(n) {
                let _ = q.try_send(FloodPdu {
                    from: node.to_string(),
                    pdu: pdu.clone(),
                    ttl,
                });
            }
        }
    }
}

/// 泛洪网络节点(网络层):接收 → 去重 → 上抛应用 → 转发邻居(ttl-1)。
/// Clone 共享同一核心(返回句柄与后台任务见同一去重集/TTL)。
#[derive(Clone)]
pub struct FloodNode {
    core: Arc<FloodCore>,
}

struct FloodCore {
    hub: Arc<FloodHub>,
    id: String,
    ttl: Mutex<u8>,
    seen: Mutex<HashSet<Vec<u8>>>,
    out: broadcast::Sender<Vec<u8>>,
}

impl FloodNode {
    fn new(hub: Arc<FloodHub>, id: String, mut rx: mpsc::Receiver<FloodPdu>) -> Self {
        let (out_tx, _) = broadcast::channel(256);
        let core = Arc::new(FloodCore {
            hub,
            id,
            ttl: Mutex::new(FLOOD_DEFAULT_TTL),
            seen: Mutex::new(HashSet::new()),
            out: out_tx,
        });
        let task = Arc::clone(&core);
        tokio::spawn(async move {
            while let Some(pkt) = rx.recv().await {
                handle(&task, pkt);
            }
        });
        Self { core }
    }
}

fn handle(core: &Arc<FloodCore>, pkt: FloodPdu) {
    // 网络层去重:整包字节已见即丢弃
    {
        let mut seen = core.seen.lock().unwrap();
        if seen.len() > 65536 {
            seen.clear();
        }
        if !seen.insert(pkt.pdu.clone()) {
            return;
        }
    }
    // 上抛应用层
    let _ = core.out.send(pkt.pdu.clone());
    // 转发邻居(ttl-1,排除来源)
    let ttl = pkt.ttl.saturating_sub(1);
    if ttl > 0 {
        core.hub.route(&core.id, &pkt.from, pkt.pdu, ttl);
    }
}

impl FloodNode {
    /// 设置本节点广播 TTL(mock 专用,模拟网络层配置)。
    pub fn set_ttl(&self, ttl: u8) {
        *self.core.ttl.lock().unwrap() = ttl;
    }
}

impl SigMeshNet for FloodNode {
    fn node_id(&self) -> String {
        self.core.id.clone()
    }

    fn neighbors(&self) -> Vec<String> {
        self.core
            .hub
            .links
            .lock()
            .unwrap()
            .get(&self.core.id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    fn broadcast(
        &self,
        pdu: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SigMeshError>> + Send>> {
        let this = self.clone();
        Box::pin(async move {
            let ttl = *this.core.ttl.lock().unwrap();
            this.core.hub.route(&this.core.id, &this.core.id, pdu, ttl);
            Ok(())
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.core.out.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(seq: u64, payload_len: usize) -> Envelope {
        Envelope::new(0, 1, b"g1".to_vec(), 0, 0, seq, vec![0x42; payload_len])
    }

    fn stack_pair(
        hub: &Arc<FloodHub>,
        a: &str,
        b: &str,
    ) -> (Arc<SigMeshStack<FloodNode>>, Arc<SigMeshStack<FloodNode>>) {
        let na = hub.register(a);
        let nb = hub.register(b);
        hub.link(a, b);
        let sa = SigMeshStack::spawn(na, 0, Duration::from_millis(0));
        let sb = SigMeshStack::spawn(nb, 0, Duration::from_millis(0));
        (sa, sb)
    }

    #[test]
    fn frame_roundtrip() {
        let f = SigMeshFrame {
            msg_id: [1, 2, 3, 4],
            chunk_idx: 0,
            total: 3,
            data: vec![0xAA; 5],
        };
        let pdu = encode_frame(&f);
        assert_eq!(pdu.len(), SM_FRAME_MAX_LEN);
        assert_eq!(decode_frame(&pdu).unwrap(), f);
    }

    #[test]
    fn frame_rejects_garbage() {
        assert!(decode_frame(&[]).is_err());
        assert!(decode_frame(&[0; 5]).is_err()); // 缺头
        assert!(decode_frame(&[0; 12]).is_err()); // 超长
        let mut pdu = vec![0u8; SM_HEADER_LEN];
        pdu[5] = 0; // total=0
        assert!(decode_frame(&pdu).is_err());
        let mut pdu = vec![0u8; SM_HEADER_LEN + 1];
        pdu[5] = 2;
        pdu[4] = 2; // chunk_idx=total
        assert!(decode_frame(&pdu).is_err());
    }

    #[test]
    fn chunks_roundtrip() {
        let data: Vec<u8> = (0..33).map(|i| i as u8).collect();
        let frames = sigmesh_chunks([9; 4], &data).unwrap();
        assert_eq!(frames.len(), 7); // 5B/片 → 7 片
        let mut collected = HashMap::new();
        for pdu in &frames {
            let f = decode_frame(pdu).unwrap();
            assert_eq!(f.msg_id, [9; 4]);
            collected.insert(f.chunk_idx, f.data);
        }
        let mut out = Vec::new();
        for i in 0..collected.len() as u8 {
            out.extend_from_slice(&collected[&i]);
        }
        assert_eq!(out, data);
    }

    #[test]
    fn chunk_size_limits() {
        assert!(sigmesh_chunks([0; 4], &[]).is_err()); // 空
        assert!(sigmesh_chunks([0; 4], &vec![0u8; SM_MAX_PAYLOAD]).is_ok());
        assert!(sigmesh_chunks([0; 4], &vec![0u8; SM_MAX_PAYLOAD + 1]).is_err());
    }

    #[tokio::test]
    async fn two_nodes_direct_delivery() {
        let hub = FloodHub::new();
        let (a, b) = stack_pair(&hub, "A", "B");
        let mut b_rx = b.subscribe();

        let env = env_with(1, 64);
        a.send("*", env.clone()).await.unwrap();
        let inbound = tokio::time::timeout(Duration::from_secs(5), b_rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");
        assert_eq!(inbound.envelope, env);
    }

    #[tokio::test]
    async fn three_nodes_flood_forward() {
        let hub = FloodHub::new();
        let na = hub.register("A");
        let nb = hub.register("B");
        let nc = hub.register("C");
        hub.link("A", "B");
        hub.link("B", "C");
        let a = SigMeshStack::spawn(na, 0, Duration::from_millis(0));
        let b = SigMeshStack::spawn(nb, 0, Duration::from_millis(0));
        let c = SigMeshStack::spawn(nc, 0, Duration::from_millis(0));
        let mut b_rx = b.subscribe();
        let mut c_rx = c.subscribe();

        // A 发送,B 与 C(经 B 转发)都应收到
        let env = env_with(7, 64);
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
    async fn dedup_drops_duplicate() {
        let hub = FloodHub::new();
        let (a, b) = stack_pair(&hub, "A", "B");
        let mut b_rx = b.subscribe();

        let env = env_with(9, 64);
        a.send("*", env.clone()).await.unwrap();
        a.send("*", env.clone()).await.unwrap(); // 重复:应用层去重

        let first = tokio::time::timeout(Duration::from_secs(5), b_rx.recv())
            .await
            .expect("first timeout");
        assert_eq!(first.unwrap().envelope, env);
        assert!(
            tokio::time::timeout(Duration::from_millis(400), b_rx.recv())
                .await
                .is_err(),
            "重复信封未被去重"
        );
    }

    #[tokio::test]
    async fn ttl_limits_flooding() {
        let hub = FloodHub::new();
        let na = hub.register("A");
        let nb = hub.register("B");
        let nc = hub.register("C");
        hub.link("A", "B");
        hub.link("B", "C");
        na.set_ttl(1); // A 只广播一跳
        let a = SigMeshStack::spawn(na, 0, Duration::from_millis(0));
        let b = SigMeshStack::spawn(nb, 0, Duration::from_millis(0));
        let c = SigMeshStack::spawn(nc, 0, Duration::from_millis(0));
        let mut b_rx = b.subscribe();
        let mut c_rx = c.subscribe();

        let env = env_with(5, 64);
        a.send("*", env.clone()).await.unwrap();
        let got_b = tokio::time::timeout(Duration::from_secs(5), b_rx.recv())
            .await
            .expect("B timeout");
        assert_eq!(got_b.unwrap().envelope, env);
        assert!(
            tokio::time::timeout(Duration::from_millis(400), c_rx.recv())
                .await
                .is_err(),
            "TTL=1 不应到达 C(2 跳)"
        );
    }

    #[tokio::test]
    async fn oversized_payload_rejected() {
        let hub = FloodHub::new();
        let (a, _b) = stack_pair(&hub, "A", "B");
        let env = env_with(1, SM_MAX_PAYLOAD + 1);
        assert!(a.send("*", env).await.is_err());
    }

    #[tokio::test]
    async fn unknown_peer_rejected() {
        let hub = FloodHub::new();
        let (a, _b) = stack_pair(&hub, "A", "B");
        let env = env_with(1, 64);
        assert!(matches!(
            a.send("C", env).await,
            Err(TransportError::UnknownPeer(_))
        ));
    }
}
