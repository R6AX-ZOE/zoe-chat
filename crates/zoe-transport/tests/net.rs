//! NetTransport 集成测试:两个传输端经 TCP 拨号互连,信封双向送达。
//! 仅当 net feature(libp2p)启用时编译。

#![cfg(feature = "net")]

use std::time::Duration;

use libp2p::identity::Keypair;
use zoe_core::envelope::{Envelope, MSG_PRIVATE};
use zoe_transport::net::NetTransport;
use zoe_transport::{Transport, TransportError};

async fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn dial_and_exchange_envelopes() {
    let a = NetTransport::spawn(Keypair::generate_ed25519());
    let b = NetTransport::spawn(Keypair::generate_ed25519());
    let mut b_rx = b.subscribe();

    // 等待 b 有监听地址
    assert!(
        wait_until(Duration::from_secs(10), || !b.listen_addrs().is_empty()).await,
        "b never got a listen addr"
    );
    let b_addr = b.listen_addrs()[0].clone();
    let b_peer = b.local_peer_id().to_string();

    // a 拨号 b
    a.dial(&b_addr).await.expect("dial");
    assert!(
        wait_until(Duration::from_secs(10), || a.peers().contains(&b_peer)).await,
        "a never connected to b (addr {b_addr})"
    );

    // a → b 信封
    let env = Envelope::new(
        0,
        MSG_PRIVATE,
        b"g1".to_vec(),
        1,
        0,
        7,
        b"hello over libp2p".to_vec(),
    );
    a.send(&b_peer, env.clone()).await.expect("send");
    let inbound = tokio::time::timeout(Duration::from_secs(10), b_rx.recv())
        .await
        .expect("timeout waiting inbound")
        .expect("channel closed");
    assert_eq!(inbound.from, a.local_peer_id().to_string());
    assert_eq!(inbound.envelope, env);

    // b → a 信封(反向)
    let mut a_rx = a.subscribe();
    let env2 = Envelope::new(
        0,
        MSG_PRIVATE,
        b"g1".to_vec(),
        1,
        1,
        8,
        b"reply over libp2p".to_vec(),
    );
    b.send(&a.local_peer_id().to_string(), env2.clone())
        .await
        .expect("send back");
    let inbound = tokio::time::timeout(Duration::from_secs(10), a_rx.recv())
        .await
        .expect("timeout waiting reply")
        .expect("channel closed");
    assert_eq!(inbound.envelope, env2);
}

#[tokio::test]
async fn unknown_peer_id_is_error() {
    let a = NetTransport::spawn(Keypair::generate_ed25519());
    let env = Envelope::new(0, MSG_PRIVATE, vec![], 0, 0, 1, vec![]);
    assert!(matches!(
        a.send("not-a-peer-id", env).await,
        Err(TransportError::UnknownPeer(_))
    ));
}
