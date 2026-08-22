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

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

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
                    // 观测对端发送者序号(乱序到达取最大值)
                    let _ = state.storage.sender_seq_observe(gid, env.sender, env.seq);
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
                let own_leaf = session.own_leaf_index();
                state.sessions.lock().unwrap().insert(gid.clone(), session);
                let _ = state.storage.create_group(
                    gid,
                    &format!("group-{}", &hex::encode(gid)[..8]),
                    epoch,
                    None,
                    now(),
                );
                // 登记邀请方为群成员(成员名 / 离线补投依赖此映射)
                let _ = state.storage.add_group_peer(gid, from, own_leaf);
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
            // 对方索要我们的 KeyPackage:按入站传输回包(net → lan → 队列兜底);
            // 同时回发 kp_info(本机身份 hex),供对方把联系人 hex ↔ libp2p id 关联。
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
            let info = Envelope::new(
                0,
                MSG_CONTROL,
                vec![],
                0,
                0,
                0,
                serde_json::to_vec(&json!({
                    "t": "kp_info",
                    "peer_id": state::identity(state)
                        .map(|i| hex::encode(i.verifying_key().to_bytes()))
                        .unwrap_or_default(),
                }))
                .unwrap(),
            );
            let state_clone = Arc::clone(state);
            let from = from.to_string();
            tokio::spawn(async move {
                let _ = send_peer_envelope(&state_clone, &from, &reply, true).await;
                let _ = send_peer_envelope(&state_clone, &from, &info, true).await;
            });
        }
        "kp_info" => {
            // 对方在连接/握手时告知身份:若已在联系人表则回填 net_peer_id;
            // 尚未导入名片 → 暂存映射,导入时由 import_card 回填。
            if let Some(pid_hex) = v.get("peer_id").and_then(|x| x.as_str()) {
                if let Ok(pid) = hex::decode(pid_hex) {
                    let exists = state
                        .storage
                        .peers()
                        .map(|ps| ps.iter().any(|p| p.peer_id == pid))
                        .unwrap_or(false);
                    if exists {
                        let _ = state.storage.set_peer_net_id(&pid, from);
                        let _ = state.events.send(
                            json!({"type":"peer","peer_id":pid_hex,"state":"seen"}).to_string(),
                        );
                    } else {
                        let mut pending = state.pending_identity.lock().unwrap();
                        pending.retain(|(h, _)| h != pid_hex);
                        pending.push((pid_hex.to_string(), from.to_string()));
                        if pending.len() > 128 {
                            pending.remove(0);
                        }
                    }
                }
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
                        // 不覆盖已导入的指纹/信任状态;缺失才登记
                        let _ = state.storage.ensure_peer(&pid, &name);
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

/// 将信封投递给单个 peer:优先 net(已连接)→ lan(已发现)→ 离线队列(outbox)。
/// `via_lan` 控制是否尝试 lan 回退(仅对 lan 身份可达的 peer 有意义;
/// lan 的 peer id 为 hex 公钥,与 libp2p id 不同格式)。
/// 返回 true = 已交给某传输或已入队;false = 完全不可达。
pub async fn send_peer_envelope(
    state: &SharedState,
    peer: &str,
    env: &Envelope,
    via_lan: bool,
) -> bool {
    if let Some(net) = state::net_handle(state) {
        if net.peers().iter().any(|p| p == peer) && net.send(peer, env.clone()).await.is_ok() {
            return true;
        }
    }
    if via_lan {
        if let Some(lan) = state::lan_handle(state) {
            if lan.peers().iter().any(|p| p == peer) && lan.send(peer, env.clone()).await.is_ok() {
                return true;
            }
        }
    }
    // 离线:入队,待 peer 重连后由 outbox 冲刷任务按序补投
    state
        .storage
        .outbox_append(peer, &env.encode(), &env.hash)
        .is_ok()
}

/// 按会话类型投递信封:私聊定向发给 direct_peer(经 net),群聊广播全部
/// 已连接 peer —— net + lan 逐 peer 发送,sigmesh 按洪泛语义发送到全网;
/// 已登记但离线的群成员入 outbox 队列(重连后补投)。
/// 返回 true = 至少有一个投递通道(直接发送或入队)成功。
pub async fn deliver_envelope(state: &SharedState, env: &Envelope) -> bool {
    let Some(net) = state::net_handle(state) else {
        // 无 net 传输:私聊兜底 SIG Mesh / LAN;群聊仅 LAN 广播 + 队列
        let direct_peer = state
            .storage
            .group(&env.group_id)
            .ok()
            .flatten()
            .and_then(|g| g.direct_peer);
        let mut delivered = false;
        if let Some(pid) = direct_peer {
            let to = String::from_utf8_lossy(&pid).to_string();
            if send_peer_envelope(state, &to, env, true).await {
                delivered = true;
            }
        } else {
            if let Some(lan) = state::lan_handle(state) {
                for peer in lan.peers() {
                    if lan.send(&peer, env.clone()).await.is_ok() {
                        delivered = true;
                    }
                }
            }
            if let Ok(peers) = state.storage.group_peers(&env.group_id) {
                let connected: HashSet<String> = state::lan_handle(state)
                    .map(|l| l.peers().into_iter().collect())
                    .unwrap_or_default();
                for m in peers {
                    if !connected.contains(&m.net_peer_id)
                        && state
                            .storage
                            .outbox_append(&m.net_peer_id, &env.encode(), &env.hash)
                            .is_ok()
                    {
                        delivered = true;
                    }
                }
            }
        }
        if let Some(sm) = state::sigmesh_handle(state) {
            if sm.send("*", env.clone()).await.is_ok() {
                delivered = true;
            }
        }
        return delivered;
    };
    let direct_peer = state
        .storage
        .group(&env.group_id)
        .ok()
        .flatten()
        .and_then(|g| g.direct_peer);
    let mut delivered = false;
    match direct_peer {
        Some(pid) => {
            let to = String::from_utf8_lossy(&pid).to_string();
            if send_peer_envelope(state, &to, env, true).await {
                delivered = true;
            }
            // 私聊兜底:net 不可达时仍尝试其余传输(移动端无 relay 时靠
            // SIG Mesh 近场 / LAN 送达)。
            if let Some(sm) = state::sigmesh_handle(state) {
                if sm.send("*", env.clone()).await.is_ok() {
                    delivered = true;
                }
            }
            if let Some(lan) = state::lan_handle(state) {
                if lan.peers().contains(&to) && lan.send(&to, env.clone()).await.is_ok() {
                    delivered = true;
                }
            }
        }
        None => {
            // 群广播:net 已知 peer
            let peers = net.peers();
            for peer in peers {
                if net.send(&peer, env.clone()).await.is_ok() {
                    delivered = true;
                }
            }
            // 局域网 peer(组播发现)
            if let Some(lan) = state::lan_handle(state) {
                for peer in lan.peers() {
                    if lan.send(&peer, env.clone()).await.is_ok() {
                        delivered = true;
                    }
                }
            }
            // SIG Mesh:洪泛到全网
            if let Some(sm) = state::sigmesh_handle(state) {
                if sm.send("*", env.clone()).await.is_ok() {
                    delivered = true;
                }
            }
            // 已登记但当前离线的成员:入队待补投
            if let Ok(members) = state.storage.group_peers(&env.group_id) {
                let connected: HashSet<String> = net
                    .peers()
                    .into_iter()
                    .chain(
                        state::lan_handle(state)
                            .map(|l| l.peers().into_iter().collect::<Vec<_>>())
                            .unwrap_or_default(),
                    )
                    .collect();
                for m in members {
                    if connected.contains(&m.net_peer_id) {
                        continue;
                    }
                    if state
                        .storage
                        .outbox_append(&m.net_peer_id, &env.encode(), &env.hash)
                        .is_ok()
                    {
                        delivered = true;
                    }
                }
            }
        }
    }
    delivered
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

/// 邀请流程:解析目标 → 拨号 → 索要 KeyPackage → 添加成员 → Welcome/元数据/commit。
///
/// 目标解析(二选一):
/// - `addr`:libp2p multiaddr(须含 `/p2p/<peer-id>`),经 net 拨号;
/// - `peer_hex`:联系人 zoe peer id(hex)。优先取联系人表登记的 net_peer_id;
///   未登记但已在局域网(组播)发现 → 走 lan 传输完成握手。
///
/// commit 投递给既有成员时,离线成员入 outbox 队列(重连后补投,防失步)。
pub async fn invite_peer(
    state: &SharedState,
    group_id: &[u8],
    addr: Option<&str>,
    peer_hex: Option<&str>,
) -> Result<serde_json::Value, String> {
    // 解析目标 peer(格式:libp2p id,或 lan hex id)+ 选路
    let target: String;
    let mut via_lan = false;
    let mut contact_hex: Option<Vec<u8>> = None;
    if let Some(a) = addr.filter(|a| !a.trim().is_empty()) {
        let a = a.trim();
        let net =
            state::net_handle(state).ok_or_else(|| "net transport not available".to_string())?;
        let pid = peer_id_from_addr(a)
            .ok_or_else(|| format!("multiaddr must include /p2p/<peer-id>: {a}"))?;
        net.dial(a).await.map_err(|e| e.to_string())?;
        target = pid;
    } else if let Some(h) = peer_hex.filter(|h| !h.trim().is_empty()) {
        let h = h.trim();
        let pid = hex::decode(h).map_err(|_| "bad peer id".to_string())?;
        contact_hex = Some(pid.clone());
        // 已登记过网络标识 → 按当前可达性选路(net 优先,lan 兜底)
        let known = state
            .storage
            .peers()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| p.peer_id == pid)
            .and_then(|p| p.net_peer_id)
            .filter(|n| !n.is_empty());
        if let Some(n) = known {
            let net_online = state::net_handle(state)
                .map(|net| net.peers().iter().any(|p| p == &n))
                .unwrap_or(false);
            let lan_online = state::lan_handle(state)
                .map(|lan| lan.peers().iter().any(|p| p == &n))
                .unwrap_or(false);
            if net_online {
                target = n;
            } else if lan_online {
                target = n;
                via_lan = true;
            } else {
                return Err("peer offline: 联系人不在线,无法完成 KeyPackage 握手".to_string());
            }
        } else if let Some(lan) = state::lan_handle(state) {
            // 未登记网络标识但局域网已发现(hex id 与 lan 传输同格式)
            if lan.peers().iter().any(|p| p == h) {
                target = h.to_string();
                via_lan = true;
            } else {
                return Err(
                    "peer unreachable: 联系人网络标识尚未确认(连接建立后数秒内自动交换身份),请稍后重试或先手动拨号对方".to_string(),
                );
            }
        } else {
            return Err("peer unreachable: 联系人无网络地址且局域网传输不可用".to_string());
        }
    } else {
        return Err("invite requires addr or peer_id".to_string());
    }
    if target.is_empty() {
        return Err("unresolved peer id".to_string());
    }

    // 注册 KeyPackage 等待
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .pending_keypackages
        .lock()
        .unwrap()
        .insert(target.clone(), tx);

    // 拨号 + 发送 kp_req(经选定传输)
    let req = Envelope::new(
        0,
        MSG_CONTROL,
        group_id.to_vec(),
        0,
        0,
        0,
        serde_json::to_vec(&json!({"t": "kp_req"})).unwrap(),
    );
    if via_lan {
        let lan =
            state::lan_handle(state).ok_or_else(|| "lan transport not available".to_string())?;
        lan.send(&target, req).await.map_err(|e| e.to_string())?;
    } else {
        let net =
            state::net_handle(state).ok_or_else(|| "net transport not available".to_string())?;
        net.send(&target, req).await.map_err(|e| e.to_string())?;
    };

    // 等待 KeyPackage(15s 超时)
    let kp_bytes = tokio::time::timeout(std::time::Duration::from_secs(15), rx)
        .await
        .map_err(|_| "timed out waiting for key package (对方需在线并可达)".to_string())?
        .map_err(|_| "key package channel closed".to_string())?;

    // 添加成员(协调者):返回 (commit, welcome, epoch);新成员 leaf = 加入前成员数
    let (commit, welcome, epoch, new_leaf) = {
        let mls = state::mls_identity(state).ok_or_else(|| "locked".to_string())?;
        let mut sessions = state.sessions.lock().unwrap();
        let session = sessions
            .get_mut(group_id)
            .ok_or_else(|| "group session not found".to_string())?;
        let new_leaf = session.members().len() as u32;
        let provider = state.provider.lock().unwrap();
        let kp = MlsSession::key_package_from_bytes(&*provider, &kp_bytes)
            .map_err(|e| format!("invalid key package: {e}"))?;
        let (commit, welcome, epoch) = session
            .add_member(&*provider, &mls, &kp)
            .map_err(|e| format!("add member failed: {e}"))?;
        (commit, welcome, epoch, new_leaf)
    };
    let _ = state.storage.update_group_epoch(group_id, epoch);
    // 登记新成员(成员名 / 离线补投依赖)
    let _ = state.storage.add_group_peer(group_id, &target, new_leaf);
    // 联系人若在册,回填 net 标识(后续可按联系人直接邀请)
    if let Some(hex_id) = &contact_hex {
        let _ = state.storage.set_peer_net_id(hex_id, &target);
    } else if !via_lan {
        let net_id = target.clone();
        if let Some(hex_id) = state
            .storage
            .peers()
            .ok()
            .into_iter()
            .flatten()
            .find(|p| p.net_peer_id.as_deref() == Some(net_id.as_str()))
            .map(|p| p.peer_id)
        {
            let _ = state.storage.set_peer_net_id(&hex_id, &target);
        }
    }

    // 发送 WELCOME 给新成员(经选定传输)
    let welcome_env = Envelope::new(
        0,
        MSG_WELCOME,
        group_id.to_vec(),
        epoch as u32,
        0,
        0,
        welcome,
    );
    if via_lan {
        state::lan_handle(state)
            .ok_or_else(|| "lan transport not available".to_string())?
            .send(&target, welcome_env)
            .await
            .map_err(|e| e.to_string())?;
    } else {
        state::net_handle(state)
            .ok_or_else(|| "net transport not available".to_string())?
            .send(&target, welcome_env)
            .await
            .map_err(|e| e.to_string())?;
    }

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
    let _ = send_peer_envelope(state, &target, &meta, via_lan).await;

    // commit 广播给既有成员(排除新成员);离线成员入队待补投
    let existing = state.storage.group_peers(group_id).ok().unwrap_or_default();
    for m in existing {
        if m.net_peer_id == target {
            continue;
        }
        let commit_env = Envelope::new(
            0,
            MSG_COMMIT,
            group_id.to_vec(),
            epoch as u32,
            0,
            0,
            commit.clone(),
        );
        let _ = send_peer_envelope(state, &m.net_peer_id, &commit_env, false).await;
    }

    let _ = state.events.send(
        json!({"type":"group","event":"member_added","group_id":hex::encode(group_id),"epoch":epoch}).to_string(),
    );

    Ok(json!({ "ok": true, "peer": target, "epoch": epoch, "leaf": new_leaf }))
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
    // 单聊也是双人 MLS 群:登记成员映射(leaf 1 = 对端)
    let _ = state.storage.add_group_peer(&gid, &target, 1);

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
