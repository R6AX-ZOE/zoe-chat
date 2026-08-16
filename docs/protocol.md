# 会话流程与排序协议 v0.1

定义:配对、建群、消息、排序、离线投递的完整流程。所有消息均为 `docs/envelope.md` 定义的 Envelope。

## 1. 首次接触(配对)

**近场(BLE)**:
1. A 进入配对模式:开始广告(广告包携带一次性配对码 `pair_code`,8 字节随机数)。
2. B 扫描到 A → GATT 连接 → 交换身份信息:
   - 双方:指纹(身份公钥 SHA-256 前 32 字节)、设备 MLS 签名公钥、设备凭据(用户身份签名)、KeyPackage、libp2p PeerId、BLE 地址。
3. 双方显示对方指纹;UI 提供 QR 带外验证(扫码比指纹,可选跳过 = TOFU)。
4. 互相写入 `peers`(trust_status=1 或 0);配对码作废。

**远程**:
1. UI 生成"名片":二维码/文本 = `zoe://peer/<fingerprint>/<peerid>`。
2. 对方扫码 → 手动输入 PeerId 建立 libp2p 连接(DCUtR 打洞,失败提示)。
3. 连接建立后走与近场相同的 §1.2–1.4 交换(信道内完成,无需再扫码)。

**安全要求**:KeyPackage 交换必须校验携带的用户身份签名;指纹不匹配直接拒绝连接(失败安全)。

## 2. 建群与邀请

1. 创建者:生成随机 32 字节 group_id,openmls 建群(自己为唯一成员),`coordinator = 创建者`。
2. 邀请成员 M:创建者经**任一可达通道**向 M 发送 `Welcome` 类型的 Envelope(内含 KeyPackage→Welcome 的 MLS 过程)。
3. M 解密 Welcome,加入群组,`coordinator` 记录为创建者。
4. 后续加人:任意成员向协调者提交 `Proposal(add)`(见 §3),协调者合并为 Commit 广播,Welcome 随 Commit 一并投递。

## 3. 排序协议(协调者即 DS)

**原则**:普通消息不经过协调者;结构变更必须经协调者全序。

- **普通消息(PrivateMessage)**:发送方直接多路径投递(可达通道全部发,bit1 置位);接收方按 Envelope `hash` 去重,MLS 层处理乱序/丢失。
- **结构变更(加人/移除/update)**:
  1. 成员 → 协调者:单播 `Proposal` Envelope(msg_type=1)。
  2. 协调者:存入 `pending_proposals`;合并窗口(500ms 或满 32 条)后,用 openmls 生成 `Commit`(msg_type=2)广播全员;同时把涉及新成员的 `Welcome` 一并广播。
  3. 全员验证 Commit 的 MLS 签名与凭据链,epoch 推进,更新本地 `groups.state`(与消息落库同事务)。
- **并发冲突**:协调者是唯一提交者,天然无并发冲突;协调者必须校验每个 Proposal 的成员资格(MLS 验证失败即丢弃并告警)。
- **协调者离线**:成员的结构变更存本地 `pending_proposals`,协调者恢复后重放;普通消息通道不受影响(多路径直连)。协调者长期离线 = 群组冻结,UI 明示。

## 4. 离线与多路径投递

- **离线缓冲**:协调者(群内)与直接邻居各自缓冲最近 24h 的 Envelope;接收方上线后发 `sync_request(last_seq)`,缓冲方回放。
- **多路径**:同一 Envelope 按"BLE > LAN(mDNS) > 互联网打洞"优先级并发投递,先到先处理,`hash` 去重。
- **送达状态**:bit0 消息要求 ack;UI 展示 待投递/已送达/已读,不做已读回执的强保证(尽力)。

## 5. 设备管理

- **新增设备**:新设备生成自身 MLS 签名钥,由用户身份私钥签发凭据 → 经任一已信任设备(近场或远程)发起 `Proposal(add)` 加入各群组;新设备 KeyPackage 随 Proposal 附送。
- **吊销设备**:任一已信任设备发起 `Proposal(remove)`;被吊销设备的凭据在群组 epoch 推进后失效(MLS 凭据链校验拒绝其消息)。
- 多设备间**不共享会话状态**(v1 简化):每设备独立 MLS 成员身份,历史消息不回放给新设备,除非其他成员重新转发密文。

## 6. 状态机(每 peer)

```
IDLE → PAIRING(近场广告/扫描 或 远程名片) → EXCHANGED(指纹+凭据已换)
     → VERIFIED(带外验证完成,trust=1) 或 TOFU(trust=0)
     → SESSION(至少一个群组共同成员) → BLOCKED(用户阻止)
```

异常路径:指纹不匹配 → 立即断开并记录;密钥/凭据签名无效 → 拒绝连接;任何降级尝试(如协商到非加密模式)不存在——协议无明文模式。
