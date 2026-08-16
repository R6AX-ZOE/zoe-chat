//! BLE 真机联调子命令(仅 Linux,feature `ble-linux`;Android/Termux 无 BlueZ,
//! 手机侧用 scripts/termux/ble-scan.sh + tools/ble-gatt-test 联调)。
//!
//! 子命令:
//!   zoe-cli ble adv [--name NAME] [--echo]     广播 + GATT 服务;--echo 回显收到的帧
//!   zoe-cli ble scan [--timeout SECS]          扫描附近 BLE 设备
//!   zoe-cli ble connect <MAC> [--send HEX] [--timeout SECS]   连接并打印通知流
//!
//! 帧格式见 docs/envelope.md §2.1:
//!   [magic 0x5A | msg_id 8 | ttl 1 | chunk_idx 1 | total 2 | data ≤499]

#![cfg(all(feature = "ble-linux", target_os = "linux"))]

use std::time::{Duration, Instant};

use zoe_transport::ble::linux::{LinuxDriver, NOTIFY_CHAR_UUID, SERVICE_UUID, WRITE_CHAR_UUID};
use zoe_transport::ble::{parse_frame, BleAddr, BleConn, BleDriver};

pub async fn cmd_ble(args: &[String]) {
    match args.get(2).map(String::as_str) {
        Some("adv") => cmd_adv(args).await,
        Some("scan") => cmd_scan(args).await,
        Some("connect") => cmd_connect(args).await,
        Some(other) => {
            eprintln!("unknown ble subcommand: {other}");
            usage();
            std::process::exit(2);
        }
        None => {
            usage();
            std::process::exit(2);
        }
    }
}

fn usage() {
    eprintln!(
        "usage: zoe-cli ble <adv|scan|connect> ...\n\
         \x20 zoe-cli ble adv [--name NAME] [--echo]\n\
         \x20 zoe-cli ble scan [--timeout SECS]\n\
         \x20 zoe-cli ble connect <MAC> [--send HEX] [--timeout SECS]"
    );
}

fn opt_val(args: &[String], key: &str, default: &str) -> String {
    match args.iter().position(|a| a == key) {
        Some(i) => args
            .get(i + 1)
            .cloned()
            .unwrap_or_else(|| default.to_string()),
        None => default.to_string(),
    }
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn print_frame(dir: &str, addr: &str, frame: &[u8]) {
    match parse_frame(frame) {
        Ok((hdr, data)) => println!(
            "[{dir}] {addr} frame msg_id={} ttl={} chunk={}/{} payload={}B {}",
            hex::encode(hdr.msg_id),
            hdr.ttl,
            hdr.chunk_idx,
            hdr.total,
            data.len(),
            hex::encode(data)
        ),
        Err(_) => println!("[{dir}] {addr} raw {}B {}", frame.len(), hex::encode(frame)),
    }
}

/// 广播 + GATT 服务端。每收到一帧打印解析结果;--echo 时把原帧回写
/// (手机侧 tools/ble-gatt-test 测试页可据此做往返验证)。
async fn cmd_adv(args: &[String]) {
    let name = opt_val(args, "--name", "zoe-device");
    let echo = has_flag(args, "--echo");

    let driver = LinuxDriver::new().await.unwrap_or_else(|e| {
        eprintln!("ble: 打开适配器失败: {e}");
        std::process::exit(1);
    });
    driver.start_advertising(&name).await.unwrap_or_else(|e| {
        eprintln!("ble: 启动广播失败: {e}");
        std::process::exit(1);
    });
    println!("== zoe-cli ble adv ==");
    println!("advertising as: {name}");
    println!("service:        {SERVICE_UUID}");
    println!("write char:     {WRITE_CHAR_UUID}  (客户端写入即入站帧)");
    println!("notify char:    {NOTIFY_CHAR_UUID} (客户端订阅后收出站帧)");
    println!("echo mode:      {echo}");
    println!("waiting for connections (Ctrl-C to stop) ...");

    let mut rx = driver.listen().await.unwrap_or_else(|e| {
        eprintln!("ble: GATT 服务启动失败: {e}");
        std::process::exit(1);
    });

    while let Some(mut conn) = rx.recv().await {
        let addr = conn.peer_addr().to_hex();
        let echo = echo;
        println!("[conn] peer connected: {addr}");
        tokio::spawn(async move {
            loop {
                match conn.read().await {
                    Ok(Some(frame)) => {
                        print_frame("rx", &addr, &frame);
                        if echo {
                            if let Err(e) = conn.write(&frame).await {
                                eprintln!("[tx] {addr} echo failed: {e}");
                                break;
                            }
                            println!("[tx] {addr} echoed {}B", frame.len());
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            println!("[conn] peer disconnected: {addr}");
        });
    }
}

/// 扫描附近 BLE 设备(等价于 MeshOverlay 的邻居发现)。
async fn cmd_scan(args: &[String]) {
    let secs: u64 = opt_val(args, "--timeout", "10").parse().unwrap_or(10);
    let driver = LinuxDriver::new().await.unwrap_or_else(|e| {
        eprintln!("ble: 打开适配器失败: {e}");
        std::process::exit(1);
    });
    println!("scanning for {secs}s ...");
    match driver.scan(Duration::from_secs(secs)).await {
        Ok(peers) => {
            if peers.is_empty() {
                println!("no BLE devices found (确认对端已开始广播、适配器已开启)");
                return;
            }
            println!("{:<22} {}", "ADDRESS", "NAME");
            for p in peers {
                println!("{:<22} {}", String::from_utf8_lossy(&p.addr.0), p.name);
            }
        }
        Err(e) => {
            eprintln!("ble scan failed: {e}");
            std::process::exit(1);
        }
    }
}

/// 连接指定 MAC 的 peripheral,打印其通知流;--send 在连接后写一帧原始 hex。
async fn cmd_connect(args: &[String]) {
    let mac = match args.get(3) {
        Some(m) if !m.starts_with("--") => m.clone(),
        _ => {
            eprintln!("usage: zoe-cli ble connect <MAC> [--send HEX] [--timeout SECS]");
            std::process::exit(2);
        }
    };
    let timeout = Duration::from_secs(opt_val(args, "--timeout", "30").parse().unwrap_or(30));

    let driver = LinuxDriver::new().await.unwrap_or_else(|e| {
        eprintln!("ble: 打开适配器失败: {e}");
        std::process::exit(1);
    });
    println!("connecting to {mac} ...");
    let mut conn = driver
        .connect(&BleAddr(mac.as_bytes().to_vec()))
        .await
        .unwrap_or_else(|e| {
            eprintln!("ble connect failed: {e}");
            std::process::exit(1);
        });
    println!("connected: {}", conn.peer_addr().to_hex());
    println!("listening for notifications (Ctrl-C to stop) ...");

    if let Some(hexstr) = args
        .iter()
        .position(|a| a == "--send")
        .and_then(|i| args.get(i + 1))
    {
        match hex::decode(hexstr) {
            Ok(bytes) => match conn.write(&bytes).await {
                Ok(()) => println!("[tx] sent {}B {hexstr}", bytes.len()),
                Err(e) => eprintln!("[tx] send failed: {e}"),
            },
            Err(e) => eprintln!("--send 不是合法 hex: {e}"),
        }
    }

    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            println!("timeout after {timeout:?}, exiting");
            return;
        }
        match tokio::time::timeout(remaining, conn.read()).await {
            Ok(Ok(Some(frame))) => print_frame("rx", &mac, &frame),
            Ok(Ok(None)) | Ok(Err(_)) => {
                println!("connection closed");
                return;
            }
            Err(_) => {
                println!("timeout after {timeout:?}, exiting");
                return;
            }
        }
    }
}
