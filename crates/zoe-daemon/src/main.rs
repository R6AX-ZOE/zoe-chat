//! zoe-chat 守护进程入口(薄壳,逻辑在 lib.rs)。
//!
//! 用法:zoe-daemon [--data-dir PATH] [--port N] [--user HEX] [--pin STR]
//! 默认监听 127.0.0.1 随机端口,端口持久化到 data-dir/port(重启复用,无访问令牌)。
//! 多用户:`--user` 指定激活用户(user_id hex,默认最近使用);PIN 保护用户
//! 启动需 `--pin`,否则以锁定模式启动(仅 /users 与 /unlock 可用);
//! `POST /users/{id}/activate` 切换活跃用户并自重启(需本进程为 CLI 模式)。

use std::path::PathBuf;

use zoe_daemon::{start, DaemonConfig};

struct Args {
    data_dir: PathBuf,
    port: u16,
    user_id: Option<String>,
    pin: Option<String>,
}

fn parse_args() -> Args {
    let mut data_dir = PathBuf::from("zoe-data");
    let mut port: u16 = 0;
    let mut user_id = None;
    let mut pin = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                i += 1;
                data_dir = PathBuf::from(args.get(i).expect("--data-dir needs a value"));
            }
            "--port" => {
                i += 1;
                port = args
                    .get(i)
                    .expect("--port needs a value")
                    .parse()
                    .expect("invalid port");
            }
            "--user" => {
                i += 1;
                user_id = Some(args.get(i).expect("--user needs a value").clone());
            }
            "--pin" => {
                i += 1;
                pin = Some(args.get(i).expect("--pin needs a value").clone());
            }
            other => panic!("unknown argument: {other}"),
        }
        i += 1;
    }
    Args {
        data_dir,
        port,
        user_id,
        pin,
    }
}

fn main() {
    let args = parse_args();
    zoe_daemon::enable_self_relaunch();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(run(args));
}

async fn run(args: Args) {
    let daemon = start(DaemonConfig {
        data_dir: args.data_dir.clone(),
        port: args.port,
        user_id: args.user_id.clone(),
        pin: args.pin.clone(),
        mobile: false,
        system_hook: None,
        #[cfg(feature = "sigmesh")]
        sigmesh_net: None,
    })
    .await
    .expect("daemon start");

    println!("zoe-chat daemon ready: http://{}", daemon.addr);
    println!("data dir: {}", args.data_dir.display());
    println!(
        "switch user: POST /api/v1/users/<id>/activate (daemon self-restarts, port stays {})",
        daemon.addr.port()
    );
    if !zoe_daemon::state::is_unlocked(&daemon.state) {
        println!(
            "LOCKED MODE: active user requires a PIN. POST /api/v1/unlock {{pin}} or restart with --pin."
        );
    }
    let user = zoe_daemon::state::active_user(&daemon.state);
    println!(
        "active user: {} ({}, {})",
        user.name,
        user.kind.as_str(),
        hex::encode(&user.user_id[..4])
    );

    std::future::pending::<()>().await;
}
