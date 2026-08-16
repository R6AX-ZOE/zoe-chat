//! zoe-chat M0 调试 CLI。
//!
//! 子命令:
//!   init [--dir PATH]       生成身份,写入 identity.json(seed/指纹/助记词)
//!   fingerprint [--dir PATH] 打印本机身份指纹
//!   demo                    双节点 loopback 演示:配对→建群→双向消息→update
//!   ble <adv|scan|connect>  BLE 真机联调(Linux,feature ble-linux,见 docs/termux-ble.md)

use std::path::PathBuf;

use openmls_rust_crypto::OpenMlsRustCrypto;
use zoe_core::envelope::{Envelope, FLAG_MULTIPATH, MSG_PRIVATE};
use zoe_core::identity::IdentityKeyPair;
use zoe_core::mls::{MlsIdentity, MlsSession, Processed};
use zoe_transport::loopback::LoopbackHub;
use zoe_transport::Transport;

#[cfg(any(
    all(feature = "ble-linux", target_os = "linux"),
    all(feature = "ble-windows", windows)
))]
mod ble;

const IDENTITY_FILE: &str = "identity.json";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("init") => cmd_init(&args),
        Some("fingerprint") => cmd_fingerprint(&args),
        Some("demo") => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(cmd_demo());
        }
        #[cfg(any(
            all(feature = "ble-linux", target_os = "linux"),
            all(feature = "ble-windows", windows)
        ))]
        Some("ble") => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(ble::cmd_ble(&args));
        }
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            std::process::exit(2);
        }
        None => {
            eprintln!("usage: zoe-cli <init|fingerprint|demo>");
            std::process::exit(2);
        }
    }
}

fn data_dir(args: &[String]) -> PathBuf {
    match args.iter().position(|a| a == "--dir") {
        Some(i) => args.get(i + 1).map(PathBuf::from).unwrap_or_default(),
        None => PathBuf::from("."),
    }
}

fn cmd_init(args: &[String]) {
    let dir = data_dir(args);
    std::fs::create_dir_all(&dir).expect("create data dir");
    let id = IdentityKeyPair::generate();
    let fingerprint = hex::encode(id.fingerprint());
    let seed = hex::encode(id.seed());
    let mnemonic = id.to_mnemonic();

    let json = format!(
        "{{\n  \"seed_hex\": \"{seed}\",\n  \"fingerprint_hex\": \"{fingerprint}\",\n  \"mnemonic\": \"{mnemonic}\"\n}}\n"
    );
    let path = dir.join(IDENTITY_FILE);
    std::fs::write(&path, json).expect("write identity.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    println!("identity written to {}", path.display());
    println!("fingerprint: {fingerprint}");
    println!("mnemonic: {mnemonic}");
}

fn cmd_fingerprint(args: &[String]) {
    let dir = data_dir(args);
    let path = dir.join(IDENTITY_FILE);
    let text = std::fs::read_to_string(&path).expect("read identity.json (run `zoe-cli init` first)");
    for line in text.lines() {
        if line.contains("fingerprint_hex") {
            println!("{}", line.trim());
        }
    }
}

/// 双节点演示:alice 建群,邀请 bob,双向消息,alice self_update 后再发一条。
/// 消息以 Envelope 经 loopback 传输投递,走完整 M0 栈。
/// 每节点独立 provider(存储),模拟真实设备。
async fn cmd_demo() {
    let alice_provider = OpenMlsRustCrypto::default();
    let bob_provider = OpenMlsRustCrypto::default();

    // 身份(演示用随机种子)
    let alice_id = MlsIdentity::new("alice", &rand::random()).expect("alice mls identity");
    let bob_id = MlsIdentity::new("bob", &rand::random()).expect("bob mls identity");

    println!("== zoe-chat M0 demo (loopback) ==");

    // 配对:交换指纹(演示为打印;真实流程见 docs/protocol.md)
    println!("pairing alice <-> bob (fingerprints exchanged)");

    // 传输
    let hub = LoopbackHub::new();
    let alice_t = hub.attach("alice");
    let bob_t = hub.attach("bob");
    let mut alice_rx = alice_t.subscribe();
    let mut bob_rx = bob_t.subscribe();

    // 会话
    let group_id = b"demo-group".to_vec();
    let mut alice =
        MlsSession::create_group(&alice_provider, &alice_id, &group_id).expect("create group");

    let bob_kp = MlsSession::key_package_from_bytes(
        &bob_provider,
        &MlsSession::new_key_package(&bob_provider, &bob_id).expect("bob key package"),
    )
    .expect("parse bob key package");
    let (_commit, welcome, _) = alice
        .add_member(&alice_provider, &alice_id, &bob_kp)
        .expect("add bob");
    let mut bob = MlsSession::join(&bob_provider, &welcome).expect("bob join");

    println!(
        "group created: {} members, epoch {}",
        alice.members().len(),
        alice.epoch()
    );

    // alice → bob
    let mut seq_a: u64 = 0;
    let mut seq_b: u64 = 0;
    seq_a += 1;
    let ct = alice
        .encrypt(&alice_provider, &alice_id, b"hello from alice")
        .expect("encrypt");
    let env = Envelope::new(
        FLAG_MULTIPATH,
        MSG_PRIVATE,
        group_id.clone(),
        alice.epoch() as u32,
        0,
        seq_a,
        ct,
    );
    alice_t.send("bob", env).await.expect("send alice→bob");
    let inbound = bob_rx.recv().await.expect("bob recv");
    match bob
        .process(&bob_provider, &inbound.envelope.payload)
        .expect("bob process")
    {
        Processed::Message(m) => println!("alice → bob: {}", String::from_utf8_lossy(&m)),
        other => panic!("unexpected: {other:?}"),
    }

    // bob → alice
    seq_b += 1;
    let ct = bob
        .encrypt(&bob_provider, &bob_id, b"hi alice")
        .expect("encrypt");
    let env = Envelope::new(
        0,
        MSG_PRIVATE,
        group_id.clone(),
        bob.epoch() as u32,
        1,
        seq_b,
        ct,
    );
    bob_t.send("alice", env).await.expect("send bob→alice");
    let inbound = alice_rx.recv().await.expect("alice recv");
    match alice
        .process(&alice_provider, &inbound.envelope.payload)
        .expect("alice process")
    {
        Processed::Message(m) => println!("bob → alice: {}", String::from_utf8_lossy(&m)),
        other => panic!("unexpected: {other:?}"),
    }

    // alice self_update(密钥轮换),bob 处理
    let commit = alice
        .self_update(&alice_provider, &alice_id)
        .expect("self update");
    seq_a += 1;
    let env = Envelope::new(
        0,
        2, /* MSG_COMMIT */
        group_id.clone(),
        alice.epoch() as u32,
        0,
        seq_a,
        commit,
    );
    alice_t.send("bob", env).await.expect("send commit");
    let inbound = bob_rx.recv().await.expect("bob recv commit");
    assert_eq!(
        bob.process(&bob_provider, &inbound.envelope.payload)
            .expect("bob process commit"),
        Processed::GroupChange
    );
    println!("alice self_update applied by bob (epoch {})", bob.epoch());

    // update 后消息仍互通
    seq_a += 1;
    let ct = alice
        .encrypt(&alice_provider, &alice_id, b"after update")
        .expect("encrypt");
    let env = Envelope::new(
        0,
        MSG_PRIVATE,
        group_id.clone(),
        alice.epoch() as u32,
        0,
        seq_a,
        ct,
    );
    alice_t.send("bob", env).await.expect("send");
    let inbound = bob_rx.recv().await.expect("bob recv");
    match bob
        .process(&bob_provider, &inbound.envelope.payload)
        .expect("bob process")
    {
        Processed::Message(m) => println!("alice → bob: {}", String::from_utf8_lossy(&m)),
        other => panic!("unexpected: {other:?}"),
    }

    println!("== demo OK: messages, group, update verified ==");
}
