# 统一信封与传输帧格式 v0.1

所有传输层只搬运 **Envelope**(统一信封);分片、确认、去重是传输层职责,与 MLS 密文无关。

## 1. Envelope(应用层,传输无关)

TLS 展示语言风格,大端序。除 `hash` 外,所有字段先序列化,`hash = SHA-256(前序全部字节)`,作为去重键与完整性校验。

```
struct {
    uint8  version;        // = 0x01
    uint8  flags;          // bit0: 需确认(ack)  bit1: 多路径投递  bit2: 含分片
    uint8  msg_type;       // 0=MLS PrivateMessage  1=MLS Proposal  2=MLS Commit
                           // 3=Welcome  4=KeyPackage  5=控制(见 §3)
    uint8  group_id_len;   // 1..=255
    opaque group_id<0..255>;
    uint32 epoch;          // MLS epoch(控制消息可为 0)
    uint32 sender;         // MLS leaf index;0xFFFFFFFF = 协调者/系统
    uint64 seq;            // 发送方每 (sender, group) 单调递增;仅作去重与排序提示
    uint32 payload_len;
    opaque payload<0..2^32-1>;  // MLSMessage(TLS 序列化)或控制载荷
    opaque hash<32>;       // SHA-256(以上全部字节)
}
```

- 总开销 ≈ 60 字节,`payload` 即 MLS 密文。
- **去重键** = `hash`;多路径投递(bit1)下,各路径同一 Envelope 只处理一次。
- **排序语义**:MLS 私密消息的乱序/丢失由 MLS 层处理(epoch + 消息序号);`seq` 只用于传输层去重与 UI 展示顺序,不作为密码学顺序依据。
- 控制消息(5):`payload` 为控制子类型 + 参数,见 §3。

## 2. 传输帧

### 2.1 BLE GATT(连接通道,MTU ≥ 512)

连接建立后协商 MTU,Envelope 分片传输;每片 13 字节头(实现含 msg_id 与 TTL,较初版设计补强):

```
struct {
    uint8  magic;          // 0x5A
    opaque msg_id<8>;      // Envelope hash 前 8 字节(去重键,接收端重组依据)
    uint8  ttl;            // 剩余转发跳数(0 = 不再转发)
    uint8  chunk_idx;      // 0..total-1
    uint16 total_chunks;   // 1..=512
    opaque data<0..499>;   // 每片载荷(MTU 512 时)
}
```

- 接收方按 (msg_id, chunk_idx) 重组;`msg_id` 已见则丢弃整条(去重集缓存 24h)。
- **存储转发**:完整 Envelope 重组后,若 `ttl > 0`,以 `ttl-1` 分片转发给除来源外的所有邻居。
- 无确认机制,依赖上层重传(见 §2.1 ack 语义在 GATT 通道上完成)。

### 2.2 SIG Bluetooth Mesh(Phase 2,应用载荷 11 字节)

SIG Mesh 单包应用载荷过小,采用分片 + 泛洪去重:

```
struct {
    uint32 msg_id;     // Envelope hash 前 4 字节
    uint8  chunk_idx;  // 0..255
    uint8  total;      // 1..255
    opaque data<5>;    // 每片 5 字节,单包载荷上限 11 字节
}
```

- 节点按 (msg_id, chunk_idx) 重组;`msg_id` 已见则丢弃整条(去重集缓存 24h)。
- 无确认机制,依赖上层重传(见 §2.1 ack 语义在 GATT 通道上完成)。

### 2.3 libp2p(远程/局域网)

- 每个传输连接上,Envelope 以 `u32` 长度前缀流式传输(逐条)。
- libp2p 协议 ID:`/zoe/envelope/0.1.0`;可选 `noise` 传输加密。
- ack 语义同 §2.1;libp2p 通道本身有可靠交付,应用层 ack 仅用于端到端送达状态。

## 3. 控制消息

| 子类型 | 载荷 | 用途 |
|---|---|---|
| 0x01 ack | `opaque hash<32>` | 送达确认 |
| 0x02 ping / 0x03 pong | `uint64 nonce` | 可达性探测(路由选择用) |
| 0x04 sync_request | `uint64 last_seq` | 上线后向协调者/邻居拉取离线 Envelope |
| 0x05 sync_done | — | 拉取完成 |
| 0x06 keypkg_request | `opaque peer_fp<32>` | 请求对方 KeyPackage(首次接触后) |

## 4. 编解码与测试要求

- 编解码为纯函数,无 I/O;实现时要求**往返属性测试**(任意字节序列解码失败不 panic、编码-解码-编码幂等)。
- 所有长度字段必须有上限校验;`hash` 必须在解码时验证(丢弃不匹配帧)。
- 未知 `version` / `msg_type` 一律丢弃并告警,不静默降级。
