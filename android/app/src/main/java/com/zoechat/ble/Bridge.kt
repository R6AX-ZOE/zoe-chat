package com.zoechat.ble

import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.content.Context
import android.os.Handler
import android.os.Looper
import org.json.JSONObject
import java.io.BufferedReader
import java.io.BufferedWriter
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.net.InetSocketAddress
import java.net.Socket

/**
 * zoe-mobile 回环 TCP 桥客户端(M1,规格见 docs/tauri-mobile.md)。
 *
 * 连接 Rust 侧 127.0.0.1:18570;协议为每行一个 JSON 对象(UTF-8,字节载荷 hex 小写):
 *   K→R: {"t":"hello","v":1} / {"t":"frame","a":"<mac>","d":"<hex>"} / {"t":"log","d":"..."}
 *   R→K: {"t":"start","n":"<广播名>"} / {"t":"stop"} / {"t":"send","a":"<mac>","d":"<hex>"} / {"t":"echo","v":bool}
 *
 * 线程模型:独立 socket 线程;BLE 回调经主线程 Handler 转发(沿用 ZoeBleServer
 * "binder 线程 → main.post" 模式);发送经 writeLock 互斥(hello 在 socket 线程,
 * log/frame 在 main 线程)。断线每 2s 重连,幂等。
 */
object Bridge {

    private const val HOST = "127.0.0.1"
    private const val PORT = 18570
    private const val CONNECT_TIMEOUT_MS = 3000
    private const val RECONNECT_DELAY_MS = 2000L
    private const val DEFAULT_ADV_NAME = "zoe-device"

    @Volatile
    private var running = false
    private var thread: Thread? = null

    private var appContext: Context? = null
    private var advertiser: ZoeAdvertiser? = null
    private var server: ZoeBleServer? = null
    private var echo = true

    private val main = Handler(Looper.getMainLooper())

    /** socket 写端;仅在连接建立后非空。 */
    private val writeLock = Any()
    private var socket: Socket? = null
    private var writer: BufferedWriter? = null

    /** 已知 GATT 客户端(mac → device);send 时优先查表,缺省回退 getRemoteDevice。 */
    private val devices = HashMap<String, BluetoothDevice>()

    /** 启动桥(幂等)。权限/蓝牙开关由调用方(Activity)先处理好。 */
    @Synchronized
    fun start(context: Context) {
        if (running) return
        running = true
        appContext = context.applicationContext
        val t = Thread({ runLoop() }, "zoe-bridge")
        thread = t
        t.start()
    }

    @Synchronized
    fun stop() {
        running = false
        closeSocket()
        thread = null
        main.post { stopBle() }
    }

    // ---- socket 循环(非主线程) ----

    private fun runLoop() {
        while (running) {
            try {
                val s = Socket()
                s.connect(InetSocketAddress(HOST, PORT), CONNECT_TIMEOUT_MS)
                socket = s
                val w = BufferedWriter(OutputStreamWriter(s.getOutputStream(), Charsets.UTF_8))
                val r = BufferedReader(InputStreamReader(s.getInputStream(), Charsets.UTF_8))
                synchronized(writeLock) { writer = w }
                sendNow("hello", JSONObject().put("v", 1))
                log("[桥] 已连接 Rust($HOST:$PORT)")
                while (running) {
                    val line = r.readLine() ?: break
                    if (line.isNotBlank()) onCommand(line)
                }
            } catch (e: Exception) {
                if (running) {
                    log("[桥] 连接异常: ${e.message ?: e.javaClass.simpleName}," +
                        "${RECONNECT_DELAY_MS / 1000}s 后重连")
                }
            } finally {
                closeSocket()
            }
            if (!running) break
            try {
                Thread.sleep(RECONNECT_DELAY_MS)
            } catch (_: InterruptedException) {
                break
            }
        }
    }

    private fun closeSocket() {
        synchronized(writeLock) {
            writer = null
            try {
                socket?.close()
            } catch (_: Exception) {
            }
            socket = null
        }
    }

    /** 一行 JSON 命令;无法解析/未知类型只记日志,不中断连接。 */
    private fun onCommand(line: String) {
        try {
            val o = JSONObject(line)
            when (o.optString("t")) {
                "start" -> main.post { startBle(o.optString("n", DEFAULT_ADV_NAME)) }
                "stop" -> main.post { stopBle() }
                "send" -> main.post { sendFrame(o.optString("a"), o.optString("d")) }
                "echo" -> main.post { setEcho(o.optBoolean("v", true)) }
                else -> log("[桥] 未知命令: ${o.optString("t")}")
            }
        } catch (e: Exception) {
            log("[桥] 坏消息已丢弃: ${e.message}")
        }
    }

    // ---- BLE(主线程) ----

    private fun startBle(name: String) {
        if (server != null) return
        val ctx = appContext ?: return
        val adv = ZoeAdvertiser(ctx) { line -> log(line) }
        val srv = ZoeBleServer(ctx, listener).also { it.setEcho(echo) }
        if (!adv.start()) {
            log("[桥] 广播启动失败(检查蓝牙/权限)")
            return
        }
        if (!srv.start()) {
            log("[桥] GATT server 启动失败")
            adv.stop()
            return
        }
        advertiser = adv
        server = srv
        log("[桥] BLE 已启动(广播名=$name, echo=${if (echo) "开" else "关"})")
    }

    private fun stopBle() {
        advertiser?.stop()
        advertiser = null
        server?.stop()
        server = null
        log("[桥] BLE 已停止")
    }

    private fun setEcho(v: Boolean) {
        echo = v
        server?.setEcho(v)
        log("[桥] echo=${if (v) "开" else "关"}")
    }

    /** 向指定 mac 发一帧(hex);设备未订阅通知时 sendNotification 返回 false。 */
    private fun sendFrame(mac: String, hexData: String) {
        val srv = server
        val dev = devices[mac] ?: run {
            try {
                BluetoothAdapter.getDefaultAdapter()?.getRemoteDevice(mac)
            } catch (e: Exception) {
                log("[桥] getRemoteDevice($mac) 失败: ${e.message}")
                null
            }
        }
        if (srv == null || dev == null) {
            log("[桥] 发送失败: server=${srv != null} device=${dev != null} ($mac)")
            return
        }
        val bytes = hexToBytes(hexData)
        log("[桥] 发送 ${bytes.size}B → $mac")
        srv.sendNotification(dev, bytes)
    }

    // ---- ZoeBleServer.Listener(主线程) ----

    private val listener = object : ZoeBleServer.Listener {
        override fun onLog(line: String) {
            log(line)
        }

        override fun onFrame(device: BluetoothDevice, header: ZoeFrame.Header?, raw: ByteArray) {
            devices[device.address] = device
            sendNow(
                "frame",
                JSONObject()
                    .put("a", device.address)
                    .put("d", hex(raw))
            )
        }
    }

    // ---- 发送(K→R) ----

    private fun log(line: String) {
        sendNow("log", JSONObject().put("d", line))
    }

    private fun sendNow(t: String, o: JSONObject) {
        o.put("t", t)
        val line = o.toString()
        synchronized(writeLock) {
            val w = writer ?: return
            try {
                w.write(line)
                w.newLine()
                w.flush()
            } catch (_: Exception) {
                // 写失败:readLine 随后退出,由重连兜底
            }
        }
    }

    private fun hex(bytes: ByteArray): String =
        bytes.joinToString("") { "%02x".format(it) }

    /** hex(容忍空白/大小写,忽略非法字符;奇数长度丢末尾半字节) → 字节。 */
    private fun hexToBytes(hexData: String): ByteArray {
        val clean = hexData.filter { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }
        val out = ByteArray(clean.length / 2)
        for (i in out.indices) {
            out[i] = clean.substring(i * 2, i * 2 + 2).toInt(16).toByte()
        }
        return out
    }
}
