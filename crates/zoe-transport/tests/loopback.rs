//! loopback 传输测试:路由、未知 peer、入站接收。

use tokio::sync::broadcast;
use zoe_core::envelope::Envelope;
use zoe_transport::loopback::LoopbackHub;
use zoe_transport::{Transport, TransportError};

#[tokio::test]
async fn route_between_two_endpoints() {
    let hub = LoopbackHub::new();
    let alice = hub.attach("alice");
    let bob = hub.attach("bob");

    let mut bob_rx: broadcast::Receiver<_> = bob.subscribe();
    let env = Envelope::new(0, 0, b"g".to_vec(), 0, 0, 1, b"payload".to_vec());

    alice.send("bob", env.clone()).await.unwrap();
    let inbound = bob_rx.recv().await.unwrap();
    assert_eq!(inbound.from, "alice");
    assert_eq!(inbound.envelope, env);

    let mut peers = alice.peers();
    peers.sort();
    assert_eq!(peers, vec!["alice".to_string(), "bob".to_string()]);
}

#[tokio::test]
async fn unknown_peer_is_error() {
    let hub = LoopbackHub::new();
    let alice = hub.attach("alice");
    let env = Envelope::new(0, 0, vec![], 0, 0, 1, vec![]);
    assert!(matches!(
        alice.send("nobody", env).await,
        Err(TransportError::UnknownPeer(_))
    ));
}
