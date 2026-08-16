//! 持久化集成测试:ZoeProvider(官方 SqliteStorage)下建群、消息,
//! 会话重载后状态完整、可继续加解密。

use std::path::PathBuf;

use openmls_rust_crypto::OpenMlsRustCrypto;
use zoe_core::mls::{MlsIdentity, MlsSession, Processed};
use zoe_core::storage::{Db, ZoeProvider, ZoeStorage};

fn tmp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "zoe-test-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn group_session_persists_across_reload() {
    let dir = tmp_dir("persist");
    let alice_mls_db = dir.join("alice-mls.db");
    let _ = std::fs::remove_file(&alice_mls_db);

    // --- 第一次生命周期:alice 建群、邀请 bob、双向消息 ---
    {
        let alice_provider = ZoeProvider::new(&alice_mls_db).unwrap();
        let bob_provider = OpenMlsRustCrypto::default();
        let alice = MlsIdentity::new("alice", &[0xAA; 32]).unwrap();
        let bob = MlsIdentity::new("bob", &[0xBB; 32]).unwrap();
        let group_id = b"persist-group";

        let mut alice_session = MlsSession::create_group(&alice_provider, &alice, group_id).unwrap();
        let bob_kp = MlsSession::key_package_from_bytes(
            &bob_provider,
            &MlsSession::new_key_package(&bob_provider, &bob).unwrap(),
        )
        .unwrap();
        let (_, welcome, epoch) = alice_session
            .add_member(&alice_provider, &alice, &bob_kp)
            .unwrap();
        assert_eq!(epoch, 1);
        let mut bob_session = MlsSession::join(&bob_provider, &welcome).unwrap();

        let ct = alice_session
            .encrypt(&alice_provider, &alice, b"before restart")
            .unwrap();
        assert_eq!(
            bob_session.process(&bob_provider, &ct).unwrap(),
            Processed::Message(b"before restart".to_vec())
        );
    } // 会话与 provider 全部 drop

    // --- 第二次生命周期:从磁盘重载 alice 会话 ---
    let alice_provider = ZoeProvider::new(&alice_mls_db).unwrap();
    let bob_provider = OpenMlsRustCrypto::default();
    let alice = MlsIdentity::new("alice", &[0xAA; 32]).unwrap();
    let bob = MlsIdentity::new("bob", &[0xBB; 32]).unwrap();

    let mut alice_session = MlsSession::load(&alice_provider, b"persist-group")
        .unwrap()
        .expect("group should be persisted");
    assert_eq!(alice_session.epoch(), 1);
    assert_eq!(alice_session.members(), vec![0, 1]);

    // 重载后可继续加密,新成员仍可加入并解密(棘轮状态完整)
    let carol_provider = OpenMlsRustCrypto::default();
    let carol = MlsIdentity::new("carol", &[0xCC; 32]).unwrap();
    let mut bob_session = {
        let kp = MlsSession::key_package_from_bytes(
            &carol_provider,
            &MlsSession::new_key_package(&carol_provider, &carol).unwrap(),
        )
        .unwrap();
        let (_, welcome, _) = alice_session
            .add_member(&alice_provider, &alice, &kp)
            .unwrap();
        MlsSession::join(&carol_provider, &welcome).unwrap()
    };

    let ct = alice_session
        .encrypt(&alice_provider, &alice, b"post-reload roundtrip")
        .unwrap();
    assert_eq!(
        bob_session.process(&bob_provider, &ct).unwrap(),
        Processed::Message(b"post-reload roundtrip".to_vec())
    );

    // 清理
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn app_storage_persists_identity_and_settings() {
    let dir = tmp_dir("app");
    let db_path = dir.join("zoe.db");
    let _ = std::fs::remove_file(&db_path);

    {
        let db = Db::open(&db_path).unwrap();
        let s = ZoeStorage::new(db);
        s.set_identity(&[0x42; 32], 99).unwrap();
        s.set_meta("ui_theme", "dark").unwrap();
    }
    {
        let db = Db::open(&db_path).unwrap();
        let s = ZoeStorage::new(db);
        let (seed, created) = s.identity().unwrap().unwrap();
        assert_eq!(seed, [0x42; 32]);
        assert_eq!(created, 99);
        assert_eq!(s.get_meta("ui_theme").unwrap(), Some("dark".to_string()));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
