package com.zoechat.ble

import android.bluetooth.BluetoothManager
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.bluetooth.le.BluetoothLeAdvertiser
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid

/**
 * BLE 广播器:广播 zoe 服务 UUID(connectable)。
 * 注意:不带设备名 —— 128 位服务 UUID 已占 18 字节,系统蓝牙名长度不可控,
 * 再加名字很容易触发 ADVERTISE_FAILED_DATA_TOO_LARGE(31 字节 legacy 载荷上限)。
 * 扫描方(zoe-cli / ble-scan.sh / ble-gatt-test)都按服务 UUID 过滤,设备名非必需。
 */
class ZoeAdvertiser(context: Context, private val onLog: (String) -> Unit) {

    private val bluetoothManager =
        context.applicationContext.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
    private val advertiser: BluetoothLeAdvertiser? = bluetoothManager.adapter?.bluetoothLeAdvertiser
    private val main = Handler(Looper.getMainLooper())
    private var callback: AdvertiseCallback? = null

    val supported: Boolean
        get() = advertiser != null

    fun start(): Boolean {
        val adv = advertiser ?: return false
        val settings = AdvertiseSettings.Builder()
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_HIGH)
            .setConnectable(true)
            .build()
        val data = AdvertiseData.Builder()
            .addServiceUuid(ParcelUuid(ZoeFrame.SERVICE_UUID))
            .build()
        callback = object : AdvertiseCallback() {
            override fun onStartSuccess(settingsInEffect: AdvertiseSettings?) {
                main.post {
                    onLog("[广播] 已开始:服务=${ZoeFrame.SERVICE_UUID}")
                }
            }

            override fun onStartFailure(errorCode: Int) {
                main.post {
                    onLog(
                        "[广播] 启动失败 errorCode=$errorCode" +
                            "(1=数据过大 2=广告实例过多 3=已在广播 4=内部错误 5=特性不支持)"
                    )
                }
            }
        }
        return try {
            adv.startAdvertising(settings, data, callback)
            true
        } catch (e: Exception) {
            onLog("[广播] 异常: ${e.message}")
            false
        }
    }

    fun stop() {
        callback?.let { advertiser?.stopAdvertising(it) }
        callback = null
    }
}
