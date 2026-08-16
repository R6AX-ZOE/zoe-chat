//! Loopback 传输:M0 开发/测试用内存通道(点对点路由,无去重/转发)。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use zoe_core::envelope::Envelope;

use crate::{Availability, Inbound, Transport, TransportError};

/// 内存路由中枢:地址 → 广播通道。
pub struct LoopbackHub {
    peers: Mutex<HashMap<String, broadcast::Sender<Inbound>>>,
}

impl LoopbackHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            peers: Mutex::new(HashMap::new()),
        })
    }

    /// 注册一个地址,返回对应传输端。
    pub fn attach(self: &Arc<Self>, addr: &str) -> LoopbackTransport {
        let (tx, _rx) = broadcast::channel(256);
        self.peers.lock().unwrap().insert(addr.to_string(), tx.clone());
        LoopbackTransport {
            addr: addr.to_string(),
            hub: Arc::clone(self),
            own: tx,
        }
    }

    pub fn addrs(&self) -> Vec<String> {
        self.peers.lock().unwrap().keys().cloned().collect()
    }
}

pub struct LoopbackTransport {
    addr: String,
    hub: Arc<LoopbackHub>,
    own: broadcast::Sender<Inbound>,
}

impl Transport for LoopbackTransport {
    fn name(&self) -> &'static str {
        "loopback"
    }

    fn availability(&self) -> Availability {
        Availability::Up
    }

    fn peers(&self) -> Vec<String> {
        self.hub.addrs()
    }

    async fn send(&self, to: &str, envelope: Envelope) -> Result<(), TransportError> {
        let tx = self
            .hub
            .peers
            .lock()
            .unwrap()
            .get(to)
            .cloned()
            .ok_or_else(|| TransportError::UnknownPeer(to.to_string()))?;
        tx.send(Inbound {
            from: self.addr.clone(),
            envelope,
        })
        .map_err(|_| TransportError::Unreachable(to.to_string()))?;
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<Inbound> {
        self.own.subscribe()
    }
}
