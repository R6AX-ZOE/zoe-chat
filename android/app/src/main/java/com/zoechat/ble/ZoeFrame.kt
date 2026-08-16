package com.zoechat.ble

import java.security.SecureRandom
import java.util.UUID

/**
 * zoe BLE 帧协议 —— 与 crates/zoe-transport/src/ble/mod.rs 严格一致:
 *
 *   [magic 0x5A | msg_id 8B | ttl 1B | chunk_idx 1B | total 2B(BE) | data ≤499B]
 *
 * 服务   7a5e0001-2e4c-4a31-9b6c-3c2a0e5f6a01
 * 写     7a5e0002-2e4c-4a31-9b6c-3c2a0e5f6a01(客户端→服务端)
 * 通知   7a5e0003-2e4c-4a31-9b6c-3c2a0e5f6a01(服务端→客户端)
 */
object ZoeFrame {
    const val MAGIC: Byte = 0x5A
    const val HEADER_LEN: Int = 13
    const val MAX_DATA: Int = 499
    const val MAX_TOTAL_CHUNKS: Int = 512

    val SERVICE_UUID: UUID = UUID.fromString("7a5e0001-2e4c-4a31-9b6c-3c2a0e5f6a01")
    val WRITE_UUID: UUID = UUID.fromString("7a5e0002-2e4c-4a31-9b6c-3c2a0e5f6a01")
    val NOTIFY_UUID: UUID = UUID.fromString("7a5e0003-2e4c-4a31-9b6c-3c2a0e5f6a01")

    private val rng = SecureRandom()

    data class Header(
        val msgId: ByteArray,   // 8B
        val ttl: Int,           // u8
        val chunkIdx: Int,      // u8
        val total: Int,         // u16 BE
        val data: ByteArray,
    ) {
        val msgIdHex: String
            get() = msgId.joinToString("") { "%02x".format(it.toInt() and 0xFF) }

        override fun equals(other: Any?): Boolean = other is Header &&
            msgId.contentEquals(other.msgId) &&
            ttl == other.ttl && chunkIdx == other.chunkIdx &&
            total == other.total && data.contentEquals(other.data)

        override fun hashCode(): Int =
            msgId.contentHashCode() * 31 + ttl * 7 + chunkIdx * 3 + total
    }

    /** 解析帧;非法返回 null。 */
    fun parse(frame: ByteArray): Header? {
        if (frame.size < HEADER_LEN || frame[0] != MAGIC) return null
        val total = ((frame[11].toInt() and 0xFF) shl 8) or (frame[12].toInt() and 0xFF)
        if (total == 0 || total > MAX_TOTAL_CHUNKS) return null
        return Header(
            msgId = frame.copyOfRange(1, 9),
            ttl = frame[9].toInt() and 0xFF,
            chunkIdx = frame[10].toInt() and 0xFF,
            total = total,
            data = frame.copyOfRange(HEADER_LEN, frame.size),
        )
    }

    /** 构造一帧(与 Rust frame_chunks 单分片输出一致)。 */
    fun build(msgId: ByteArray, ttl: Int, chunkIdx: Int, total: Int, data: ByteArray): ByteArray {
        require(msgId.size == 8) { "msgId 必须 8 字节" }
        val out = ByteArray(HEADER_LEN + data.size)
        out[0] = MAGIC
        System.arraycopy(msgId, 0, out, 1, 8)
        out[9] = ttl.toByte()
        out[10] = chunkIdx.toByte()
        out[11] = ((total shr 8) and 0xFF).toByte()
        out[12] = (total and 0xFF).toByte()
        System.arraycopy(data, 0, out, HEADER_LEN, data.size)
        return out
    }

    /** 随机 8 字节 msg_id。 */
    fun randomMsgId(): ByteArray = ByteArray(8).also { rng.nextBytes(it) }

    fun hex(bytes: ByteArray): String =
        bytes.joinToString(" ") { "%02x".format(it.toInt() and 0xFF) }
}
