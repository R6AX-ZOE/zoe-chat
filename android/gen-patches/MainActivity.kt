package com.zoechat.mobile

import android.Manifest
import android.content.Intent
import android.net.wifi.WifiManager
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
    // 局域网传输:Wi-Fi 组播锁(Rust lan.rs 组播信标需要;漏掉会收不到对端信标)
    try {
      val wifi = getSystemService(WIFI_SERVICE) as WifiManager
      wifi.createMulticastLock("zoe-lan").apply {
        setReferenceCounted(false)
        acquire()
      }
    } catch (e: Exception) {
      android.util.Log.w("ZoeMain", "multicast lock failed: ${e.message}")
    }
    Bridge.start(this)   // 桥生命周期=进程生命周期(坑 10,不用前台 Service)
    attach(this)
  }

  companion object {
    @Volatile private var current: MainActivity? = null

    fun attach(activity: MainActivity) {
      current = activity
    }

    /**
     * 前端"重启服务"命令入口(由 Rust relaunch.rs 经 JNI 调用,无参静态方法):
     * 重建 Activity + 杀进程冷启动。冷启动后内嵌守护进程以 PIN 用户无 --pin
     * 启动 → 锁定模式 → Web UI 直接显示锁定屏。
     */
    @JvmStatic
    fun restartApp() {
      val a = current
      if (a != null) {
        a.runOnUiThread {
          a.startActivity(
            Intent(a, MainActivity::class.java)
              .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TASK or Intent.FLAG_ACTIVITY_NEW_TASK)
          )
        }
      }
      // 重建 intent 已交给系统后结束本进程(标准冷启动方式)
      android.os.Process.killProcess(android.os.Process.myPid())
    }
  }
}