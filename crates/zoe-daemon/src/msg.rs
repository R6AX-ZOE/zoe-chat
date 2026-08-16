//! 消息核心:入站信封分发与跨设备会话流程。
//!
//! 入站信封类型(MSG_*):
//! - PRIVATE   → MLS 解密 → 落库 + WS 事件
//! - PROPOSAL / COMMIT → MLS 处理(epoch 推进)→ 更新群组元数据
//! - WELCOME   → 加入群组会话 + 落库
//! - KEY_PACKAGE → 完成邀请流程的 KeyPackage 请求
//! - CONTROL   → keypackage 请求应答 / 群组元数据(group_meta)

use serde_json::json;
use zoe_core::envelope::{
    Envelope, MSG_COMMIT, MSG_CONTROL, MSG_KEY_PACKAGE, MSG_PRIVATE, MSG_PROPOSAL, MSG_WELCOME,
};
use zoe_core::mls::{MlsSession, Processed};
use zoe_transport::Transport;

use crate::state::{now, SharedState};

/// 处理一条入站信封(由传输订阅任务调用)。
pub fn handle_inbound(state: &SharedState, from: &str, env: &Envelope) {
    let gid = &env.group_id;
    match env.msg_type {
        MSG_PRIVATE | MSG_PROPOSAL | MSG_COMMIT => {
            let result = {
                let mut sessions = state.sessions.lock().unwrap();
                match sessions.get_mut(gid) {
                    None => {
                        eprintln!("inbound {from}: no session for group {}", hex::encode(gid));
                        return;
                    }
                    Some(session) => {
                        let provider = state.provider.lock().unwrap();
                        session.process(&*provider, &env.payload)
                    }
                }
            };
            match result {
                Ok(Processed::Message(plaintext)) => {
                    let _ = state.storage.insert_message(
                        &env.hash,
                        &env.encode(),
                        gid,
                        env.epoch as u64,
                        Some(from.as_bytes()),
                        Some(env.seq),
                        0, // direction = in
                        1, // status = delivered
                        Some(&plaintext),
                        now(),
                    );
                    let _ = state
                        .events
                        .send(json!({"type":"message","group_id":hex::encode(gid)}).to_string());
                }
                Ok(Processed::GroupChange) => {
                    let epoch = sessions_epoch(state, gid);
                    let _ = state.storage.update_group_epoch(gid, epoch);
                    let _ = state.events.send(
                        json!({"type":"group","event":"epoch","group_id":hex::encode(gid),"epoch":epoch}).to_string(),
                    );
                }
                Err(e) => {
                    eprintln!("inbound {from}: mls process error: {e}");
                }
            }
        }
        MSG_WELCOME => match MlsSession::join(&*state.provider.lock().unwrap(), &env.payload) {
            Ok(session) => {
                let epoch = session.epoch();
                state.sessions.lock().unwrap().insert(gid.clone(), session);
                let _ = state.storage.create_group(
                    gid,
                    &format!("group-{}", &hex::encode(gid)[..8]),
                    epoch,
                    None,
                    now(),
                );
                let _ = state.events.send(
                    json!({"type":"group","event":"joined","group_id":hex::encode(gid)})
                        .to_string(),
                );
            }
            Err(e) => eprintln!("inbound {from}: welcome join failed: {e}"),
        },
        MSG_KEY_PACKAGE => {
            if let Some(tx) = state.pending_keypackages.lock().unwrap().remove(from) {
                let _ = tx.send(env.payload.clone());
            }
        }
        MSG_CONTROL => {
            handle_control(state, from, env);
        }
        other => eprintln!("inbound {from}: unknown msg_type {other}"),
    }
}

fn handle_control(state: &SharedState, from: &str, env: &Envelope) {
    // 控制载荷 = JSON {"t": <子类型>, ...}
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&env.payload) else {
        eprintln!("inbound {from}: bad control payload");
        return;
    };
    let t = v.get("t").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "kp_req" => {
            // 对方索要我们的 KeyPackage:发送 MSG_KEY_PACKAGE 信封
            let provider = state.provider.lock().unwrap();
            let kp = match MlsSession::new_key_package(&*provider, &state.mls_identity) {
                Ok(kp) => kp,
                Err(e) => {
                    eprintln!("kp_req: key package generation failed: {e}");
                    return;
                }
            };
            drop(provider);
            let reply = Envelope::new(0, MSG_KEY_PACKAGE, vec![], 0, 0, 0, kp);
            if let Some(net) = &state.net {
                let net = net.clone();
                let from = from.to_string();
                tokio::spawn(async move {
                    let _ = net.send(&from, reply).await;
                });
            }
        }
        "group_meta" => {
            // 邀请方告知群组名称(在 WELCOME 之前发送)
            if let Some(name) = v.get("name").and_then(|x| x.as_str()) {
                let _ = state.storage.set_group_name(&env.group_id, name);
                let _ = state.events.send(
                    json!({"type":"group","event":"renamed","group_id":hex::encode(&env.group_id)})
                        .to_string(),
                );
            }
        }
        other => eprintln!("inbound {from}: unknown control subtype {other}"),
    }
}

fn sessions_epoch(state: &SharedState, gid: &[u8]) -> u64 {
    state
        .sessions
        .lock()
        .unwrap()
        .get(gid)
        .map(|s| s.epoch())
        .unwrap_or(0)
}

/// 向所有已连接 peer 广播信封(由消息发送调用)。
pub async fn broadcast_envelope(state: &SharedState, env: &Envelope) {
    let Some(net) = &state.net else { return };
    let peers = net.peers();
    if peers.is_empty() {
        return;
    }
    for peer in peers {
        if let Err(e) = net.send(&peer, env.clone()).await {
            eprintln!("broadcast to {peer}: {e}");
        }
    }
}

/// 解析 multiaddr 中的 /p2p/<peerid> 段。
pub fn peer_id_from_addr(addr: &str) -> Option<String> {
    let mut parts = addr.split("/p2p/");
    parts.next()?;
    let pid = parts.next()?.split('/').next()?;
    if pid.is_empty() {
        None
    } else {
        Some(pid.to_string())
    }
}

/// 邀请流程:向 addr 拨号 → 索要 KeyPackage → 添加成员 → 发送 Welcome/元数据。
pub async fn invite_peer(
    state: &SharedState,
    group_id: &[u8],
    addr: &str,
) -> Result<serde_json::Value, String> {
    let net = state
        .net
        .clone()
        .ok_or_else(|| "net transport not available".to_string())?;

    // 解析目标 peer id
    let peer = peer_id_from_addr(addr)
        .ok_or_else(|| format!("multiaddr must include /p2p/<peer-id>: {addr}"))?;

    // 注册 KeyPackage 等待
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .pending_keypackages
        .lock()
        .unwrap()
        .insert(peer.clone(), tx);

    // 拨号 + 发送 kp_req
    net.dial(addr).await.map_err(|e| e.to_string())?;
    let req = Envelope::new(
        0,
        MSG_CONTROL,
        group_id.to_vec(),
        0,
        0,
        0,
        serde_json::to_vec(&json!({"t": "kp_req"})).unwrap(),
    );
    net.send(&peer, req).await.map_err(|e| e.to_string())?;

    // 等待 KeyPackage(15s 超时)
    let kp_bytes = tokio::time::timeout(std::time::Duration::from_secs(15), rx)
        .await
        .map_err(|_| "timed out waiting for key package".to_string())?
        .map_err(|_| "key package channel closed".to_string())?;

    // 添加成员(协调者):返回 (commit, welcome, epoch)
    let (commit, welcome, epoch) = {
        let mut sessions = state.sessions.lock().unwrap();
        let session = sessions
            .get_mut(group_id)
            .ok_or_else(|| "group session not found".to_string())?;
        let provider = state.provider.lock().unwrap();
        let kp = MlsSession::key_package_from_bytes(&*provider, &kp_bytes)
            .map_err(|e| format!("invalid key package: {e}"))?;
        session
            .add_member(&*provider, &state.mls_identity, &kp)
            .map_err(|e| format!("add member failed: {e}"))?
    };
    let _ = state.storage.update_group_epoch(group_id, epoch);

    // 发送 WELCOME 给新成员
    let welcome_env = Envelope::new(
        0,
        MSG_WELCOME,
        group_id.to_vec(),
        epoch as u32,
        0,
        0,
        welcome,
    );
    net.send(&peer, welcome_env)
        .await
        .map_err(|e| e.to_string())?;

    // 发送群组元数据(名称)
    let name = state
        .storage
        .group(group_id)
        .ok()
        .flatten()
        .and_then(|g| g.name)
        .unwrap_or_default();
    let meta = Envelope::new(
        0,
        MSG_CONTROL,
        group_id.to_vec(),
        epoch as u32,
        0,
        0,
        serde_json::to_vec(&json!({"t": "group_meta", "name": name})).unwrap(),
    );
    let _ = net.send(&peer, meta).await;

    // commit 广播给既有成员(排除新成员)
    let connected: Vec<String> = net.peers();
    for p in connected.iter().filter(|p| *p != &peer) {
        let commit_env = Envelope::new(
            0,
            MSG_COMMIT,
            group_id.to_vec(),
            epoch as u32,
            0,
            0,
            commit.clone(),
        );
        let _ = net.send(p, commit_env).await;
    }

    let _ = state.events.send(
        json!({"type":"group","event":"member_added","group_id":hex::encode(group_id),"epoch":epoch}).to_string(),
    );

    Ok(json!({ "ok": true, "peer": peer, "epoch": epoch }))
}
