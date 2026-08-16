//! HTTP/WS API(docs/api.md 的 M1 子集)+ 内嵌静态 UI。

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use qrcode::render::svg;
use qrcode::QrCode;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zoe_core::envelope::{Envelope, MSG_PRIVATE};
use zoe_core::mls::MlsSession;
use zoe_core::storage::StorageError;
use zoe_transport::Transport;

use crate::msg;
use crate::state::{ct_eq, now, SharedState};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

pub enum ApiError {
    NotFound(String),
    BadRequest(String),
    Unauthorized,
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (
            status,
            Json(json!({ "error": { "code": status.as_u16(), "message": message } })),
        )
            .into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(e.to_string())
}

// ---------------------------------------------------------------------------
// 认证中间件
// ---------------------------------------------------------------------------

async fn auth(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|a| a.strip_prefix("Bearer "))
        .map(|t| ct_eq(t.as_bytes(), state.token.as_bytes()))
        .unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        ApiError::Unauthorized.into_response()
    }
}

// ---------------------------------------------------------------------------
// 静态资源(内嵌 webui/dist)
// ---------------------------------------------------------------------------

const INDEX_HTML: &str = include_str!("../../../webui/dist/index.html");
const MAIN_JS: &str = include_str!("../../../webui/dist/main.js");
const API_JS: &str = include_str!("../../../webui/dist/api.js");
const I18N_JS: &str = include_str!("../../../webui/dist/i18n.js");
const ICONS_JS: &str = include_str!("../../../webui/dist/icons.js");
const THEME_JS: &str = include_str!("../../../webui/dist/theme.js");
const STYLES_CSS: &str = include_str!("../../../webui/dist/styles.css");

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn asset(Path(file): Path<String>) -> Result<Response, ApiError> {
    let (body, ctype) = match file.as_str() {
        "main.js" => (MAIN_JS, "text/javascript; charset=utf-8"),
        "api.js" => (API_JS, "text/javascript; charset=utf-8"),
        "i18n.js" => (I18N_JS, "text/javascript; charset=utf-8"),
        "icons.js" => (ICONS_JS, "text/javascript; charset=utf-8"),
        "theme.js" => (THEME_JS, "text/javascript; charset=utf-8"),
        "styles.css" => (STYLES_CSS, "text/css; charset=utf-8"),
        _ => return Err(ApiError::NotFound("asset".to_string())),
    };
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, ctype)],
        Body::from(body),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// 登录与身份
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LoginReq {
    token: String,
}

async fn login(State(state): State<SharedState>, Json(req): Json<LoginReq>) -> Response {
    if ct_eq(req.token.as_bytes(), state.token.as_bytes()) {
        Json(json!({ "ok": true })).into_response()
    } else {
        ApiError::Unauthorized.into_response()
    }
}

async fn me(State(state): State<SharedState>) -> ApiResult {
    let (_, created_at) = state
        .storage
        .identity()
        .map_err(internal)?
        .ok_or_else(|| ApiError::Internal("no identity".to_string()))?;
    Ok(Json(json!({
        "user_id": hex::encode(state.identity.verifying_key().to_bytes()),
        "fingerprint": hex::encode(state.identity.fingerprint()),
        "created_at": created_at,
        "device": {
            "name": state.mls_identity.name(),
            "signature_public_key": hex::encode(state.mls_identity.signature_public_key()),
        },
        "started_at": state.started_at,
    })))
}

async fn card(State(state): State<SharedState>) -> ApiResult {
    let fingerprint = hex::encode(state.identity.fingerprint());
    let peer_id = hex::encode(state.identity.verifying_key().to_bytes());
    let text = format!("zoe://peer/{peer_id}/{fingerprint}");
    let code = QrCode::new(text.as_bytes()).map_err(|e| ApiError::Internal(e.to_string()))?;
    let qr_svg = code.render::<svg::Color>().min_dimensions(4, 4).build();
    Ok(Json(json!({
        "peer_id": peer_id,
        "fingerprint": fingerprint,
        "qr_svg": qr_svg,
    })))
}

#[derive(Deserialize)]
struct ImportReq {
    text: String,
}

async fn import_card(State(state): State<SharedState>, Json(req): Json<ImportReq>) -> ApiResult {
    let text = req.text.trim();
    let rest = text
        .strip_prefix("zoe://peer/")
        .ok_or_else(|| ApiError::BadRequest("invalid card format".to_string()))?;
    let mut parts = rest.split('/');
    let peer_id_hex = parts.next().unwrap_or_default();
    let fp_hex = parts.next().unwrap_or_default();
    let peer_id =
        hex::decode(peer_id_hex).map_err(|_| ApiError::BadRequest("bad peer id".to_string()))?;
    let fp =
        hex::decode(fp_hex).map_err(|_| ApiError::BadRequest("bad fingerprint".to_string()))?;
    let display = format!("peer-{}", &peer_id_hex[..peer_id_hex.len().min(8)]);
    state
        .storage
        .upsert_peer(&peer_id, &fp, &display, 0, now())
        .map_err(internal)?;
    Ok(Json(json!({ "ok": true, "peer_id": peer_id_hex })))
}

async fn peers_list(State(state): State<SharedState>) -> ApiResult {
    let peers = state.storage.peers().map_err(internal)?;
    Ok(Json(json!(peers
        .iter()
        .map(|p| {
            json!({
                "peer_id": hex::encode(&p.peer_id),
                "fingerprint": hex::encode(&p.fingerprint),
                "display_name": p.display_name,
                "trust_status": p.trust_status,
            })
        })
        .collect::<Vec<_>>())))
}

// ---------------------------------------------------------------------------
// 群组与消息
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateGroupReq {
    name: String,
}

async fn groups_list(State(state): State<SharedState>) -> ApiResult {
    let records = state.storage.groups().map_err(internal)?;
    let sessions = state.sessions.lock().unwrap();
    let mut out = Vec::new();
    for g in &records {
        let members = sessions
            .get(&g.group_id)
            .map(|s| s.members())
            .unwrap_or_default();
        out.push(json!({
            "group_id": hex::encode(&g.group_id),
            "name": g.name,
            "epoch": g.epoch,
            "coordinator": g.coordinator.as_ref().map(hex::encode),
            "members": members,
            "created_at": g.created_at,
        }));
    }
    Ok(Json(json!(out)))
}

async fn create_group(
    State(state): State<SharedState>,
    Json(req): Json<CreateGroupReq>,
) -> ApiResult {
    let name = req.name.trim().to_string();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::BadRequest(
            "name must be 1..=64 chars".to_string(),
        ));
    }
    let mut gid = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut gid);
    {
        let provider = state.provider.lock().unwrap();
        let session =
            MlsSession::create_group(&*provider, &state.mls_identity, &gid).map_err(internal)?;
        let epoch = session.epoch();
        let members = session.members();
        state.sessions.lock().unwrap().insert(gid.to_vec(), session);
        state
            .storage
            .create_group(&gid, &name, epoch, None, now())
            .map_err(internal)?;
        let _ = state
            .events
            .send(json!({"type":"group","event":"created"}).to_string());
        Ok(Json(json!({
            "group_id": hex::encode(gid),
            "name": name,
            "epoch": epoch,
            "members": members,
        })))
    }
}

fn decode_group_id(s: &str) -> Result<Vec<u8>, ApiError> {
    hex::decode(s).map_err(|_| ApiError::NotFound("group".to_string()))
}

#[derive(Deserialize)]
struct ListMessagesQuery {
    limit: Option<u32>,
    before: Option<i64>,
}

async fn list_messages(
    State(state): State<SharedState>,
    Path(gid_hex): Path<String>,
    Query(q): Query<ListMessagesQuery>,
) -> ApiResult {
    let gid = decode_group_id(&gid_hex)?;
    let limit = q.limit.unwrap_or(100).min(500);
    let msgs = state
        .storage
        .messages(&gid, limit, q.before)
        .map_err(internal)?;
    Ok(Json(json!(msgs
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "seq": m.seq,
                "direction": m.direction,
                "status": m.status,
                "text": m.plaintext.as_ref().map(|p| String::from_utf8_lossy(p).to_string()),
                "received_at": m.received_at,
            })
        })
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct SendReq {
    text: String,
}

async fn send_message(
    State(state): State<SharedState>,
    Path(gid_hex): Path<String>,
    Json(req): Json<SendReq>,
) -> ApiResult {
    let text = req.text.trim().to_string();
    if text.is_empty() || text.len() > 4096 {
        return Err(ApiError::BadRequest(
            "text must be 1..=4096 chars".to_string(),
        ));
    }
    let gid = decode_group_id(&gid_hex)?;

    let (hash, env_bytes, env, epoch, seq) = {
        let mut sessions = state.sessions.lock().unwrap();
        let session = sessions
            .get_mut(&gid)
            .ok_or_else(|| ApiError::NotFound("group session".to_string()))?;
        let provider = state.provider.lock().unwrap();
        let ct = session
            .encrypt(&*provider, &state.mls_identity, text.as_bytes())
            .map_err(internal)?;
        let epoch = session.epoch();
        drop(provider);

        // seq = 该群组最后一条 seq + 1
        let last = state
            .storage
            .messages(&gid, 1, None)
            .map_err(internal)?
            .into_iter()
            .filter_map(|m| m.seq)
            .next_back()
            .unwrap_or(0);
        let seq = last + 1;
        let env = Envelope::new(0, MSG_PRIVATE, gid.clone(), epoch as u32, 0, seq, ct);
        let hash = env.hash;
        let env_bytes = env.encode();
        (hash, env_bytes, env, epoch, seq)
    };

    state
        .storage
        .insert_message(
            &hash,
            &env_bytes,
            &gid,
            epoch,
            None,
            Some(seq),
            1,
            0,
            Some(text.as_bytes()),
            now(),
        )
        .map_err(internal)?;
    state
        .storage
        .update_group_epoch(&gid, epoch)
        .map_err(internal)?;
    let _ = state
        .events
        .send(json!({"type":"message","group_id":gid_hex}).to_string());

    // 经所有可用传输投递(当前:libp2p 广播到已连接 peer)
    msg::broadcast_envelope(&state, &env).await;

    Ok(Json(json!({ "id": hex::encode(hash) })))
}

// ---------------------------------------------------------------------------
// 设备、备份、设置、传输
// ---------------------------------------------------------------------------

async fn devices(State(state): State<SharedState>) -> ApiResult {
    Ok(Json(json!([
        {
            "device_id": hex::encode(state.mls_identity.signature_public_key()),
            "name": state.mls_identity.name(),
            "revoked": false,
        }
    ])))
}

async fn backup_mnemonic(State(state): State<SharedState>) -> ApiResult {
    Ok(Json(json!({ "mnemonic": state.identity.to_mnemonic() })))
}

#[derive(Deserialize)]
struct RestoreReq {
    mnemonic: String,
}

async fn restore(State(state): State<SharedState>, Json(req): Json<RestoreReq>) -> ApiResult {
    let id = zoe_core::identity::IdentityKeyPair::from_mnemonic(req.mnemonic.trim())
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    state
        .storage
        .set_identity(&id.seed(), now())
        .map_err(internal)?;
    // 注意:设备 MLS 凭据派生自旧身份种子,恢复后重启即与旧群组脱离;
    // 需重新扫码授权/被邀请(见 docs/protocol.md §5)。
    Ok(Json(
        json!({ "ok": true, "note": "identity restored; re-invite devices to groups" }),
    ))
}

#[derive(Deserialize, Default)]
struct SettingsReq {
    ui_theme: Option<String>,
    ui_language: Option<String>,
}

async fn get_settings(State(state): State<SharedState>) -> ApiResult {
    let theme = state.storage.get_meta("ui_theme").map_err(internal)?;
    let lang = state.storage.get_meta("ui_language").map_err(internal)?;
    Ok(Json(json!({ "ui_theme": theme, "ui_language": lang })))
}

async fn post_settings(
    State(state): State<SharedState>,
    Json(req): Json<SettingsReq>,
) -> ApiResult {
    if let Some(t) = req.ui_theme {
        if matches!(t.as_str(), "light" | "dark" | "system") {
            state.storage.set_meta("ui_theme", &t).map_err(internal)?;
        }
    }
    if let Some(l) = req.ui_language {
        if matches!(l.as_str(), "zh-CN" | "en-US") {
            state
                .storage
                .set_meta("ui_language", &l)
                .map_err(internal)?;
        }
    }
    get_settings(State(state)).await
}

async fn transports(State(state): State<SharedState>) -> ApiResult {
    // M1:loopback;M3:net(libp2p);BLE 驱动在 M2(平台相关)
    let net_up = state.net.is_some();
    Ok(Json(json!({
        "ble": "down",
        "lan": "down",
        "net": if net_up { "up" } else { "down" },
        "loopback": "up",
        "net_peers": state.net.as_ref().map(|n| n.peers().len()).unwrap_or(0),
    })))
}

// ---------------------------------------------------------------------------
// 网络(M3):地址查询、拨号、邀请
// ---------------------------------------------------------------------------

async fn net_addr(State(state): State<SharedState>) -> ApiResult {
    match &state.net {
        Some(net) => {
            let pid = net.local_peer_id().to_string();
            // 每个监听地址附上 /p2p/<peer-id>,供邀请流程解析目标身份
            let addrs: Vec<String> = net
                .listen_addrs()
                .iter()
                .map(|a| format!("{a}/p2p/{pid}"))
                .collect();
            Ok(Json(json!({
                "peer_id": pid,
                "listen_addrs": addrs,
            })))
        }
        None => Err(ApiError::NotFound(
            "net transport not available".to_string(),
        )),
    }
}

#[derive(Deserialize)]
struct DialReq {
    addr: String,
}

async fn net_dial(State(state): State<SharedState>, Json(req): Json<DialReq>) -> ApiResult {
    let net = state
        .net
        .clone()
        .ok_or_else(|| ApiError::NotFound("net transport not available".to_string()))?;
    net.dial(&req.addr).await.map_err(internal)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct InviteReq {
    addr: String,
}

async fn invite(
    State(state): State<SharedState>,
    Path(gid_hex): Path<String>,
    Json(req): Json<InviteReq>,
) -> ApiResult {
    let gid = decode_group_id(&gid_hex)?;
    let result = msg::invite_peer(&state, &gid, &req.addr)
        .await
        .map_err(ApiError::BadRequest)?;
    Ok(Json(result))
}

// ---------------------------------------------------------------------------
// WebSocket 事件流
// ---------------------------------------------------------------------------

async fn events_ws(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let token = params.get("token").map(String::as_str).unwrap_or("");
    if !ct_eq(token.as_bytes(), state.token.as_bytes()) {
        return ApiError::Unauthorized.into_response();
    }
    ws.on_upgrade(move |socket| ws_loop(socket, state))
}

async fn ws_loop(mut socket: WebSocket, state: SharedState) {
    let mut rx = state.events.subscribe();
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(t))) if t.as_str() == "ping" => {
                        if socket.send(Message::Text(Utf8Bytes::from_static("pong"))).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(e) => {
                        if socket.send(Message::Text(Utf8Bytes::from(e))).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

pub fn router(state: SharedState) -> Router {
    let protected = Router::new()
        .route("/me", get(me))
        .route("/card", get(card))
        .route("/card/import", post(import_card))
        .route("/peers", get(peers_list))
        .route("/groups", get(groups_list).post(create_group))
        .route(
            "/groups/{id}/messages",
            get(list_messages).post(send_message),
        )
        .route("/groups/{id}/invite", post(invite))
        .route("/net/addr", get(net_addr))
        .route("/net/dial", post(net_dial))
        .route("/devices", get(devices))
        .route("/backup/mnemonic", get(backup_mnemonic))
        .route("/restore", post(restore))
        .route("/settings", get(get_settings).post(post_settings))
        .route("/transports", get(transports))
        .route("/events", get(events_ws))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state.clone());

    Router::new()
        .route("/", get(index))
        .route("/assets/{file}", get(asset))
        .route("/api/v1/login", post(login))
        .nest("/api/v1", protected)
        .with_state(state)
}

// 供 main 引用,避免未用告警
#[allow(dead_code)]
fn _storage_error_to_api(e: StorageError) -> ApiError {
    ApiError::Internal(e.to_string())
}

#[allow(dead_code)]
fn _sha256_placeholder(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}
