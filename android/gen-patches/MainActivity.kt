package com.zoechat.mobile

import android.Manifest
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge
import com.zoechat.ble.Bridge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // BLE 权限:API 31+ 三件套;≤30 定位(与 android-old MainActivity 一致)
    val perms = if (Build.VERSION.SDK_INT >= 31) {
      arrayOf(
        Manifest.permission.BLUETOOTH_SCAN,
        Manifest.permission.BLUETOOTH_CONNECT,
        Manifest.permission.BLUETOOTH_ADVERTISE,
      )
    } else {
      arrayOf(Manifest.permission.ACCESS_FINE_LOCATION)
    }
    requestPermissions(perms, 100)
    Bridge.start(this)   // 桥生命周期=进程生命周期(坑 10,不用前台 Service)
  }
}
