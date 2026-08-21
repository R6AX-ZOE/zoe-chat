package com.zoechat.ble

import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothGattServer
import android.bluetooth.BluetoothGattServerCallback
import android.bluetooth.BluetoothGattService
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.content.Context
import android.os.Handler
import android.os.Looper

/**
 * zoe GATT 服务端(peripheral):托管 zoe 服务,
 * 写特性收到帧 → 解析/回显;通知特性用于向客户端回发帧。
 * 回调在 binder 线程,统一切主线程处理;value 数组先 copyOf(框架可能复用)。
 */
class ZoeBleServer(context: Context, private val listener: Listener) {

    interface Listener {
        fun onLog(line: String)
        fun onFrame(device: BluetoothDevice, header: ZoeFrame.Header?, raw: ByteArray)
        /** 设备订阅 NOTIFY(进入 SIG Mesh 洪泛邻居)。 */
        fun onDev(device: BluetoothDevice)
        /** 设备取消订阅/断开(移出 SIG Mesh 洪泛邻居)。 */
        fun onUndev(device: BluetoothDevice)
    }

    private val appContext: Context = context.applicationContext
    private val bluetoothManager =
        appContext.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
    private val main = Handler(Looper.getMainLooper())

    private var gattServer: BluetoothGattServer? = null
    private var echo = true

    /** 已订阅 NOTIFY 的设备(SIG Mesh 广播目标 + 连接状态回传)。 */
    private val subscribed = HashSet<BluetoothDevice>()

    private val writeChar = BluetoothGattCharacteristic(
        ZoeFrame.WRITE_UUID,
        BluetoothGattCharacteristic.PROPERTY_WRITE or
            BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE,
        BluetoothGattCharacteristic.PERMISSION_WRITE
    )
    private val notifyChar = BluetoothGattCharacteristic(
        ZoeFrame.NOTIFY_UUID,
        BluetoothGattCharacteristic.PROPERTY_NOTIFY,
        0
    ).apply {
        // 显式添加 CCCD(0x2902):框架自动添加的 CCCD 在部分机型上权限为 0,
        // 客户端订阅(写 CCCD)会收到 WriteNotPermitted(ATT 0x03)导致连接失败。
        addDescriptor(
            BluetoothGattDescriptor(
                ZoeFrame.CCCD_UUID,
                BluetoothGattDescriptor.PERMISSION_READ or
                    BluetoothGattDescriptor.PERMISSION_WRITE
            )
        )
    }

    private val callback = object : BluetoothGattServerCallback() {

        override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
            main.post {
                when (newState) {
                    BluetoothProfile.STATE_CONNECTED ->
                        listener.onLog("[连接] ${device.address} (${device.name ?: "?"})")
                    BluetoothProfile.STATE_DISCONNECTED -> {
                        listener.onLog("[断开] ${device.address}")
                        if (subscribed.remove(device)) listener.onUndev(device)
                    }
                }
            }
        }

        override fun onCharacteristicWriteRequest(
            device: BluetoothDevice,
            requestId: Int,
            characteristic: BluetoothGattCharacteristic,
            preparedWrite: Boolean,
            responseNeeded: Boolean,
            offset: Int,
            value: ByteArray?
        ) {
            if (characteristic.uuid != ZoeFrame.WRITE_UUID) return
            // 立即回复,避免客户端等待(WRITE 属性)
            if (responseNeeded) {
                gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, null)
            }
            val raw = value?.copyOf() ?: ByteArray(0)
            main.post {
                val header = ZoeFrame.parse(raw)
                val summary = if (header != null) {
                    "帧 msg_id=${header.msgIdHex} ttl=${header.ttl} 片 ${header.chunkIdx + 1}/${header.total} 数据 ${header.data.size}B"
                } else {
                    "原始 ${raw.size}B(非 zoe 帧)"
                }
                listener.onLog("[收] ${device.address} $summary")
                listener.onFrame(device, header, raw)
                if (echo) {
                    sendNotification(device, raw)
                }
            }
        }

        override fun onNotificationSent(device: BluetoothDevice, status: Int) {
            main.post {
                listener.onLog(
                    if (status == BluetoothGatt.GATT_SUCCESS) {
                        "[发] 通知已送达 ${device.address}"
                    } else {
                        "[发] 通知失败(status=$status) ${device.address}(未订阅通知?)"
                    }
                )
            }
        }

        override fun onDescriptorWriteRequest(
            device: BluetoothDevice,
            requestId: Int,
            descriptor: BluetoothGattDescriptor,
            preparedWrite: Boolean,
            responseNeeded: Boolean,
            offset: Int,
            value: ByteArray?
        ) {
            // CCCD(0x2902)订阅写入:栈通常自动处理,但部分机型会回调到 App;
            // 不显式应答会让客户端等到超时(表现为主机侧 Unreachable)。这里统一应答。
            if (responseNeeded) {
                gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, null)
            }
            // 维护订阅集合(SIG Mesh 广播目标;0x0001=enable,0x0000=disable)
            if (descriptor.uuid == ZoeFrame.CCCD_UUID) {
                val v = value?.copyOf()
                val enabled = v != null && v.size == 2 &&
                    ((v[0].toInt() and 0xFF) or ((v[1].toInt() and 0xFF) shl 8)) != 0
                if (enabled) {
                    if (subscribed.add(device)) listener.onDev(device)
                } else {
                    subscribed.remove(device)
                }
            }
        }
    }

    fun setEcho(enabled: Boolean) {
        echo = enabled
    }

    /** 启动 GATT server;失败返回 false。 */
    fun start(): Boolean {
        val server = bluetoothManager.openGattServer(appContext, callback) ?: return false
        val service = BluetoothGattService(
            ZoeFrame.SERVICE_UUID,
            BluetoothGattService.SERVICE_TYPE_PRIMARY
        )
        service.addCharacteristic(writeChar)
        service.addCharacteristic(notifyChar)
        if (!server.addService(service)) {
            server.close()
            return false
        }
        gattServer = server
        return true
    }

    fun stop() {
        gattServer?.close()
        gattServer = null
    }

    /** 向指定设备发一帧(需该设备已订阅 NOTIFY 特性)。 */
    fun sendNotification(device: BluetoothDevice, bytes: ByteArray): Boolean {
        notifyChar.value = bytes
        return gattServer?.notifyCharacteristicChanged(device, notifyChar, false) ?: false
    }

    /** SIG Mesh 洪泛:向所有已订阅设备广播 PDU。 */
    fun sendBroadcast(bytes: ByteArray): Int {
        notifyChar.value = bytes
        var sent = 0
        for (d in subscribed) {
            if (gattServer?.notifyCharacteristicChanged(d, notifyChar, false) == true) sent++
        }
        return sent
    }
}
