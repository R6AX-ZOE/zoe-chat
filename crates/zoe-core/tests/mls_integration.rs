//! MLS 双节点集成测试:建群、加入、双向消息、update(PCS)、加第三人、移除成员。
//!
//! 每个参与者使用独立的 provider(存储),模拟真实的多设备拓扑。

use openmls_rust_crypto::OpenMlsRustCrypto;
use zoe_core::mls::{MlsIdentity, MlsSession, Processed};

fn identity(name: &str, byte: u8) -> MlsIdentity {
    MlsIdentity::new(name, &[byte; 32]).unwrap()
}

#[test]
fn two_party_messaging() {
    let alice_provider = OpenMlsRustCrypto::default();
    let bob_provider = OpenMlsRustCrypto::default();
    let alice = identity("alice", 0xAA);
    let bob = identity("bob", 0xBB);
    let group_id = b"integration-group-01";

    // 建群
    let mut alice_session = MlsSession::create_group(&alice_provider, &alice, group_id).unwrap();
    assert_eq!(alice_session.epoch(), 0);
    assert_eq!(alice_session.members(), vec![0]);

    // 邀请 bob:commit 给既有成员(无),welcome 给 bob
    let bob_kp = MlsSession::key_package_from_bytes(
        &bob_provider,
        &MlsSession::new_key_package(&bob_provider, &bob).unwrap(),
    )
    .unwrap();
    let (_, welcome, epoch) = alice_session
        .add_member(&alice_provider, &alice, &bob_kp)
        .unwrap();
    assert_eq!(epoch, 1);
    assert_eq!(alice_session.epoch(), 1);
    assert_eq!(alice_session.members(), vec![0, 1]);

    // bob 加入
    let mut bob_session = MlsSession::join(&bob_provider, &welcome).unwrap();
    assert_eq!(bob_session.epoch(), 1);

    // 双向消息
    let ct = alice_session
        .encrypt(&alice_provider, &alice, b"hello from alice")
        .unwrap();
    assert_eq!(
        bob_session.process(&bob_provider, &ct).unwrap(),
        Processed::Message(b"hello from alice".to_vec())
    );

    let ct = bob_session
        .encrypt(&bob_provider, &bob, b"hi alice")
        .unwrap();
    assert_eq!(
        alice_session.process(&alice_provider, &ct).unwrap(),
        Processed::Message(b"hi alice".to_vec())
    );

    // alice self_update(密钥轮换 → PCS),bob 处理 commit
    let commit = alice_session.self_update(&alice_provider, &alice).unwrap();
    assert_eq!(alice_session.epoch(), 2);
    assert_eq!(
        bob_session.process(&bob_provider, &commit).unwrap(),
        Processed::GroupChange
    );
    assert_eq!(bob_session.epoch(), 2);

    // update 后消息仍互通
    let ct = alice_session
        .encrypt(&alice_provider, &alice, b"after update")
        .unwrap();
    assert_eq!(
        bob_session.process(&bob_provider, &ct).unwrap(),
        Processed::Message(b"after update".to_vec())
    );
}

#[test]
fn third_member_join_and_remove() {
    let alice_provider = OpenMlsRustCrypto::default();
    let bob_provider = OpenMlsRustCrypto::default();
    let charlie_provider = OpenMlsRustCrypto::default();
    let alice = identity("alice", 0xAA);
    let bob = identity("bob", 0xBB);
    let charlie = identity("charlie", 0xCC);

    let mut alice_session = MlsSession::create_group(&alice_provider, &alice, b"group-3p").unwrap();

    // bob 加入
    let bob_kp = MlsSession::key_package_from_bytes(
        &bob_provider,
        &MlsSession::new_key_package(&bob_provider, &bob).unwrap(),
    )
    .unwrap();
    let (_, welcome, _) = alice_session
        .add_member(&alice_provider, &alice, &bob_kp)
        .unwrap();
    let mut bob_session = MlsSession::join(&bob_provider, &welcome).unwrap();

    // charlie 加入:commit 需给既有成员 bob
    let charlie_kp = MlsSession::key_package_from_bytes(
        &charlie_provider,
        &MlsSession::new_key_package(&charlie_provider, &charlie).unwrap(),
    )
    .unwrap();
    let (commit, welcome, epoch) = alice_session
        .add_member(&alice_provider, &alice, &charlie_kp)
        .unwrap();
    assert_eq!(epoch, 2);
    assert_eq!(
        bob_session.process(&bob_provider, &commit).unwrap(),
        Processed::GroupChange
    );
    assert_eq!(bob_session.epoch(), 2);
    let mut charlie_session = MlsSession::join(&charlie_provider, &welcome).unwrap();
    assert_eq!(charlie_session.epoch(), 2);

    assert_eq!(alice_session.members(), vec![0, 1, 2]);

    // 三人互通
    let ct = alice_session
        .encrypt(&alice_provider, &alice, b"to all")
        .unwrap();
    assert_eq!(
        bob_session.process(&bob_provider, &ct).unwrap(),
        Processed::Message(b"to all".to_vec())
    );
    assert_eq!(
        charlie_session.process(&charlie_provider, &ct).unwrap(),
        Processed::Message(b"to all".to_vec())
    );

    // alice 移除 bob(leaf 1):commit 给既有成员 charlie;bob 处理后被移除
    let commit = alice_session
        .remove_member(&alice_provider, &alice, 1)
        .unwrap();
    assert_eq!(alice_session.epoch(), 3);
    assert_eq!(
        charlie_session.process(&charlie_provider, &commit).unwrap(),
        Processed::GroupChange
    );
    assert_eq!(charlie_session.epoch(), 3);
    let _ = bob_session.process(&bob_provider, &commit).unwrap();

    // 移除后:charlie 与 alice 正常,新消息 bob 无法解密
    let ct = alice_session
        .encrypt(&alice_provider, &alice, b"bob should not read")
        .unwrap();
    assert_eq!(
        charlie_session.process(&charlie_provider, &ct).unwrap(),
        Processed::Message(b"bob should not read".to_vec())
    );
    assert!(bob_session.process(&bob_provider, &ct).is_err());
}
