//! 回环 TCP 桥(M1):Rust 侧 `tokio::TcpListener` 只绑 `127.0.0.1:18570`。
//!
//! 协议(每行一个 JSON,UTF-8,字节载荷 hex 小写;规格见 docs/tauri-mobile.md "桥协议完整规格"):
//!   K→R: `{"t":"hello","v":1}` / `{"t":"frame","a":"<mac>","d":"<hex>"}` / `{"t":"log","d":"<text>"}`
//!   R→K: `{"t":"start","n":"<广播名>"}` / `{"t":"stop"}` / `{"t":"send","a":"<mac>","d":"<hex>"}` / `{"t":"echo","v":bool}`
//!
//! 状态机:`BridgeState { Disconnected, Connected }`(Mutex 单例 + 命令通道);
//! accept 循环常驻;accept 后等 Kotlin hello(5s 超时断开等重连);写失败/关闭 → Disconnected,
//! 继续 accept(重连由 Kotlin 每 2s 主动发起,坑 8:真机回环可用,不绑 0.0.0.0)。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// 桥端口:只绑回环,不暴露外部端口(坑 8;模拟器才需要 10.0.2.2,本项目只验收真机)。
const BRIDGE_ADDR: &str = "127.0.0.1:18570";
/// accept 后等待 Kotlin `hello` 的超时;超时断开,等 Kotlin 2s 后重连。
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// 缺省广播名(M2 起由 start 消息的 `n` 字段覆盖)。
const DEFAULT_ADV_NAME: &str = "zoe-device";
/// 命令通道容量(UI 点击级操作,足够)。
const CMD_CHANNEL: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BridgeState {
    Disconnected,
    Connected,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeStatus {
    connected: bool,
    last_error: Option<String>,
}

struct BridgeInner {
    state: Mutex<BridgeState>,
    last_error: Mutex<Option<String>>,
    /// 当前连接的 Kotlin 写端;None = 未连接。
    tx: Mutex<Option<mpsc::Sender<Value>>>,
    /// accept 循环是否已在跑(保证 start_bridge 幂等)。
    listening: AtomicBool,
    /// 用户请求过启动 BLE;hello 到达后补发 `{"t":"start"}`(幂等)。
    start_requested: AtomicBool,
}

fn inner() -> &'static BridgeInner {
    static BRIDGE: OnceLock<BridgeInner> = OnceLock::new();
    BRIDGE.get_or_init(|| BridgeInner {
        state: Mutex::new(BridgeState::Disconnected),
        last_error: Mutex::new(None),
        tx: Mutex::new(None),
        listening: AtomicBool::new(false),
        start_requested: AtomicBool::new(false),
    })
}

fn emit_log(app: &AppHandle, line: impl Into<String>) {
    let _ = app.emit("bridge-log", line.into());
}

fn set_state(s: BridgeState) {
    *inner().state.lock().unwrap() = s;
}

fn set_error(e: Option<String>) {
    *inner().last_error.lock().unwrap() = e;
}

/// 把 R→K 命令投递到当前连接;未连接时记日志不报错(命令全部幂等)。
fn send(v: Value, app: &AppHandle) {
    let tx = inner().tx.lock().unwrap().clone();
    match tx {
        Some(tx) => {
            if tx.try_send(v).is_err() {
                emit_log(app, "[桥] 命令通道已关闭,命令丢弃");
            }
        }
        None => emit_log(app, "[桥] 未连接,命令未发送"),
    }
}

async fn write_line(w: &mut tokio::net::tcp::OwnedWriteHalf, v: &Value) -> std::io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    w.write_all(line.as_bytes()).await?;
    w.flush().await
}

// ---- tauri commands(异步、幂等;坑 16:命令函数不要 pub,跨模块用 pub(crate)) ----

/// 启动桥(幂等):起 127.0.0.1:18570 监听,并向 Kotlin 发 `{"t":"start"}` 启动广播+GATT。
/// 未连接时先置 start_requested,hello 到达后补发(Kotlin 断线 2s 重连,连接建立即补发)。
#[tauri::command]
pub(crate) async fn start_bridge(app: AppHandle) -> Result<String, String> {
    if !inner().listening.swap(true, Ordering::SeqCst) {
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move { accept_loop(app2).await });
    }
    inner().start_requested.store(true, Ordering::SeqCst);
    send(json!({"t": "start", "n": DEFAULT_ADV_NAME}), &app);
    Ok("started".into())
}

/// 停止 BLE(发 `{"t":"stop"}`;保持 TCP,桥状态不变)。
#[tauri::command]
pub(crate) async fn stop_bridge(app: AppHandle) -> Result<String, String> {
    inner().start_requested.store(false, Ordering::SeqCst);
    send(json!({"t": "stop"}), &app);
    Ok("stopped".into())
}

/// 设置 Kotlin 侧 ZoeBleServer 自动回显(发 `{"t":"echo","v":...}`)。
#[tauri::command]
pub(crate) async fn set_echo(v: bool, app: AppHandle) -> Result<String, String> {
    send(json!({"t": "echo", "v": v}), &app);
    Ok("ok".into())
}

/// 桥状态:connected + 最近一次错误(前端轮询显示)。
#[tauri::command]
pub(crate) async fn bridge_status() -> BridgeStatus {
    let connected = *inner().state.lock().unwrap() == BridgeState::Connected;
    let last_error = inner().last_error.lock().unwrap().clone();
    BridgeStatus {
        connected,
        last_error,
    }
}

// ---- accept 循环(常驻) ----

async fn accept_loop(app: AppHandle) {
    let listener = match TcpListener::bind(BRIDGE_ADDR).await {
        Ok(l) => l,
        Err(e) => {
            inner().listening.store(false, Ordering::SeqCst); // 允许下次 start_bridge 重试
            let msg = format!("[桥] 监听 {BRIDGE_ADDR} 失败: {e}");
            set_error(Some(msg.clone()));
            emit_log(&app, msg);
            return;
        }
    };
    emit_log(&app, format!("[桥] 监听 {BRIDGE_ADDR} 就绪"));
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move { handle_conn(stream, app).await });
            }
            Err(e) => {
                let msg = format!("[桥] accept 失败: {e}");
                set_error(Some(msg.clone()));
                emit_log(&app, msg);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

// ---- 单连接处理 ----

async fn handle_conn(stream: TcpStream, app: AppHandle) {
    let (read_half, write_half) = stream.into_split();
    let (tx, rx) = mpsc::channel::<Value>(CMD_CHANNEL);
    let mut reader = BufReader::new(read_half).lines();
    let mut writer = write_half;

    // 等 Kotlin hello(5s 超时;超时/非法则断开,等 Kotlin 2s 后重连)
    let hello = tokio::time::timeout(HELLO_TIMEOUT, reader.next_line()).await;
    let hello_ok = match hello {
        Ok(Ok(Some(line))) => {
            let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            v.get("t").and_then(|t| t.as_str()) == Some("hello")
        }
        _ => false,
    };
    if !hello_ok {
        emit_log(&app, "[桥] hello 超时/非法,断开等待 Kotlin 重连");
        return; // 两个 half 一起 drop → socket 关闭
    }

    // 连接建立:注册写端 + 状态 Connected + 补发未决的 start
    *inner().tx.lock().unwrap() = Some(tx.clone());
    set_state(BridgeState::Connected);
    set_error(None);
    emit_log(&app, "[桥] Kotlin 已连接(hello)");
    if inner().start_requested.load(Ordering::SeqCst) {
        let _ = write_line(&mut writer, &json!({"t": "start", "n": DEFAULT_ADV_NAME})).await;
    }

    let mut rx = rx;
    loop {
        tokio::select! {
            line = reader.next_line() => {
                match line {
                    Ok(Some(l)) if !l.trim().is_empty() => on_line(&l, &app).await,
                    Ok(Some(_)) => {}
                    _ => break, // EOF/读错误 → 断开
                }
            }
            cmd = rx.recv() => {
                match cmd {
                    Some(c) => {
                        if write_line(&mut writer, &c).await.is_err() {
                            break; // 写失败 → 断开等重连
                        }
                    }
                    None => break,
                }
            }
        }
    }

    // 断开:清写端 + 状态 Disconnected;accept 循环继续等 Kotlin 重连
    *inner().tx.lock().unwrap() = None;
    set_state(BridgeState::Disconnected);
    emit_log(&app, "[桥] 桥断开,等待 Kotlin 重连");
}

// ---- K→R 消息(坏行/未知类型:丢弃 + 记日志,不中断连接) ----

async fn on_line(line: &str, app: &AppHandle) {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            emit_log(app, format!("[桥] 坏行已丢弃: {e}"));
            return;
        }
    };
    match v.get("t").and_then(|t| t.as_str()).unwrap_or("") {
        // Kotlin 日志行 → 原样转发 bridge-log(K→R→UI 全链路)
        "log" => {
            let d = v.get("d").and_then(|d| d.as_str()).unwrap_or("");
            emit_log(app, d.to_string());
        }
        // BLE 收到完整帧(含 13B 头):M1 先转发日志行;M2 起喂 MeshOverlay
        "frame" => {
            let a = v.get("a").and_then(|a| a.as_str()).unwrap_or("?");
            let d = v.get("d").and_then(|d| d.as_str()).unwrap_or("");
            emit_log(app, format!("[收] 帧 a={a} len={}B d={d}", d.len() / 2));
        }
        other => emit_log(app, format!("[桥] 未知消息类型: {other}")),
    }
}
