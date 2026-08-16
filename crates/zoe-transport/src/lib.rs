//! 传输抽象层:所有传输(BLE GATT 覆盖网 / SIG Mesh / libp2p / loopback)
//! 实现同一 trait,上层只依赖本面。

pub mod loopback;

#[cfg(feature = "net")]
pub mod net;

#[cfg(any(feature = "ble-linux", feature = "ble-windows"))]
pub mod ble;

use std::future::Future;

use tokio::sync::broadcast;
use zoe_core::envelope::Envelope;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Up,
    Down,
    Degraded,
}

#[derive(Clone, Debug)]
pub struct Inbound {
    pub from: String,
    pub envelope: Envelope,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("unknown peer `{0}`")]
    UnknownPeer(String),
    #[error("peer unreachable: {0}")]
    Unreachable(String),
    #[error("queue full")]
    QueueFull,
    #[error("io: {0}")]
    Io(String),
}

pub trait Transport: Send + Sync {
    fn name(&self) -> &'static str;
    fn availability(&self) -> Availability;
    /// 当前可达邻居地址。
    fn peers(&self) -> Vec<String>;
    fn send(
        &self,
        to: &str,
        envelope: Envelope,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;
    /// 手动拨号(默认不支持;libp2p 等远程传输实现)。
    fn dial(&self, addr: &str) -> impl Future<Output = Result<(), TransportError>> + Send {
        async move {
            Err(TransportError::Io(format!(
                "transport does not support dialing: {addr}"
            )))
        }
    }
    /// 入站信封流(每传输单订阅者)。
    fn subscribe(&self) -> broadcast::Receiver<Inbound>;
}
