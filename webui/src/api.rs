//! 守护进程 API 客户端(Bearer token 认证)+ WebSocket 事件流。
//! 与 docs/api.md 契约一致;经相对路径访问(桌面 daemon 与移动端内嵌 daemon 同源)。

use gloo_net::http::Request;
use gloo_net::websocket::{futures::WebSocket, Message};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wasm_bindgen_futures::spawn_local;

const TOKEN_KEY: &str = "zoe.token";

pub fn get_token() -> Option<String> {
    local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item(TOKEN_KEY).ok().flatten())
        .filter(|t| !t.is_empty())
}

pub fn set_token(token: &str) {
    if let Ok(Some(s)) = local_storage() {
        let _ = s.set_item(TOKEN_KEY, token);
    }
}

pub fn clear_token() {
    if let Ok(Some(s)) = local_storage() {
        let _ = s.remove_item(TOKEN_KEY);
    }
}

fn local_storage() -> Result<Option<web_sys::Storage>, ()> {
    let w = web_sys::window().ok_or(())?;
    w.local_storage().map_err(|_| ())
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub user_id: String,
    pub fingerprint: String,
    pub created_at: i64,
    pub device: Device,
    pub devices: Vec<Device>,
    pub started_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    pub device_id: String,
    pub name: String,
    pub revoked: bool,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Card {
    pub peer_id: String,
    pub fingerprint: String,
    pub qr_svg: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Peer {
    pub peer_id: String,
    pub fingerprint: String,
    pub display_name: Option<String>,
    pub trust_status: i64,
    #[serde(default)]
    pub first_seen: i64,
    #[serde(default)]
    pub last_seen: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    pub group_id: String,
    pub name: Option<String>,
    pub epoch: u64,
    pub coordinator: Option<String>,
    pub members: Vec<u32>,
    #[serde(default)]
    pub created_at: i64,
    /// 单聊(私聊)标记:直接与会话列表一起返回。
    #[serde(default)]
    pub direct: bool,
    /// 单聊对端 libp2p peer id。
    #[serde(default)]
    pub direct_peer: Option<String>,
    /// 单聊对端在联系人表中的 zoe peer id(hex;未登记为 None)。
    #[serde(default)]
    pub direct_peer_id: Option<String>,
    /// 单聊对端展示名(解析自联系人表)。
    #[serde(default)]
    pub direct_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub mime: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Msg {
    pub id: i64,
    #[serde(default)]
    pub msg_hash: String,
    pub seq: Option<u64>,
    pub direction: i64,
    pub status: i64,
    pub text: Option<String>,
    /// 文件消息元数据(文本消息为 None)。
    #[serde(default)]
    pub file: Option<FileInfo>,
    #[serde(default)]
    pub file_downloaded: bool,
    pub received_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Direct {
    pub group_id: String,
    pub peer_id: Option<String>,
    pub name: Option<String>,
    pub epoch: u64,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransportStatus {
    pub ble: String,
    pub lan: String,
    pub net: String,
    pub loopback: String,
    #[serde(default)]
    pub sigmesh: Option<String>,
    #[serde(default)]
    pub net_peers: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetAddr {
    pub peer_id: String,
    pub listen_addrs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PairStart {
    pub pair_code: String,
    pub bt_advertising: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub ui_theme: Option<String>,
    pub ui_language: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LoginReq {
    token: String,
}

#[derive(Debug, Clone, Serialize)]
struct TextReq {
    text: String,
}

#[derive(Debug, Clone, Serialize)]
struct VerifyReq {
    peer_id: String,
    ok: bool,
}

// ---------------------------------------------------------------------------
// HTTP 请求
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ApiError(pub u16, pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "api {}: {}", self.0, self.1)
    }
}

async fn request<T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: Option<String>,
) -> Result<T, ApiError> {
    let mut builder = match method {
        "POST" => Request::post(path),
        _ => Request::get(path),
    };
    if let Some(token) = get_token() {
        builder = builder.header("Authorization", &format!("Bearer {token}"));
    }
    let req = match body {
        Some(b) => {
            let builder = builder.header("Content-Type", "application/json");
            builder.body(b).map_err(|e| ApiError(0, e.to_string()))?
        }
        None => builder.build().map_err(|e| ApiError(0, e.to_string()))?,
    };
    let resp = req.send().await.map_err(|e| ApiError(0, e.to_string()))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| ApiError(status, e.to_string()))?;
    if status == 401 {
        clear_token();
    }
    if !(200..300).contains(&status) {
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(ApiError(status, msg));
    }
    serde_json::from_str(&text).map_err(|e| ApiError(status, e.to_string()))
}

pub async fn login(token: &str) -> Result<(), ApiError> {
    // 注意:服务端返回 {"ok":true},必须用 Value 解析 —— 泛型 () 无法反序列化 map
    let _: serde_json::Value = request(
        "POST",
        "/api/v1/login",
        Some(
            serde_json::to_string(&LoginReq {
                token: token.to_string(),
            })
            .unwrap(),
        ),
    )
    .await?;
    Ok(())
}

pub async fn me() -> Result<Me, ApiError> {
    request("GET", "/api/v1/me", None).await
}

pub async fn card() -> Result<Card, ApiError> {
    request("GET", "/api/v1/card", None).await
}

pub async fn import_card(text: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        "/api/v1/card/import",
        Some(format!(
            "{{\"text\":{}}}",
            serde_json::to_string(text).unwrap()
        )),
    )
    .await?;
    Ok(())
}

pub async fn peers() -> Result<Vec<Peer>, ApiError> {
    request("GET", "/api/v1/peers", None).await
}

pub async fn block_peer(peer_id: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        &format!("/api/v1/peers/{}/block", encode(peer_id)),
        None,
    )
    .await?;
    Ok(())
}

pub async fn pair_start() -> Result<PairStart, ApiError> {
    request("POST", "/api/v1/pair/start", None).await
}

pub async fn pair_stop() -> Result<(), ApiError> {
    let _: serde_json::Value = request("POST", "/api/v1/pair/stop", None).await?;
    Ok(())
}

pub async fn pair_verify(peer_id: &str, ok: bool) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        "/api/v1/pair/verify",
        Some(
            serde_json::to_string(&VerifyReq {
                peer_id: peer_id.to_string(),
                ok,
            })
            .unwrap(),
        ),
    )
    .await?;
    Ok(())
}

pub async fn groups() -> Result<Vec<Group>, ApiError> {
    request("GET", "/api/v1/groups", None).await
}

pub async fn create_group(name: &str) -> Result<Group, ApiError> {
    request(
        "POST",
        "/api/v1/groups",
        Some(format!(
            "{{\"name\":{}}}",
            serde_json::to_string(name).unwrap()
        )),
    )
    .await
}

pub async fn invite(group_id: &str, addr: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        &format!("/api/v1/groups/{}/invite", encode(group_id)),
        Some(format!(
            "{{\"addr\":{}}}",
            serde_json::to_string(addr).unwrap()
        )),
    )
    .await?;
    Ok(())
}

pub async fn leave_group(group_id: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        &format!("/api/v1/groups/{}/leave", encode(group_id)),
        None,
    )
    .await?;
    Ok(())
}

pub async fn messages(
    group_id: &str,
    limit: u32,
    before: Option<i64>,
) -> Result<Vec<Msg>, ApiError> {
    let before_q = before.map(|b| format!("&before={b}")).unwrap_or_default();
    request(
        "GET",
        &format!(
            "/api/v1/groups/{}/messages?limit={limit}{before_q}",
            encode(group_id)
        ),
        None,
    )
    .await
}

pub async fn send_message(group_id: &str, text: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        &format!("/api/v1/groups/{}/messages", encode(group_id)),
        Some(
            serde_json::to_string(&TextReq {
                text: text.to_string(),
            })
            .unwrap(),
        ),
    )
    .await?;
    Ok(())
}

pub async fn directs() -> Result<Vec<Direct>, ApiError> {
    request("GET", "/api/v1/directs", None).await
}

pub async fn start_direct(peer_id: &str, addr: Option<&str>) -> Result<Direct, ApiError> {
    let mut body = format!(
        "{{\"peer_id\":{}}}",
        serde_json::to_string(peer_id).unwrap()
    );
    if let Some(a) = addr.filter(|a| !a.trim().is_empty()) {
        body = format!(
            "{{\"peer_id\":{},\"addr\":{}}}",
            serde_json::to_string(peer_id).unwrap(),
            serde_json::to_string(a).unwrap()
        );
    }
    request("POST", "/api/v1/directs", Some(body)).await
}

/// 发送文件消息(data = base64 内容)。
pub async fn send_file(group_id: &str, name: &str, mime: &str, data: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        &format!("/api/v1/groups/{}/files", encode(group_id)),
        Some(format!(
            "{{\"name\":{},\"mime\":{},\"data\":{}}}",
            serde_json::to_string(name).unwrap(),
            serde_json::to_string(mime).unwrap(),
            serde_json::to_string(data).unwrap()
        )),
    )
    .await?;
    Ok(())
}

/// 下载文件消息内容(服务端同时落盘并标记已下载)。返回文件字节。
pub async fn download_file(msg_hash: &str) -> Result<Vec<u8>, ApiError> {
    let mut builder = Request::get(&format!("/api/v1/files/{}", encode(msg_hash)));
    if let Some(token) = get_token() {
        builder = builder.header("Authorization", &format!("Bearer {token}"));
    }
    let req = builder.build().map_err(|e| ApiError(0, e.to_string()))?;
    let resp = req.send().await.map_err(|e| ApiError(0, e.to_string()))?;
    let status = resp.status();
    if status == 401 {
        clear_token();
    }
    if !(200..300).contains(&status) {
        return Err(ApiError(status, "download failed".to_string()));
    }
    resp.binary()
        .await
        .map_err(|e| ApiError(status, e.to_string()))
}

pub async fn devices() -> Result<Vec<Device>, ApiError> {
    request("GET", "/api/v1/devices", None).await
}

pub async fn revoke_device(device_id: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        &format!("/api/v1/devices/{}/revoke", encode(device_id)),
        None,
    )
    .await?;
    Ok(())
}

pub async fn backup_mnemonic() -> Result<String, ApiError> {
    let v: serde_json::Value = request("GET", "/api/v1/backup/mnemonic", None).await?;
    v["mnemonic"]
        .as_str()
        .map(String::from)
        .ok_or(ApiError(0, "bad response".into()))
}

pub async fn restore(mnemonic: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        "/api/v1/restore",
        Some(format!(
            "{{\"mnemonic\":{}}}",
            serde_json::to_string(mnemonic).unwrap()
        )),
    )
    .await?;
    Ok(())
}

pub async fn transports() -> Result<TransportStatus, ApiError> {
    request("GET", "/api/v1/transports", None).await
}

pub async fn net_addr() -> Result<NetAddr, ApiError> {
    request("GET", "/api/v1/net/addr", None).await
}

pub async fn net_dial(addr: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        "/api/v1/net/dial",
        Some(format!(
            "{{\"addr\":{}}}",
            serde_json::to_string(addr).unwrap()
        )),
    )
    .await?;
    Ok(())
}

pub async fn save_settings(theme: &str, lang: &str) -> Result<(), ApiError> {
    let _: serde_json::Value = request(
        "POST",
        "/api/v1/settings",
        Some(format!(
            "{{\"ui_theme\":{},\"ui_language\":{}}}",
            serde_json::to_string(theme).unwrap(),
            serde_json::to_string(lang).unwrap()
        )),
    )
    .await?;
    Ok(())
}

fn encode(s: &str) -> String {
    // group_id / peer_id 均为 hex,无需百分号编码;保留简单实现
    s.to_string()
}

// ---------------------------------------------------------------------------
// WebSocket 事件流(自动重连,2s 退避)
// ---------------------------------------------------------------------------

/// 事件回调:type 字段 + 其余 JSON。
pub type EventHandler = Box<dyn FnMut(serde_json::Value)>;

/// Tauri(移动端)专用:经 `window.__TAURI_INTERNALS__.invoke("zoe_boot_token")`
/// 取内嵌守护进程的访问令牌。桌面浏览器无此环境,返回 None(走手动登录)。
pub async fn tauri_boot_token() -> Option<String> {
    let window = web_sys::window()?;
    let internals = js_sys::Reflect::get(
        &window,
        &wasm_bindgen::JsValue::from_str("__TAURI_INTERNALS__"),
    )
    .ok()?;
    if internals.is_undefined() || internals.is_null() {
        return None;
    }
    let invoke =
        js_sys::Reflect::get(&internals, &wasm_bindgen::JsValue::from_str("invoke")).ok()?;
    let f = js_sys::Function::from(invoke);
    let promise = f
        .call1(
            &internals,
            &wasm_bindgen::JsValue::from_str("zoe_boot_token"),
        )
        .ok()?;
    let val = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(promise))
        .await
        .ok()?;
    val.as_string()
}

pub fn connect_events(mut on_event: EventHandler) {
    spawn_local(async move {
        loop {
            let url = format!(
                "{proto}://{host}/api/v1/events?token={token}",
                proto = if web_sys::window()
                    .unwrap()
                    .location()
                    .protocol()
                    .unwrap_or_default()
                    == "https:"
                {
                    "wss"
                } else {
                    "ws"
                },
                host = web_sys::window()
                    .unwrap()
                    .location()
                    .host()
                    .unwrap_or_default(),
                token = get_token().unwrap_or_default(),
            );
            let Ok(mut ws) = WebSocket::open(&url) else {
                sleep_ms(2000).await;
                continue;
            };
            // 只收不维持心跳:服务端 60s 无活动断开,断线重连循环兜底
            while let Some(msg) = ws.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        on_event(v);
                    }
                }
            }
            sleep_ms(2000).await;
        }
    });
}

use futures_util::StreamExt;

/// wasm 环境的 sleep(setTimeout 封装)。
pub async fn sleep_ms(ms: u64) {
    gloo_timers::future::sleep(std::time::Duration::from_millis(ms)).await;
}
