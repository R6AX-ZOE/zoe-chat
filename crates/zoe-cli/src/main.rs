//! zoe-chat M0 调试 CLI。
//!
//! 子命令:
//!   init [--dir PATH]       生成身份,写入 identity.json(seed/指纹/助记词)
//!   fingerprint [--dir PATH] 打印本机身份指纹
//!   user <list|add|set-pin|activate>  多用户注册表管理(data_dir/users.db)
//!   demo                    双节点 loopback 演示:配对→建群→双向消息→update
//!   ble <adv|scan|connect>  BLE 真机联调(Linux,feature ble-linux,见 docs/termux-ble.md)

use std::path::PathBuf;

use openmls_rust_crypto::OpenMlsRustCrypto;
use zoe_core::envelope::{Envelope, FLAG_MULTIPATH, MSG_PRIVATE};
use zoe_core::identity::IdentityKeyPair;
use zoe_core::mls::{MlsIdentity, MlsSession, Processed};
use zoe_core::users::{UserRegistry, UserKind};
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
        Some("user") => cmd_user(&args),
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
            eprintln!("usage: zoe-cli <init|fingerprint|user|demo>");
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
    let text =
        std::fs::read_to_string(&path).expect("read identity.json (run `zoe-cli init` first)");
    for line in text.lines() {
        if line.contains("fingerprint_hex") {
            println!("{}", line.trim());
        }
    }
}

fn cmd_user(args: &[String]) {
    let dir = data_dir(args);
    let sub = args.get(2).map(String::as_str).unwrap_or("");
    let reg = UserRegistry::open(&dir).expect("open user registry");
    match sub {
        "list" => {
            let users = reg.list().expect("list users");
            if users.is_empty() {
                println!("no users in {}", dir.display());
                return;
            }
            let most_recent = reg.most_recent().expect("most recent").map(|u| u.user_id);
            for u in &users {
                let active = if most_recent.as_ref() == Some(&u.user_id) {
                    "*"
                } else {
                    " "
                };
                let last = u.last_used.unwrap_or(0);
                println!(
                    "{active} {}  kind={}  dir={}  last_used={}  id={}",
                    u.name,
                    u.kind.as_str(),
                    u.dir.display(),
                    last,
                    hex::encode(u.user_id)
                );
            }
            println!("(*) = activate (daemon default pick; restart to apply)");
        }
        "add" => {
            let name = flag(args, "--name").unwrap_or_else(|| panic!("user add needs --name <NAME>"));
            let pin = flag(args, "--pin").unwrap_or_else(|| panic!("user add needs --pin <PIN>"));
            let id = IdentityKeyPair::generate();
            let user = reg
                .add_pin_user(&name, &pin, &id.seed())
                .expect("create user");
            println!("created user: {} (id={})", user.name, hex::encode(user.user_id));
            println!("fingerprint: {}", hex::encode(id.fingerprint()));
            println!("activate: restart daemon with --user {}", hex::encode(user.user_id));
        }
        "set-pin" => {
            let id_hex = flag(args, "--id")
                .or_else(|| find_user_id(args))
                .expect("user set-pin needs <USER_ID> or --id");
            let pin = flag(args, "--pin").expect("user set-pin needs --pin <PIN>");
            let uid = hex::decode(&id_hex).expect("bad user id");
            // 升级为 PIN 保护需要当前种子:仅对明文用户可在注册表外读取其 zoe.db;
            // 对已解锁的 daemon 请走 API `/users/{id}/set-pin`(种子在内存)。
            let user = reg.get(&uid).expect("user not found");
            match user.kind {
                UserKind::Pin => {
                    // 未持有种子无法重加密 → 引导走 API
                    eprintln!(
                        "user '{}' is already PIN-protected; change its PIN via the daemon API \
                         (POST /api/v1/users/{{id}}/set-pin) while the user is active & unlocked",
                        user.name
                    );
                    std::process::exit(2);
                }
                UserKind::Plain => {
                    // 明文用户:从其数据目录读种子(仅根目录布局支持 CLI 升级)
                    let storage = zoe_core::storage::ZoeStorage::new(
                        zoe_core::storage::Db::open(&dir.join("zoe.db")).expect("open zoe.db"),
                    );
                    let (seed, _) = storage.identity().expect("read identity").expect("no identity");
                    reg.set_pin(&user.user_id, &pin, &seed).expect("set pin");
                    let _ = storage.set_meta("seed_enc", "1");
                    println!(
                        "user '{}' upgraded to PIN protection. Restart daemon with --user {} --pin <PIN>",
                        user.name,
                        hex::encode(user.user_id)
                    );
                }
            }
        }
        "activate" => {
            let id_hex = flag(args, "--id")
                .or_else(|| find_user_id(args))
                .expect("user activate needs <USER_ID> or --id");
            let uid = hex::decode(&id_hex).expect("bad user id");
            reg.get(&uid).expect("user not found");
            reg.set_last_used(&uid).expect("set last_used");
            println!("user active marker set; restart the daemon to switch users");
        }
        other => {
            eprintln!("usage: zoe-cli user <list|add --name N --pin P|set-pin <ID> --pin P|activate <ID>>");
            eprintln!("unknown user subcommand: {other}");
            std::process::exit(2);
        }
    }
}

/// 在散落的位置参数里找一个 64-hex 的用户 ID(跳过 --key 与 --key 的值)。
fn find_user_id(args: &[String]) -> Option<String> {
    let mut skip_next = false;
    for a in args.iter().skip(3) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with("--") {
            skip_next = true;
            continue;
        }
        if a.len() == 64 && hex::decode(a).is_ok() {
            return Some(a.clone());
        }
    }
    None
}

/// 从 args 中取 `--key value` 的值。
fn flag(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
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
