//! HTTP/WS API(docs/api.md 全量契约)+ 内嵌静态 UI。

use axum::body::Body;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use qrcode::render::svg;
use qrcode::QrCode;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zoe_core::content;
use zoe_core::envelope::{Envelope, MSG_PRIVATE};
use zoe_core::mls::MlsSession;
use zoe_core::storage::StorageError;
use zoe_transport::Transport;

use crate::msg;
use crate::state::{self, now, SharedState};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

pub enum ApiError {
    NotFound(String),
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
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
// 锁定门:未解锁时除用户注册表/解锁外一律 423。
// ---------------------------------------------------------------------------

async fn locked_gate(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let allowed =
        path == "/users" || path == "/unlock" || path == "/status" || path.starts_with("/users/");
    if state::is_unlocked(&state) || allowed {
        next.run(req).await
    } else {
        (
            StatusCode::LOCKED,
            Json(json!({
                "error": { "code": 423, "message": "locked: enter PIN via POST /api/v1/unlock" }
            })),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// 静态资源(内嵌 webui/dist —— Rust/Leptos wasm 产物)
// ---------------------------------------------------------------------------

const INDEX_HTML: &str = include_str!("../../../webui/dist/index.html");
const STYLES_CSS: &str = include_str!("../../../webui/dist/styles.css");
const ZOE_WEBUI_JS: &str = include_str!("../../../webui/dist/assets/zoe_webui.js");
const ZOE_WEBUI_WASM: &[u8] = include_bytes!("../../../webui/dist/assets/zoe_webui_bg.wasm");

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn asset(Path(file): Path<String>) -> Result<Response, ApiError> {
    use std::borrow::Cow;
    let (body, ctype): (Cow<'static, [u8]>, &str) = match file.as_str() {
        "styles.css" => (
            Cow::Borrowed(STYLES_CSS.as_bytes()),
            "text/css; charset=utf-8",
        ),
        "zoe_webui.js" => (
            Cow::Borrowed(ZOE_WEBUI_JS.as_bytes()),
            "text/javascript; charset=utf-8",
        ),
        "zoe_webui_bg.wasm" => (Cow::Borrowed(ZOE_WEBUI_WASM), "application/wasm"),
        _ => return Err(ApiError::NotFound("asset".to_string())),
    };
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, ctype)],
        Body::from(body.into_owned()),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// 用户注册表(docs/api.md §1.1;PIN 保护 + 锁定模式 + 切换)
// ---------------------------------------------------------------------------

fn user_json(u: &zoe_core::users::User) -> Value {
    json!({
        "user_id": hex::encode(u.user_id),
        "name": u.name,
        "kind": u.kind.as_str(),
        "created_at": u.created_at,
        "last_used": u.last_used,
    })
}

/// 列出注册表全部用户 + 活跃用户与解锁状态(锁定模式下允许的最小读接口)。
async fn users_list(State(state): State<SharedState>) -> ApiResult {
    let users = state
        .registry
        .list()
        .map_err(internal)?
        .iter()
        .map(user_json)
        .collect::<Vec<_>>();
    let active = state::active_user(&state);
    Ok(Json(json!({
        "unlocked": state::is_unlocked(&state),
        "active": user_json(&active),
        "users": users,
        "can_switch": !state.mobile,
    })))
}

/// 切换活跃用户(仅桌面 CLI):更新 `last_used` 并自重启,重启后按最近使用激活。
/// 移动端禁用(由宿主应用管理用户生命周期)。
async fn activate_user(State(state): State<SharedState>, Path(uid_hex): Path<String>) -> ApiResult {
    if state.mobile {
        return Err(ApiError::BadRequest(
            "user switching disabled on mobile".to_string(),
        ));
    }
    let uid = hex::decode(&uid_hex).map_err(|_| ApiError::NotFound("user".to_string()))?;
    if uid == state::active_user(&state).user_id {
        return Ok(Json(json!({ "ok": true, "switched": false })));
    }
    state.registry.set_last_used(&uid).map_err(internal)?;
    let _ = state.events.send(
        json!({
            "type": "user",
            "event": "switched",
            "user_id": hex::encode(&uid),
        })
        .to_string(),
    );
    crate::relaunch(&state.data_dir, &uid_hex);
    Ok(Json(json!({
        "ok": true,
        "switched": true,
        "note": "daemon restarting with the new active user",
    })))
}

#[derive(Deserialize)]
struct CreateUserReq {
    name: String,
    pin: String,
}

/// 创建新 PIN 用户:独立数据目录 users/<id> + 加密种子 + argon2id 校验串。
/// 新用户不会自动激活(激活 = 重启 daemon 时指定 --user)。
async fn create_user(
    State(state): State<SharedState>,
    Json(req): Json<CreateUserReq>,
) -> ApiResult {
    let name = req.name.trim().to_string();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::BadRequest(
            "name must be 1..=64 chars".to_string(),
        ));
    }
    let exists = state
        .registry
        .list()
        .map_err(internal)?
        .iter()
        .any(|u| u.name == name);
    if exists {
        return Err(ApiError::BadRequest(
            "a user with this name already exists".to_string(),
        ));
    }
    let id = zoe_core::identity::IdentityKeyPair::generate();
    let user = state
        .registry
        .add_pin_user(&name, &req.pin, &id.seed())
        .map_err(internal)?;
    let _ = state.events.send(
        json!({
            "type": "user",
            "event": "created",
            "user_id": hex::encode(user.user_id),
        })
        .to_string(),
    );
    Ok(Json(json!({
        "ok": true,
        "user": user_json(&user),
        "note": "activate by POST /users/{id}/activate (daemon self-restart)",
    })))
}

#[derive(Deserialize)]
struct UnlockReq {
    pin: String,
}

/// 解锁 PIN 用户(锁定模式唯一出口;成功则恢复身份/MLS/net)。
async fn unlock(State(state): State<SharedState>, Json(req): Json<UnlockReq>) -> ApiResult {
    let user = crate::unlock(&state, &req.pin).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(Json(json!({
        "ok": true,
        "user": user_json(&user),
        "unlocked": true,
    })))
}

#[derive(Deserialize)]
struct SetPinReq {
    pin: String,
}

/// 设置/更换活跃用户的 PIN(需已解锁;统一走注册表重新加密种子)。
async fn set_pin(
    State(state): State<SharedState>,
    Path(uid_hex): Path<String>,
    Json(req): Json<SetPinReq>,
) -> ApiResult {
    if !state::is_unlocked(&state) {
        return Err(ApiError::BadRequest(
            "locked: unlock before setting a PIN".to_string(),
        ));
    }
    let active = state::active_user(&state);
    let uid = hex::decode(&uid_hex).map_err(|_| ApiError::NotFound("user".to_string()))?;
    if uid != active.user_id {
        return Err(ApiError::BadRequest(
            "PIN can only be set for the active user in v1 (switch users by restart)".to_string(),
        ));
    }
    let seed = {
        let id = state::identity(&state)
            .ok_or_else(|| ApiError::Internal("identity unavailable".to_string()))?;
        id.seed()
    };
    state
        .registry
        .set_pin(&active.user_id, &req.pin, &seed)
        .map_err(internal)?;
    // 种子已迁入注册表 users.db;用户 zoe.db 仅留标记位(明文种子不再写入磁盘)
    let _ = state.storage.set_meta("seed_enc", "1");
    let _ = state
        .events
        .send(json!({"type":"user","event":"pin_set","user_id":uid_hex}).to_string());
    Ok(Json(json!({
        "ok": true,
        "note": "PIN set; the daemon will require it after restart (locked mode until /unlock)",
    })))
}

async fn me(State(state): State<SharedState>) -> ApiResult {
    let active = state::active_user(&state);
    let identity = state::identity(&state)
        .ok_or_else(|| ApiError::Internal("locked: identity unavailable".to_string()))?;
    let mls_identity = state::mls_identity(&state)
        .ok_or_else(|| ApiError::Internal("locked: mls unavailable".to_string()))?;
    let devices: Vec<Value> = state
        .storage
        .devices()
        .map_err(internal)?
        .iter()
        .map(|d| {
            json!({
                "device_id": hex::encode(&d.device_id),
                "revoked": d.revoked_at.is_some(),
                "created_at": d.created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "user_id": hex::encode(identity.verifying_key().to_bytes()),
        "fingerprint": hex::encode(identity.fingerprint()),
        "created_at": active.created_at,
        "device": {
            "name": mls_identity.name(),
            "signature_public_key": hex::encode(mls_identity.signature_public_key()),
        },
        "devices": devices,
        "started_at": state.started_at,
    })))
}

async fn card(State(state): State<SharedState>) -> ApiResult {
    let identity = state::identity(&state)
        .ok_or_else(|| ApiError::Internal("locked: identity unavailable".to_string()))?;
    let fingerprint = hex::encode(identity.fingerprint());
    let peer_id = hex::encode(identity.verifying_key().to_bytes());
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
                "first_seen": p.first_seen,
                "last_seen": p.last_seen,
            })
        })
        .collect::<Vec<_>>())))
}

fn decode_peer_id(s: &str) -> Result<Vec<u8>, ApiError> {
    hex::decode(s).map_err(|_| ApiError::NotFound("peer".to_string()))
}

/// 阻止对端(trust_status=2;协议层同时应停止与其的密钥交换)。
async fn block_peer(State(state): State<SharedState>, Path(pid_hex): Path<String>) -> ApiResult {
    let pid = decode_peer_id(&pid_hex)?;
    state.storage.set_peer_trust(&pid, 2).map_err(internal)?;
    let _ = state
        .events
        .send(json!({"type":"peer","peer_id":pid_hex,"state":"blocked"}).to_string());
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// 配对(protocol.md §1):配对模式状态机 + 带外验证
// ---------------------------------------------------------------------------

async fn pair_start(State(state): State<SharedState>) -> ApiResult {
    let mut code = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut code);
    state
        .pairing
        .store(true, std::sync::atomic::Ordering::SeqCst);
    *state.pair_code.lock().unwrap() = Some(code);
    let _ = state
        .events
        .send(json!({"type":"peer","peer_id":"","state":"pairing"}).to_string());
    // BLE 广告由传输驱动接管(守护进程当前未挂载 BLE 驱动,如实上报)
    Ok(Json(json!({
        "pair_code": hex::encode(code),
        "bt_advertising": false,
    })))
}

async fn pair_stop(State(state): State<SharedState>) -> ApiResult {
    state
        .pairing
        .store(false, std::sync::atomic::Ordering::SeqCst);
    *state.pair_code.lock().unwrap() = None;
    let _ = state
        .events
        .send(json!({"type":"peer","peer_id":"","state":"pairing_stopped"}).to_string());
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PairVerifyReq {
    peer_id: String,
    ok: bool,
}

/// 带外验证(指纹比对结果):ok=true → trust=1;ok=false → 保持 TOFU 并拒绝。
async fn pair_verify(
    State(state): State<SharedState>,
    Json(req): Json<PairVerifyReq>,
) -> ApiResult {
    let pid = decode_peer_id(&req.peer_id)?;
    if req.ok {
        state.storage.set_peer_trust(&pid, 1).map_err(internal)?;
        let _ = state
            .events
            .send(json!({"type":"peer","peer_id":req.peer_id,"state":"verified"}).to_string());
        Ok(Json(json!({ "ok": true, "trust_status": 1 })))
    } else {
        // 指纹不匹配:失败安全 —— 维持未信任,不置阻止(用户可另行阻止)
        Ok(Json(json!({ "ok": false, "trust_status": 0 })))
    }
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
        // 私聊:解析对端 zoe peer id(经联系人表 net_peer_id 匹配)与展示名
        let direct = g.direct_peer.is_some();
        let direct_peer_id = g
            .direct_peer
            .as_ref()
            .and_then(|p| peer_id_by_net(&state, p));
        let direct_name = if direct {
            direct_peer_id
                .as_ref()
                .and_then(|pid| {
                    state.storage.peers().ok().and_then(|ps| {
                        ps.into_iter()
                            .find(|p| &p.peer_id == pid)
                            .and_then(|p| p.display_name)
                    })
                })
                .or_else(|| g.name.clone())
        } else {
            None
        };
        out.push(json!({
            "group_id": hex::encode(&g.group_id),
            "name": g.name,
            "epoch": g.epoch,
            "coordinator": g.coordinator.as_ref().map(hex::encode),
            "members": members,
            "created_at": g.created_at,
            "direct": direct,
            "direct_peer": g.direct_peer.as_ref().map(|p| String::from_utf8_lossy(p).to_string()),
            "direct_peer_id": direct_peer_id.as_ref().map(hex::encode),
            "direct_name": direct_name,
        }));
    }
    Ok(Json(json!(out)))
}

/// 由 libp2p peer id 反查联系人表中登记的 zoe peer id(net_peer_id 匹配)。
fn peer_id_by_net(state: &SharedState, net_peer_id: &[u8]) -> Option<Vec<u8>> {
    let net_id = String::from_utf8_lossy(net_peer_id);
    state
        .storage
        .peers()
        .ok()?
        .into_iter()
        .find(|p| p.net_peer_id.as_deref() == Some(net_id.as_ref()))
        .map(|p| p.peer_id)
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
        let mls = state::mls_identity(&state)
            .ok_or_else(|| ApiError::Internal("locked: mls unavailable".to_string()))?;
        let provider = state.provider.lock().unwrap();
        let session = MlsSession::create_group(&*provider, &mls, &gid).map_err(internal)?;
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

// ---------------------------------------------------------------------------
// 单聊(与联系人的私聊 = 双人 MLS 群)
// ---------------------------------------------------------------------------

/// 既有私聊群列表(经 net_peer_id 匹配到联系人)。
async fn directs_list(State(state): State<SharedState>) -> ApiResult {
    let groups = state.storage.groups().map_err(internal)?;
    let mut out = Vec::new();
    for g in groups.into_iter().filter(|g| g.direct_peer.is_some()) {
        let net_id = String::from_utf8_lossy(g.direct_peer.as_deref().unwrap_or_default());
        let peer_id = state
            .storage
            .peers()
            .map_err(internal)?
            .into_iter()
            .find(|p| p.net_peer_id.as_deref() == Some(net_id.as_ref()))
            .map(|p| hex::encode(&p.peer_id));
        out.push(json!({
            "group_id": hex::encode(&g.group_id),
            "peer_id": peer_id,
            "name": g.name,
            "epoch": g.epoch,
            "created_at": g.created_at,
        }));
    }
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
struct DirectReq {
    peer_id: String,
    #[serde(default)]
    addr: Option<String>,
}

/// 发起与联系人的单聊(可带可选地址;已有私聊则复用)。
async fn create_direct(State(state): State<SharedState>, Json(req): Json<DirectReq>) -> ApiResult {
    let addr = req.addr.as_deref().map(str::trim).filter(|a| !a.is_empty());
    let result = msg::start_direct(&state, &req.peer_id, addr)
        .await
        .map_err(ApiError::BadRequest)?;
    Ok(Json(result))
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
            let (text, file) = match m.plaintext.as_deref() {
                Some(pt) => parse_plaintext(pt),
                None => (None, None),
            };
            json!({
                "id": m.id,
                "msg_hash": hex::encode(&m.msg_hash),
                "seq": m.seq,
                "direction": m.direction,
                "status": m.status,
                "text": text,
                "file": file,
                "file_downloaded": m.file_downloaded == 1,
                "received_at": m.received_at,
            })
        })
        .collect::<Vec<_>>())))
}

/// 解析消息明文:结构化文件消息 → file;其余(含旧版文本)按 UTF-8 文本返回。
fn parse_plaintext(pt: &[u8]) -> (Option<String>, Option<Value>) {
    if let Some(f) = content::decode_file(pt) {
        (
            None,
            Some(json!({
                "name": f.name,
                "mime": f.mime,
                "size": f.size,
            })),
        )
    } else {
        (Some(String::from_utf8_lossy(pt).to_string()), None)
    }
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
    let hash = send_plaintext(&state, &gid, text.as_bytes().to_vec()).await?;
    Ok(Json(json!({ "id": hex::encode(hash) })))
}

/// 加密 + 落库 + 投递的公共核心(文本与文件消息共用),返回消息哈希。
async fn send_plaintext(
    state: &SharedState,
    gid: &[u8],
    plaintext: Vec<u8>,
) -> Result<[u8; 32], ApiError> {
    let (hash, env_bytes, env, epoch, seq) = {
        let mls = state::mls_identity(state)
            .ok_or_else(|| ApiError::Internal("locked: mls unavailable".to_string()))?;
        let mut sessions = state.sessions.lock().unwrap();
        let session = sessions
            .get_mut(gid)
            .ok_or_else(|| ApiError::NotFound("group session".to_string()))?;
        let provider = state.provider.lock().unwrap();
        let ct = session
            .encrypt(&*provider, &mls, &plaintext)
            .map_err(internal)?;
        let epoch = session.epoch();
        drop(provider);

        // seq = 该群组最后一条 seq + 1
        let last = state
            .storage
            .messages(gid, 1, None)
            .map_err(internal)?
            .into_iter()
            .filter_map(|m| m.seq)
            .next_back()
            .unwrap_or(0);
        let seq = last + 1;
        let env = Envelope::new(0, MSG_PRIVATE, gid.to_vec(), epoch as u32, 0, seq, ct);
        let hash = env.hash;
        let env_bytes = env.encode();
        (hash, env_bytes, env, epoch, seq)
    };

    state
        .storage
        .insert_message(
            &hash,
            &env_bytes,
            gid,
            epoch,
            None,
            Some(seq),
            1,
            0,
            Some(&plaintext),
            now(),
        )
        .map_err(internal)?;
    state
        .storage
        .update_group_epoch(gid, epoch)
        .map_err(internal)?;
    let _ = state
        .events
        .send(json!({"type":"message","group_id":hex::encode(gid)}).to_string());

    // 投递:私聊定向 / 群聊广播
    let _ = msg::deliver_envelope(state, &env).await;

    Ok(hash)
}

// ---------------------------------------------------------------------------
// 文件消息:发送(小文件自动下载)/ 下载
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SendFileReq {
    name: String,
    #[serde(default)]
    mime: Option<String>,
    /// base64 编码的文件内容。
    data: String,
}

async fn send_file(
    State(state): State<SharedState>,
    Path(gid_hex): Path<String>,
    Json(req): Json<SendFileReq>,
) -> ApiResult {
    let name = req.name.trim().to_string();
    if name.is_empty() || name.len() > 255 {
        return Err(ApiError::BadRequest(
            "name must be 1..=255 chars".to_string(),
        ));
    }
    let mime = req
        .mime
        .unwrap_or_default()
        .trim()
        .chars()
        .take(128)
        .collect::<String>();
    let mime = if mime.is_empty() {
        "application/octet-stream".to_string()
    } else {
        mime
    };
    let data = base64::engine::general_purpose::STANDARD
        .decode(req.data.as_bytes())
        .map_err(|_| ApiError::BadRequest("bad base64 data".to_string()))?;
    if data.is_empty() {
        return Err(ApiError::BadRequest("empty file".to_string()));
    }
    let plaintext = content::encode_file(&name, &mime, &data)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let gid = decode_group_id(&gid_hex)?;
    let hash = send_plaintext(&state, &gid, plaintext).await?;

    // 发出方同样自动落盘小文件(本地留档,幂等)
    if data.len() <= content::FILE_AUTO_MAX {
        let f = content::FileContent {
            name,
            mime,
            size: data.len() as u64,
            data,
        };
        if msg::persist_file(&state, &hash, &f) {
            let _ = state.storage.mark_message_downloaded(&hash);
        }
    }
    Ok(Json(json!({ "id": hex::encode(hash) })))
}

/// 下载文件消息内容:从密文库中取回解密明文并流式返回;
/// 同时落盘到 files/ 目录并标记已下载(幂等)。
async fn get_file(
    State(state): State<SharedState>,
    Path(hash_hex): Path<String>,
) -> Result<Response, ApiError> {
    let hash = hex::decode(&hash_hex).map_err(|_| ApiError::NotFound("file".to_string()))?;
    let m = state
        .storage
        .message_by_hash(&hash)
        .map_err(internal)?
        .ok_or_else(|| ApiError::NotFound("file".to_string()))?;
    let pt = m
        .plaintext
        .ok_or_else(|| ApiError::NotFound("file".to_string()))?;
    let f = content::decode_file(&pt).ok_or_else(|| ApiError::NotFound("file".to_string()))?;

    if msg::persist_file(&state, &hash, &f) {
        let _ = state.storage.mark_message_downloaded(&hash);
        let _ = state
            .events
            .send(json!({"type":"message","group_id":hex::encode(&m.group_id)}).to_string());
    }

    let content_type = if f.mime.len() <= 128 && f.mime.bytes().all(|b| b.is_ascii()) {
        f.mime.clone()
    } else {
        "application/octet-stream".to_string()
    };
    let filename = percent_encode_header(&f.name);
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename*=UTF-8''{filename}"),
            ),
        ],
        Body::from(f.data),
    )
        .into_response())
}

/// RFC 5987 filename* 百分号编码(unreserved 之外全部编码)。
fn percent_encode_header(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 设备、备份、设置、传输
// ---------------------------------------------------------------------------

async fn devices(State(state): State<SharedState>) -> ApiResult {
    let rows = state.storage.devices().map_err(internal)?;
    let mls_identity = state::mls_identity(&state)
        .ok_or_else(|| ApiError::Internal("locked: mls unavailable".to_string()))?;
    Ok(Json(json!(rows
        .iter()
        .map(|d| {
            json!({
                "device_id": hex::encode(&d.device_id),
                "name": if d.device_id == mls_identity.signature_public_key() {
                    mls_identity.name().to_string()
                } else {
                    format!("device-{}", &hex::encode(&d.device_id)[..8])
                },
                "revoked": d.revoked_at.is_some(),
                "created_at": d.created_at,
            })
        })
        .collect::<Vec<_>>())))
}

/// 吊销设备(本地台账)。群组内的完整吊销需经 MLS Proposal(remove)(protocol.md §5)。
async fn revoke_device(State(state): State<SharedState>, Path(did_hex): Path<String>) -> ApiResult {
    let did = hex::decode(&did_hex).map_err(|_| ApiError::NotFound("device".to_string()))?;
    state.storage.revoke_device(&did, now()).map_err(internal)?;
    let _ = state
        .events
        .send(json!({"type":"group","event":"device_revoked","device_id":did_hex}).to_string());
    Ok(Json(json!({ "ok": true })))
}

/// 本地退出群组:删除会话与本地存储。完整 MLS 移除需协调者
/// Proposal(remove)(协议文档 §3),v1 退群 = 本地离开。
async fn leave_group(State(state): State<SharedState>, Path(gid_hex): Path<String>) -> ApiResult {
    let gid = decode_group_id(&gid_hex)?;
    let mut sessions = state.sessions.lock().unwrap();
    match sessions.remove(&gid) {
        Some(_) => {}
        None => {
            return Err(ApiError::NotFound("group session".to_string()));
        }
    }
    state.storage.delete_group(&gid).map_err(internal)?;
    let _ = state
        .events
        .send(json!({"type":"group","event":"left","group_id":gid_hex}).to_string());
    Ok(Json(
        json!({ "ok": true, "note": "local leave; other members unaffected (see protocol §3)" }),
    ))
}

async fn backup_mnemonic(State(state): State<SharedState>) -> ApiResult {
    let identity = state::identity(&state)
        .ok_or_else(|| ApiError::Internal("locked: identity unavailable".to_string()))?;
    Ok(Json(json!({ "mnemonic": identity.to_mnemonic() })))
}

#[derive(Deserialize)]
struct RestoreReq {
    mnemonic: String,
}

async fn restore(State(state): State<SharedState>, Json(req): Json<RestoreReq>) -> ApiResult {
    // PIN 用户已有自身种子(加密在注册表):恢复会绕过 PIN,直接拒绝
    if state::active_kind(&state) == zoe_core::users::UserKind::Pin {
        return Err(ApiError::BadRequest(
            "restore is only available for plain users; create a new user instead".to_string(),
        ));
    }
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
    // loopback + net(libp2p)常驻;BLE 状态由宿主应用(移动端回环桥)回写,
    // LAN/SIG Mesh 驱动未挂载时如实上报 down。
    let net_up = state::net_handle(&state).is_some();
    let net_peers = state::net_handle(&state)
        .map(|n| n.peers().len())
        .unwrap_or(0);
    let ble_up = state.ble_up.load(std::sync::atomic::Ordering::SeqCst);
    let lan_up = state::lan_handle(&state).is_some();
    let sigmesh_up = state::sigmesh_handle(&state).is_some();
    Ok(Json(json!({
        // 平台标记:移动端曾用于隐藏桌面专属传输;三项已全部接通,保留字段备用
        "platform": if cfg!(target_os = "android") { "mobile" } else { "desktop" },
        "ble": if ble_up { "up" } else { "down" },
        "lan": if lan_up { "up" } else { "down" },
        "net": if net_up { "up" } else { "down" },
        "loopback": "up",
        "sigmesh": if sigmesh_up { "up" } else { "down" },
        "net_peers": net_peers,
    })))
}

// ---------------------------------------------------------------------------
// 系统命令(宿主钩子,仅内嵌场景):重启应用/重连服务
// ---------------------------------------------------------------------------

/// 前端"重启服务"入口:调用宿主注册的钩子(移动端 → 重建 MainActivity 冷启动,
/// 冷启动后 PIN 用户进入锁定模式 → 立即见锁定屏)。桌面无钩子 → 404。
/// 放在 locked_gate 之外:锁定态下也允许重启(重启不改变锁定态,无安全风险)。
async fn system_restart(State(state): State<SharedState>) -> ApiResult {
    let Some(hook) = state.system_hook.as_ref() else {
        return Err(ApiError::NotFound(
            "system commands not registered (desktop daemon)".to_string(),
        ));
    };
    hook("restart").map_err(ApiError::Internal)?;
    Ok(Json(json!({
        "ok": true,
        "note": "restart requested",
    })))
}

// ---------------------------------------------------------------------------
// 网络(M3):地址查询、拨号、邀请
// ---------------------------------------------------------------------------

async fn net_addr(State(state): State<SharedState>) -> ApiResult {
    match state::net_handle(&state) {
        Some(net) => net_addr_impl(&net),
        None => Err(ApiError::NotFound(
            "net transport not available".to_string(),
        )),
    }
}

#[cfg(feature = "net")]
fn net_addr_impl(net: &crate::state::NetHandle) -> ApiResult {
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

#[cfg(not(feature = "net"))]
fn net_addr_impl(_net: &crate::state::NetHandle) -> ApiResult {
    Err(ApiError::NotFound(
        "net transport not available".to_string(),
    ))
}

#[derive(Deserialize)]
struct DialReq {
    addr: String,
}

async fn net_dial(State(state): State<SharedState>, Json(req): Json<DialReq>) -> ApiResult {
    let net = state::net_handle(&state)
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

async fn events_ws(State(state): State<SharedState>, ws: WebSocketUpgrade) -> Response {
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
    // 文件上传需要大请求体(8 MiB 文件 → ~10.7 MiB base64 JSON)
    let file_upload = Router::new()
        .route("/groups/{id}/files", post(send_file))
        .layer(DefaultBodyLimit::max(14 * 1024 * 1024));

    let protected = Router::new()
        .route("/users", get(users_list).post(create_user))
        .route("/users/{id}/set-pin", post(set_pin))
        .route("/unlock", post(unlock))
        .route("/me", get(me))
        .route("/card", get(card))
        .route("/card/import", post(import_card))
        .route("/peers", get(peers_list))
        .route("/peers/{id}/block", post(block_peer))
        .route("/pair/start", post(pair_start))
        .route("/pair/stop", post(pair_stop))
        .route("/pair/verify", post(pair_verify))
        .route("/groups", get(groups_list).post(create_group))
        .route(
            "/groups/{id}/messages",
            get(list_messages).post(send_message),
        )
        .route("/groups/{id}/invite", post(invite))
        .route("/groups/{id}/leave", post(leave_group))
        .route("/directs", get(directs_list).post(create_direct))
        .route("/files/{hash}", get(get_file))
        .merge(file_upload)
        .route("/net/addr", get(net_addr))
        .route("/net/dial", post(net_dial))
        .route("/devices", get(devices))
        .route("/devices/{id}/revoke", post(revoke_device))
        .route("/backup/mnemonic", get(backup_mnemonic))
        .route("/restore", post(restore))
        .route("/settings", get(get_settings).post(post_settings))
        .route("/transports", get(transports))
        .route("/events", get(events_ws))
        .route("/users/{id}/activate", post(activate_user))
        .route_layer(middleware::from_fn_with_state(state.clone(), locked_gate))
        .with_state(state.clone());

    Router::new()
        .route("/", get(index))
        .route("/assets/{file}", get(asset))
        .nest("/api/v1", protected)
        .route("/api/v1/system/restart", post(system_restart))
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
