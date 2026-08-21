//! 消息核心:入站信封分发与跨设备会话流程。
//!
//! 入站信封类型(MSG_*):
//! - PRIVATE   → MLS 解密 → 落库 + WS 事件(文件消息:小文件自动下载落盘)
//! - PROPOSAL / COMMIT → MLS 处理(epoch 推进)→ 更新群组元数据
//! - WELCOME   → 加入群组会话 + 落库
//! - KEY_PACKAGE → 完成邀请流程的 KeyPackage 请求
//! - CONTROL   → keypackage 请求应答 / 群组元数据(group_meta,含单聊标记)
//!
//! 投递路由:私聊(单聊)信封只发给 `direct_peer`;群聊广播给所有已连接 peer。

use std::path::PathBuf;

use serde_json::json;
use zoe_core::content::{self, FileContent};
use zoe_core::envelope::{
    Envelope, MSG_COMMIT, MSG_CONTROL, MSG_KEY_PACKAGE, MSG_PRIVATE, MSG_PROPOSAL, MSG_WELCOME,
};
use zoe_core::mls::{MlsSession, Processed};
use zoe_transport::Transport;

use crate::state::{self, now, SharedState};

/// 处理一条入站信封(由传输订阅任务调用)。
pub fn handle_inbound(state: &SharedState, from: &str, env: &Envelope) {
    if !state::is_unlocked(state) {
        eprintln!("inbound {from}: ignored while locked");
        return;
    }
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
                    // 文件消息:小文件自动下载(解密后直接落盘 + 标记)
                    if let Some(f) = content::decode_file(&plaintext) {
                        if f.size <= content::FILE_AUTO_MAX as u64
                            && (!persist_file(state, &env.hash, &f)
                                || state.storage.mark_message_downloaded(&env.hash).is_err())
                        {
                            eprintln!("auto-download failed for {}", hex::encode(env.hash));
                        }
                    }
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
            let Some(mls) = state::mls_identity(state) else {
                eprintln!("kp_req: ignored while locked");
                return;
            };
            let provider = state.provider.lock().unwrap();
            let kp = match MlsSession::new_key_package(&*provider, &mls) {
                Ok(kp) => kp,
                Err(e) => {
                    eprintln!("kp_req: key package generation failed: {e}");
                    return;
                }
            };
            drop(provider);
            let reply = Envelope::new(0, MSG_KEY_PACKAGE, vec![], 0, 0, 0, kp);
            if let Some(net) = state::net_handle(state) {
                let from = from.to_string();
                tokio::spawn(async move {
                    let _ = net.send(&from, reply).await;
                });
            }
        }
        "group_meta" => {
            // 邀请方/发起方告知群组名称与单聊标记(在 WELCOME 之后发送)
            let direct = v.get("direct").and_then(|x| x.as_bool()).unwrap_or(false);
            if let Some(name) = v.get("name").and_then(|x| x.as_str()) {
                if name.len() <= 64 {
                    let _ = state.storage.set_group_name(&env.group_id, name);
                }
            }
            if direct {
                // 单聊:direct_peer = 发信方 libp2p id;同时登记联系人
                // (peer_id 由发起方在 meta 中携带,与二维码名片同钥)。
                let _ = state
                    .storage
                    .set_group_direct(&env.group_id, from.as_bytes());
                if let Some(pid_hex) = v.get("peer_id").and_then(|x| x.as_str()) {
                    if let Ok(pid) = hex::decode(pid_hex) {
                        let name = v
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let _ = state.storage.upsert_peer(&pid, &pid, &name, 0, now());
                        let _ = state.storage.set_peer_net_id(&pid, from);
                        let _ = state.events.send(
                            json!({"type":"peer","peer_id":pid_hex,"state":"paired"}).to_string(),
                        );
                    }
                }
            }
            let _ = state.events.send(
                json!({"type":"group","event":"renamed","group_id":hex::encode(&env.group_id)})
                    .to_string(),
            );
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

/// 按会话类型投递信封:私聊定向发给 direct_peer(经 net),群聊广播全部
/// 已连接 peer —— net + lan 逐 peer 发送,sigmesh 按洪泛语义发送到全网。
pub async fn deliver_envelope(state: &SharedState, env: &Envelope) {
    let Some(net) = state::net_handle(state) else {
        return;
    };
    let direct_peer = state
        .storage
        .group(&env.group_id)
        .ok()
        .flatten()
        .and_then(|g| g.direct_peer);
    match direct_peer {
        Some(pid) => {
            let to = String::from_utf8_lossy(&pid).to_string();
            if let Err(e) = net.send(&to, env.clone()).await {
                eprintln!("direct send to {to}: {e}");
            }
        }
        None => {
            // 群广播:net 已知 peer
            let peers = net.peers();
            for peer in peers {
                if let Err(e) = net.send(&peer, env.clone()).await {
                    eprintln!("broadcast to {peer}: {e}");
                }
            }
            // 局域网 peer(组播发现)
            if let Some(lan) = state::lan_handle(state) {
                for peer in lan.peers() {
                    if let Err(e) = lan.send(&peer, env.clone()).await {
                        eprintln!("lan broadcast to {peer}: {e}");
                    }
                }
            }
            // SIG Mesh:洪泛到全网
            if let Some(sm) = state::sigmesh_handle(state) {
                if let Err(e) = sm.send("*", env.clone()).await {
                    eprintln!("sigmesh flood: {e}");
                }
            }
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
    let net = state::net_handle(state).ok_or_else(|| "net transport not available".to_string())?;

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
        let mls = state::mls_identity(state).ok_or_else(|| "locked".to_string())?;
        let mut sessions = state.sessions.lock().unwrap();
        let session = sessions
            .get_mut(group_id)
            .ok_or_else(|| "group session not found".to_string())?;
        let provider = state.provider.lock().unwrap();
        let kp = MlsSession::key_package_from_bytes(&*provider, &kp_bytes)
            .map_err(|e| format!("invalid key package: {e}"))?;
        session
            .add_member(&*provider, &mls, &kp)
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

/// 发起与联系人的单聊:建立连接 → 索要 KeyPackage → 建双人 MLS 群 → Welcome。
///
/// `peer_id` = 联系人表中登记的 zoe peer id(hex);`addr` 可选:
/// 已记录过对端 libp2p 标识且在线时无需地址;否则必须提供(拨号建立连接)。
pub async fn start_direct(
    state: &SharedState,
    peer_id_hex: &str,
    addr: Option<&str>,
) -> Result<serde_json::Value, String> {
    let net = state::net_handle(state).ok_or_else(|| "net transport not available".to_string())?;
    let pid = hex::decode(peer_id_hex).map_err(|_| "bad peer id".to_string())?;

    // 联系人必须在册且未被阻止
    let peer = state
        .storage
        .peers()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.peer_id == pid)
        .ok_or_else(|| "contact not found".to_string())?;
    if peer.trust_status == 2 {
        return Err("peer blocked".to_string());
    }

    // 解析对端 libp2p id:优先联系人已记录的网络标识,否则从地址 /p2p/ 段取
    let target = match peer.net_peer_id.clone() {
        Some(t) => t,
        None => {
            let a = addr
                .filter(|a| !a.trim().is_empty())
                .ok_or_else(|| "peer unreachable: 对端无记录地址,请提供对端地址".to_string())?;
            peer_id_from_addr(a)
                .ok_or_else(|| format!("multiaddr must include /p2p/<peer-id>: {a}"))?
        }
    };

    // 已有私聊则直接复用
    if let Some(existing) = find_direct_with(state, &target) {
        return Ok(json!({
            "group_id": hex::encode(&existing),
            "peer_id": peer_id_hex,
            "existing": true,
        }));
    }

    // 拨号(可选)后索要 KeyPackage
    if let Some(a) = addr.filter(|a| !a.trim().is_empty()) {
        net.dial(a).await.map_err(|e| e.to_string())?;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .pending_keypackages
        .lock()
        .unwrap()
        .insert(target.clone(), tx);
    let req = Envelope::new(
        0,
        MSG_CONTROL,
        vec![],
        0,
        0,
        0,
        serde_json::to_vec(&json!({"t": "kp_req"})).unwrap(),
    );
    net.send(&target, req).await.map_err(|e| e.to_string())?;

    let kp_bytes = tokio::time::timeout(std::time::Duration::from_secs(20), rx)
        .await
        .map_err(|_| "timed out waiting for key package".to_string())?
        .map_err(|_| "key package channel closed".to_string())?;

    // 建双人 MLS 群(创建者 + 联系人)
    let mut gid = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut gid);
    let mls = state::mls_identity(state).ok_or_else(|| "locked".to_string())?;
    let (epoch, welcome) = {
        let provider = state.provider.lock().unwrap();
        let mut session = MlsSession::create_group(&*provider, &mls, &gid)
            .map_err(|e| format!("create group failed: {e}"))?;
        let kp = MlsSession::key_package_from_bytes(&*provider, &kp_bytes)
            .map_err(|e| format!("invalid key package: {e}"))?;
        let (_commit, welcome, epoch) = session
            .add_member(&*provider, &mls, &kp)
            .map_err(|e| format!("add member failed: {e}"))?;
        state.sessions.lock().unwrap().insert(gid.to_vec(), session);
        (epoch, welcome)
    };

    let name = peer
        .display_name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("peer-{}", &peer_id_hex[..peer_id_hex.len().min(8)]));
    state
        .storage
        .create_group(&gid, &name, epoch, None, now())
        .map_err(|e| e.to_string())?;
    state
        .storage
        .set_group_direct(&gid, target.as_bytes())
        .map_err(|e| e.to_string())?;
    state
        .storage
        .set_peer_net_id(&pid, &target)
        .map_err(|e| e.to_string())?;

    // WELCOME + 元数据(单聊标记 + 本机 peer_id,供对方登记联系人)
    let welcome_env = Envelope::new(0, MSG_WELCOME, gid.to_vec(), epoch as u32, 0, 0, welcome);
    net.send(&target, welcome_env)
        .await
        .map_err(|e| e.to_string())?;
    let own_peer_id = state::identity(state)
        .map(|i| hex::encode(i.verifying_key().to_bytes()))
        .unwrap_or_default();
    let meta = json!({
        "t": "group_meta",
        "name": name,
        "direct": true,
        "peer_id": own_peer_id,
    });
    let meta_env = Envelope::new(
        0,
        MSG_CONTROL,
        gid.to_vec(),
        epoch as u32,
        0,
        0,
        serde_json::to_vec(&meta).unwrap(),
    );
    let _ = net.send(&target, meta_env).await;

    let _ = state
        .events
        .send(json!({"type":"group","event":"created","group_id":hex::encode(gid)}).to_string());
    Ok(json!({
        "group_id": hex::encode(gid),
        "peer_id": peer_id_hex,
        "epoch": epoch,
    }))
}

/// 查找与指定 libp2p peer 的既有私聊群。
pub fn find_direct_with(state: &SharedState, libp2p_peer: &str) -> Option<Vec<u8>> {
    state
        .storage
        .groups()
        .ok()?
        .into_iter()
        .find(|g| g.direct_peer.as_deref() == Some(libp2p_peer.as_bytes()))
        .map(|g| g.group_id)
}

// ---------------------------------------------------------------------------
// 文件落盘(小文件自动下载 / 手动下载端点共用)
// ---------------------------------------------------------------------------

/// 文件存储目录(data_dir/files)。
pub fn files_dir(state: &SharedState) -> PathBuf {
    state.data_dir.join("files")
}

/// 将文件消息内容写入本地 files/ 目录。文件名 = <消息哈希>.<安全扩展名>。
pub fn persist_file(state: &SharedState, msg_hash: &[u8], f: &FileContent) -> bool {
    let dir = files_dir(state);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = dir.join(format!("{}.{}", hex::encode(msg_hash), safe_ext(&f.name)));
    std::fs::write(&path, &f.data).is_ok()
}

/// 从文件名提取安全扩展名(仅字母数字,小写,≤8 字符;失败回退 "bin")。
pub fn safe_ext(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("bin");
    let clean: String = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if clean.is_empty() {
        "bin".to_string()
    } else {
        clean.to_ascii_lowercase()
    }
}
