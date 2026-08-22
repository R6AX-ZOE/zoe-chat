package com.zoechat.ble

import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothManager
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
import android.util.Log

/**
 * BLE GATT 客户端(手机↔手机):扫描 zoe 服务 → 连接 → 订阅通知 → 收发帧。
 *
 * 服务端(ZoeBleServer)只能被动等别人连;两台手机互发消息必须有一方主动连接,
 * 因此双方都跑本扫描器:发现对端广播 → 连接 → 订阅 NOTIFY;Rust 经桥下发的
 * "send" 命令由 writeCharacteristic 写入对端 WRITE 特性,对端 GATT 服务端
 * 回调 → onFrame → 桥 → Rust(与直接连接同一入站路径)。
 */
class ZoeScanner(context: Context, private val listener: Listener) {

    interface Listener {
        fun onLog(line: String)
        /** 客户端连上对端(进入 SIG Mesh 邻居)。 */
        fun onClientConnected(device: BluetoothDevice)
        fun onClientDisconnected(device: BluetoothDevice)
        /** 从对端通知/读取到的原始字节。 */
        fun onFrame(device: BluetoothDevice, raw: ByteArray)
    }

    private val appContext: Context = context.applicationContext
    private val bluetoothManager =
        appContext.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
    private val main = Handler(Looper.getMainLooper())
    private val tag = "ZoeScanner"

    private var scanner: android.bluetooth.le.BluetoothLeScanner? = null
    private var scanning = false

    /** 已连接/连接中的设备(避免重复连接)。 */
    private val connected = HashMap<String, BluetoothGatt>()
    private val connecting = HashSet<String>()

    private val scanCallback = object : ScanCallback() {
        override fun onScanResult(callbackType: Int, result: ScanResult) {
            val d = result.device
            val mac = d.address
            if (connected.containsKey(mac) || connecting.contains(mac)) return
            connecting.add(mac)
            main.post {
                listener.onLog("[扫] 发现 ${d.name ?: "?"} ($mac),连接中…")
                connect(d)
            }
        }

        override fun onScanFailed(errorCode: Int) {
            main.post { listener.onLog("[扫] 扫描失败 errorCode=$errorCode") }
        }
    }

    fun start(): Boolean {
        val adapter = bluetoothManager.adapter ?: return false
        val s = adapter.bluetoothLeScanner ?: return false
        val filter = ScanFilter.Builder()
            .setServiceUuid(ParcelUuid(ZoeFrame.SERVICE_UUID))
            .build()
        val settings = ScanSettings.Builder()
            .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
            .build()
        scanner = s
        return try {
            s.startScan(listOf(filter), settings, scanCallback)
            scanning = true
            listener.onLog("[扫] 扫描已启动(服务=${ZoeFrame.SERVICE_UUID})")
            true
        } catch (e: Exception) {
            listener.onLog("[扫] 扫描启动失败: ${e.message}")
            false
        }
    }

    fun stop() {
        if (scanning) {
            try {
                scanner?.stopScan(scanCallback)
            } catch (_: Exception) {
            }
            scanning = false
        }
        synchronized(connected) {
            for (g in connected.values) {
                try {
                    g.disconnect()
                    g.close()
                } catch (_: Exception) {
                }
            }
            connected.clear()
        }
        connecting.clear()
    }

    private fun connect(device: BluetoothDevice) {
        val gatt = device.connectGatt(
            appContext, false,
            object : BluetoothGattCallback() {
                override fun onConnectionStateChange(g: BluetoothGatt, status: Int, newState: Int) {
                    when (newState) {
                        android.bluetooth.BluetoothProfile.STATE_CONNECTED -> {
                            main.post { listener.onLog("[扫] 已连接 $device") }
                            g.discoverServices()
                        }
                        android.bluetooth.BluetoothProfile.STATE_DISCONNECTED -> {
                            connecting.remove(device.address)
                            connected.remove(device.address)?.let { g2 ->
                                try {
                                    g2.close()
                                } catch (_: Exception) {
                                }
                            }
                            main.post {
                                listener.onLog("[扫] 断开 $device")
                                listener.onClientDisconnected(device)
                            }
                        }
                    }
                }

                override fun onServicesDiscovered(g: BluetoothGatt, status: Int) {
                    if (status != BluetoothGatt.GATT_SUCCESS) {
                        main.post {
                            listener.onLog("[扫] 服务发现失败($status) $device")
                        }
                        g.disconnect()
                        return
                    }
                    val service = g.getService(ZoeFrame.SERVICE_UUID) ?: run {
                        main.post {
                            listener.onLog("[扫] 未找到 zoe 服务 $device")
                        }
                        g.disconnect()
                        return
                    }
                    val notify = service.getCharacteristic(ZoeFrame.NOTIFY_UUID)
                    if (notify == null) {
                        main.post {
                            listener.onLog("[扫] 无通知特性 $device")
                        }
                        g.disconnect()
                        return
                    }
                    connected[device.address] = g
                    connecting.remove(device.address)
                    g.setCharacteristicNotification(notify, true)
                    val cccd = notify.getDescriptor(ZoeFrame.CCCD_UUID)
                    if (cccd != null) {
                        cccd.value = byteArrayOf(0x01, 0x00)
                        try {
                            g.writeDescriptor(cccd)
                        } catch (_: Exception) {
                        }
                    }
                    main.post {
                        listener.onLog("[扫] 订阅完成 $device")
                        listener.onClientConnected(device)
                    }
                }

                override fun onCharacteristicChanged(
                    g: BluetoothGatt,
                    characteristic: BluetoothGattCharacteristic,
                    value: ByteArray,
                ) {
                    main.post {
                        listener.onFrame(device, value.copyOf())
                    }
                }
            }
        )
        if (gatt == null) {
            connecting.remove(device.address)
        }
    }

    /** 向对端写一帧(WRITE_NO_RESPONSE 特性)。 */
    fun write(device: BluetoothDevice, bytes: ByteArray): Boolean {
        val gatt = connected[device.address] ?: return false
        val service = gatt.getService(ZoeFrame.SERVICE_UUID) ?: return false
        val writeChar = service.getCharacteristic(ZoeFrame.WRITE_UUID) ?: return false
        writeChar.value = bytes
        return try {
            gatt.writeCharacteristic(writeChar)
        } catch (e: Exception) {
            Log.w(tag, "write failed: ${e.message}")
            false
        }
    }

    /** 向所有已连接的客户端写(SIG Mesh 洪泛)。 */
    fun broadcast(bytes: ByteArray): Int {
        var sent = 0
        val devices = connected.keys.toList()
        for (mac in devices) {
            val gatt = connected[mac] ?: continue
            val service = gatt.getService(ZoeFrame.SERVICE_UUID) ?: continue
            val writeChar = service.getCharacteristic(ZoeFrame.WRITE_UUID) ?: continue
            writeChar.value = bytes
            try {
                if (gatt.writeCharacteristic(writeChar)) sent++
            } catch (_: Exception) {
            }
        }
        return sent
    }
}
