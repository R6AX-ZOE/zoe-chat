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
 * 注:Android 的 AdvertiseData 没有自定义 LocalName 字段,
 * setIncludeDeviceName 使用手机系统蓝牙名称(设置 → 蓝牙 → 设备名 可改);
 * 对端按服务 UUID 过滤即可(与 tools/ble-gatt-test 一致)。
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
            .setIncludeDeviceName(true)
            .build()
        callback = object : AdvertiseCallback() {
            override fun onStartSuccess(settingsInEffect: AdvertiseSettings?) {
                main.post {
                    onLog("[广播] 已开始:名称=${bluetoothManager.adapter?.name ?: "?"} 服务=${ZoeFrame.SERVICE_UUID}")
                }
            }

            override fun onStartFailure(errorCode: Int) {
                main.post {
                    onLog("[广播] 启动失败 errorCode=$errorCode(1=数据过大 2=太频繁 3=数据非法 4=不支持 5=内部错误)")
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
