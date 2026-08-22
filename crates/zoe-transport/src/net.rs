//! 远程传输:libp2p(mDNS 局域网发现 + DCUtR 打洞 + relay 客户端 + 手动拨号)。
//!
//! 设计(docs/DESIGN.md §6.4):
//! - 传输层安全:noise 握手使用用户 Ed25519 身份密钥 —— PeerId 派生自身份公钥,
//!   与 QR 名片互认的是同一把钥匙(无传输层 TOFU);
//! - 消息面:request-response 协议 `/zoe/envelope/1`(cbor 编解码),
//!   请求 = Envelope,响应 = 空 ack;子流由 yamux 管理;
//! - 发现:手动拨号(必选)+ mDNS(局域网自动);DCUtR 打洞在存在 relay 地址时生效。
//!
//! 需要 feature `net`(默认开启)。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p::{
    dcutr, identify, identity::Keypair, mdns, multiaddr::Protocol, Multiaddr, PeerId,
    StreamProtocol, SwarmBuilder,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use zoe_core::envelope::Envelope;

use crate::{Availability, Inbound, Transport, TransportError};

const PROTOCOL: &str = "/zoe/envelope/1";

/// request-response 请求上限:信封载荷 16 MiB 上限 + 余量。
const MAX_REQUEST_SIZE: u64 = 16 * 1024 * 1024 + 1024;

/// 每 peer 待发送队列上限(防内存膨胀)。
const MAX_PENDING_PER_PEER: usize = 256;

#[derive(NetworkBehaviour)]
struct Behaviour {
    request_response: request_response::cbor::Behaviour<Envelope, Vec<u8>>,
    mdns: mdns::tokio::Behaviour,
    dcutr: dcutr::Behaviour,
    identify: identify::Behaviour,
}

impl Behaviour {
    fn new(key: &Keypair) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // cbor codec 默认请求上限 1 MiB(read 截断即解码失败);
        // 文件消息(≤8 MiB)需要更大的请求上限。响应仅空 ack。
        let codec = request_response::cbor::codec::Codec::<Envelope, Vec<u8>>::default()
            .set_request_size_maximum(MAX_REQUEST_SIZE)
            .set_response_size_maximum(64 * 1024);
        Ok(Self {
            request_response: request_response::Behaviour::with_codec(
                codec,
                [(StreamProtocol::new(PROTOCOL), ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(Duration::from_secs(60)),
            ),
            mdns: mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?,
            dcutr: dcutr::Behaviour::new(key.public().to_peer_id()),
            identify: identify::Behaviour::new(identify::Config::new(
                "zoe-chat/0.1".to_string(),
                key.public(),
            )),
        })
    }
}

enum Command {
    Dial {
        addr: Multiaddr,
        ack: oneshot::Sender<Result<(), String>>,
    },
    Send {
        peer: PeerId,
        envelope: Envelope,
    },
}

struct NetInner {
    peer_id: PeerId,
    inbound: broadcast::Sender<Inbound>,
    commands: mpsc::Sender<Command>,
    connected: Mutex<Vec<String>>,
    listen_addrs: Mutex<Vec<String>>,
}

/// libp2p 远程传输端(跨线程共享的句柄)。
pub struct NetTransport {
    inner: Arc<NetInner>,
}

impl NetTransport {
    /// 从 32 字节种子构造 libp2p 身份密钥并启动传输(后台 swarm 任务)。
    ///
    /// 种子通常派生自用户身份密钥 —— 传输层认证与 QR 名片互认的是同一把钥匙。
    pub fn spawn_from_seed(seed: &[u8; 32]) -> Option<Arc<Self>> {
        let keypair = Keypair::ed25519_from_bytes(*seed).ok()?;
        Some(Self::spawn(keypair))
    }

    /// 用给定的 libp2p 身份密钥启动传输(后台 swarm 任务)。
    pub fn spawn(keypair: Keypair) -> Arc<Self> {
        let peer_id = keypair.public().to_peer_id();
        let (inbound_tx, _) = broadcast::channel(512);
        let (command_tx, command_rx) = mpsc::channel(64);
        let inner = Arc::new(NetInner {
            peer_id,
            inbound: inbound_tx,
            commands: command_tx,
            connected: Mutex::new(Vec::new()),
            listen_addrs: Mutex::new(Vec::new()),
        });
        let t = Arc::new(Self {
            inner: Arc::clone(&inner),
        });
        tokio::spawn(run_swarm(keypair, command_rx, inner));
        t
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.inner.peer_id
    }

    pub fn listen_addrs(&self) -> Vec<String> {
        self.inner.listen_addrs.lock().unwrap().clone()
    }

    /// 对外可达的拨号地址:监听在通配地址(0.0.0.0/::)时,把 IP 替换为
    /// 本机实际出口地址,并附上 /p2p/<peer-id>(供对端粘贴直接拨号)。
    pub fn dial_addrs(&self) -> Vec<String> {
        let addrs = self.inner.listen_addrs.lock().unwrap().clone();
        let pid = self.inner.peer_id.to_string();
        let v4 = local_ips();
        let mut out = Vec::new();
        for a in addrs {
            let Ok(parsed) = a.parse::<Multiaddr>() else {
                continue;
            };
            let mut rebuilt = Multiaddr::empty();
            for p in parsed.iter() {
                match p {
                    Protocol::Ip4(ip) if ip.is_unspecified() => match v4 {
                        Some(ip4) => rebuilt.push(Protocol::Ip4(ip4)),
                        None => continue,
                    },
                    Protocol::Ip6(ip) if ip.is_unspecified() => continue,
                    other => rebuilt.push(other),
                }
            }
            out.push(format!("{rebuilt}/p2p/{pid}"));
        }
        out
    }

    pub async fn dial(&self, addr: &str) -> Result<(), TransportError> {
        let addr: Multiaddr = addr
            .parse()
            .map_err(|e| TransportError::Io(format!("invalid multiaddr {addr}: {e}")))?;
        let (tx, rx) = oneshot::channel();
        self.inner
            .commands
            .send(Command::Dial { addr, ack: tx })
            .await
            .map_err(|_| TransportError::Io("net transport stopped".to_string()))?;
        rx.await
            .map_err(|_| TransportError::Io("net transport stopped".to_string()))?
            .map_err(TransportError::Io)
    }
}

impl Transport for NetTransport {
    fn name(&self) -> &'static str {
        "net"
    }

    fn availability(&self) -> Availability {
        Availability::Up
    }

    fn peers(&self) -> Vec<String> {
        self.inner.connected.lock().unwrap().clone()
    }

    fn dial(
        &self,
        addr: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>>
    {
        let addr = addr.to_string();
        Box::pin(async move {
            let addr: Multiaddr = addr
                .parse()
                .map_err(|e| TransportError::Io(format!("invalid multiaddr {addr}: {e}")))?;
            let (tx, rx) = oneshot::channel();
            self.inner
                .commands
                .send(Command::Dial { addr, ack: tx })
                .await
                .map_err(|_| TransportError::Io("net transport stopped".to_string()))?;
            rx.await
                .map_err(|_| TransportError::Io("net transport stopped".to_string()))?
                .map_err(TransportError::Io)
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
            let peer: PeerId = to
                .parse()
                .map_err(|_| TransportError::UnknownPeer(to.to_string()))?;
            inner
                .commands
                .send(Command::Send { peer, envelope })
                .await
                .map_err(|_| TransportError::Io("net transport stopped".to_string()))
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<Inbound> {
        self.inner.inbound.subscribe()
    }
}

/// 本机实际出口地址:通过 UDP "connect"(不发包)取默认路由出口 IPv4。
fn local_ips() -> Option<Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        _ => None,
    }
}

fn parse_dial_addr(addr: Multiaddr) -> (Multiaddr, Option<PeerId>) {
    let mut addr = addr;
    let pid = match addr.pop() {
        Some(Protocol::P2p(id)) => Some(id),
        Some(p) => {
            addr.push(p);
            None
        }
        None => None,
    };
    (addr, pid)
}

async fn run_swarm(keypair: Keypair, mut commands: mpsc::Receiver<Command>, inner: Arc<NetInner>) {
    let mut swarm = match build_swarm(keypair) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("net transport failed to start: {e}");
            return;
        }
    };

    // 监听 IPv4 与 IPv6 的随机端口
    for addr in ["/ip4/0.0.0.0/tcp/0", "/ip6/::/tcp/0"] {
        if let Ok(m) = addr.parse::<Multiaddr>() {
            let _ = swarm.listen_on(m);
        }
    }

    // 待发送队列(peer 未连接时暂存,连接建立后冲刷)
    let mut pending: HashMap<PeerId, Vec<Envelope>> = HashMap::new();

    loop {
        tokio::select! {
            cmd = commands.recv() => {
                match cmd {
                    Some(Command::Dial { addr, ack }) => {
                        let (base, pid) = parse_dial_addr(addr);
                        let r = if pid.is_some_and(|p| swarm.is_connected(&p)) {
                            Ok(())
                        } else {
                            let opts = match pid {
                                Some(p) => DialOpts::peer_id(p).addresses(vec![base]).build(),
                                None => DialOpts::unknown_peer_id().address(base).build(),
                            };
                            swarm.dial(opts).map_err(|e| e.to_string())
                        };
                        let _ = ack.send(r);
                    }
                    Some(Command::Send { peer, envelope }) => {
                        let connected = swarm.connected_peers().any(|p| *p == peer);
                        if connected {
                            let _ = swarm.behaviour_mut().request_response.send_request(&peer, envelope);
                        } else {
                            let q = pending.entry(peer).or_default();
                            if q.len() < MAX_PENDING_PER_PEER {
                                q.push(envelope);
                            }
                        }
                    }
                    None => break,
                }
            }
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        inner.listen_addrs.lock().unwrap().push(address.to_string());
                    }
                    SwarmEvent::ExpiredListenAddr { address, .. } => {
                        inner.listen_addrs.lock().unwrap().retain(|a| a != &address.to_string());
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        let pid = peer_id.to_string();
                        if !inner.connected.lock().unwrap().contains(&pid) {
                            inner.connected.lock().unwrap().push(pid);
                        }
                        if let Some(q) = pending.remove(&peer_id) {
                            for env in q {
                                let _ = swarm.behaviour_mut().request_response.send_request(&peer_id, env);
                            }
                        }
                    }
                    SwarmEvent::ConnectionClosed { peer_id, .. } => {
                        inner.connected.lock().unwrap().retain(|p| p != &peer_id.to_string());
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::RequestResponse(
                        request_response::Event::Message { peer, message, .. },
                    )) => {
                        match message {
                            request_response::Message::Request { request, channel, .. } => {
                                // 投递入站信封;ack 响应
                                let _ = inner.inbound.send(Inbound { from: peer.to_string(), envelope: request });
                                let _ = swarm.behaviour_mut().request_response.send_response(channel, Vec::new());
                            }
                            request_response::Message::Response { .. } => {}
                        }
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                        for (peer, addr) in list {
                            swarm.add_peer_address(peer, addr);
                        }
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Expired(_))) => {}
                    _ => {}
                }
            }
        }
    }
}

// 注:SwarmBuilder 各阶段返回 Result,逐段 ?(错误类型不同,统一为 Box<dyn Error>)。
fn build_swarm(
    keypair: Keypair,
) -> Result<Swarm<Behaviour>, Box<dyn std::error::Error + Send + Sync>> {
    let builder = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default().nodelay(true),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    let builder = builder
        .with_behaviour(Behaviour::new)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    Ok(builder
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build())
}
