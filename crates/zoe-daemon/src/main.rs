//! zoe-chat 守护进程入口(薄壳,逻辑在 lib.rs)。
//!
//! 用法:zoe-daemon [--data-dir PATH] [--port N] [--token STR]
//! 默认监听 127.0.0.1 随机端口;首次启动生成访问令牌写入 data-dir/token。

use std::path::PathBuf;

use zoe_daemon::{start, DaemonConfig};

struct Args {
    data_dir: PathBuf,
    port: u16,
    token: Option<String>,
}

fn parse_args() -> Args {
    let mut data_dir = PathBuf::from("zoe-data");
    let mut port: u16 = 0;
    let mut token = None;
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
            "--token" => {
                i += 1;
                token = Some(args.get(i).expect("--token needs a value").clone());
            }
            other => panic!("unknown argument: {other}"),
        }
        i += 1;
    }
    Args {
        data_dir,
        port,
        token,
    }
}

fn main() {
    let args = parse_args();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(run(args));
}

async fn run(args: Args) {
    let daemon = start(DaemonConfig {
        data_dir: args.data_dir.clone(),
        port: args.port,
        token: args.token.clone(),
    })
    .await
    .expect("daemon start");

    println!("zoe-chat daemon ready: http://{}", daemon.addr);
    if args.token.is_none() {
        let token = zoe_daemon::load_or_create_token(&args.data_dir).expect("token");
        println!("access token: {token}");
    }
    println!("data dir: {}", args.data_dir.display());

    std::future::pending::<()>().await;
}
