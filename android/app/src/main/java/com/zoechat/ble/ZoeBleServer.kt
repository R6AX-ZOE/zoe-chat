package com.zoechat.ble

import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCharacteristic
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
    }

    private val appContext: Context = context.applicationContext
    private val bluetoothManager =
        appContext.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
    private val main = Handler(Looper.getMainLooper())

    private var gattServer: BluetoothGattServer? = null
    private var echo = true

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
    )

    private val callback = object : BluetoothGattServerCallback() {

        override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
            main.post {
                when (newState) {
                    BluetoothProfile.STATE_CONNECTED ->
                        listener.onLog("[连接] ${device.address} (${device.name ?: "?"})")
                    BluetoothProfile.STATE_DISCONNECTED ->
                        listener.onLog("[断开] ${device.address}")
                }
            }
        }

        override fun onCharacteristicWriteRequest(
            device: BluetoothDevice,
            requestId: Int,
            characteristic: BluetoothGattCharacteristic,
            preparedWrite: Boolean,
            responseNeeded: Boolean,
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
}
