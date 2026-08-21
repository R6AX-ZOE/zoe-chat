//! 传输抽象层:所有传输(BLE GATT 覆盖网 / SIG Mesh / libp2p / loopback)
//! 实现同一 trait,上层只依赖本面。

pub mod loopback;

#[cfg(feature = "net")]
pub mod net;

#[cfg(feature = "lan")]
pub mod lan;

#[cfg(feature = "sigmesh")]
pub mod sigmesh;

// 帧/MeshOverlay 为平台无关纯逻辑:linux/windows 驱动随各自 feature 编译,
// mobile(android)只启用 ble-mobile,复用同一份帧与存储转发实现。
#[cfg(any(feature = "ble-linux", feature = "ble-windows", feature = "ble-mobile"))]
pub mod ble;

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
    /// 发送信封(BoxFuture,`'_` 允许借用 self:对象安全,daemon 可持有
    /// `Arc<dyn Transport>`;实现也可返回 'static 的闭包捕获版)。
    fn send(
        &self,
        to: &str,
        envelope: Envelope,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>>;
    /// 手动拨号(默认不支持;libp2p/lan 等实现)。
    fn dial(
        &self,
        addr: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>>
    {
        let addr = addr.to_string();
        Box::pin(async move {
            Err(TransportError::Io(format!(
                "transport does not support dialing: {addr}"
            )))
        })
    }
    /// 入站信封流(每传输单订阅者)。
    fn subscribe(&self) -> broadcast::Receiver<Inbound>;
}
