package com.zoechat.ble

import android.Manifest
import android.app.Activity
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.content.Intent
import android.content.pm.PackageManager
import android.location.LocationManager
import android.os.Build
import android.os.Bundle
import android.view.View
import android.widget.Button
import android.widget.CompoundButton
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast

/**
 * zoe BLE 服务端(手机 peripheral)控制台:
 * 启动后广播 zoe 服务并托管 GATT server,电脑端 zoe-cli ble 可连接联调。
 * 详见 docs/termux-ble.md 方案 B。
 */
class MainActivity : Activity(), ZoeBleServer.Listener {

    companion object {
        private const val REQ_PERMS = 100
        private const val REQ_BT_ON = 101
        private const val LOG_MAX_CHARS = 40_000
        private val NEEDED_PERMS_31 = arrayOf(
            Manifest.permission.BLUETOOTH_SCAN,
            Manifest.permission.BLUETOOTH_CONNECT,
            Manifest.permission.BLUETOOTH_ADVERTISE,
        )
        private val NEEDED_PERMS_LEGACY = arrayOf(Manifest.permission.ACCESS_FINE_LOCATION)
    }

    private lateinit var btnStart: Button
    private lateinit var btnStop: Button
    private lateinit var btnClear: Button
    private lateinit var switchEcho: CompoundButton
    private lateinit var logView: TextView
    private lateinit var scrollView: ScrollView
    private lateinit var statusView: TextView

    private var advertiser: ZoeAdvertiser? = null
    private var server: ZoeBleServer? = null
    private var running = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        btnStart = findViewById(R.id.btnStart)
        btnStop = findViewById(R.id.btnStop)
        btnClear = findViewById(R.id.btnClear)
        switchEcho = findViewById(R.id.switchEcho)
        logView = findViewById(R.id.logView)
        scrollView = findViewById(R.id.scrollView)
        statusView = findViewById(R.id.statusView)

        btnStart.setOnClickListener { onStartClicked() }
        btnStop.setOnClickListener { stopAll(); log("[系统] 已停止") }
        btnClear.setOnClickListener { logView.text = "" }

        log("[系统] zoe BLE 服务端就绪(echo ${if (switchEcho.isChecked) "开" else "关"})")
        log("[系统] 电脑端:zoe-cli ble scan / connect 或 Chrome 打开 tools/ble-gatt-test")
    }

    private fun onStartClicked() {
        if (running) return
        val adapter = BluetoothAdapter.getDefaultAdapter()
        if (adapter == null) {
            toast("设备不支持蓝牙"); return
        }
        if (!adapter.isEnabled) {
            log("[系统] 请求打开蓝牙...")
            startActivityForResult(Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE), REQ_BT_ON)
            return
        }
        if (!checkPermissions()) return
        startAll()
    }

    private fun checkPermissions(): Boolean {
        val perms = if (Build.VERSION.SDK_INT >= 31) NEEDED_PERMS_31 else NEEDED_PERMS_LEGACY
        val missing = perms.filter { checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }
        if (missing.isEmpty()) return true
        if (Build.VERSION.SDK_INT < 31) {
            val lm = getSystemService(LOCATION_SERVICE) as LocationManager
            if (!lm.isLocationEnabled) {
                log("[系统] 请先开启系统定位(Android 12 以下 BLE 扫描依赖定位)")
                toast("请开启定位")
                return false
            }
        }
        requestPermissions(missing.toTypedArray(), REQ_PERMS)
        return false
    }

    override fun onRequestPermissionsResult(
        requestCode: Int, permissions: Array<out String>, grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQ_PERMS) {
            if (grantResults.all { it == PackageManager.PERMISSION_GRANTED }) {
                startAll()
            } else {
                log("[系统] 权限被拒绝,无法广播")
            }
        }
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_BT_ON) {
            if (resultCode == RESULT_OK) {
                if (checkPermissions()) startAll()
            } else {
                log("[系统] 蓝牙未开启")
            }
        }
    }

    private fun startAll() {
        if (running) return
        val adv = ZoeAdvertiser(this) { line -> log(line) }
        val srv = ZoeBleServer(this, this).also { it.setEcho(switchEcho.isChecked) }
        if (!adv.start()) {
            log("[系统] 广播启动失败(检查权限/蓝牙)") 
            return
        }
        if (!srv.start()) {
            log("[系统] GATT server 启动失败")
            adv.stop()
            return
        }
        advertiser = adv
        server = srv
        running = true
        btnStart.isEnabled = false
        btnStop.isEnabled = true
        statusView.text = "运行中:广播 + GATT 服务(zoe-device)"
        log("[系统] 已启动:广播 + GATT 服务(等待电脑连接...)")
    }

    private fun stopAll() {
        advertiser?.stop()
        server?.stop()
        advertiser = null
        server = null
        running = false
        btnStart.isEnabled = true
        btnStop.isEnabled = false
        statusView.text = "已停止"
    }

    override fun onDestroy() {
        stopAll()
        super.onDestroy()
    }

    // ---- ZoeBleServer.Listener ----

    override fun onLog(line: String) {
        log(line)
    }

    override fun onFrame(device: BluetoothDevice, header: ZoeFrame.Header?, raw: ByteArray) {
        // echo 由 server 内部处理;此处仅记录(如需扩展可在此做分片重组/转发)
        if (header != null) {
            log("[帧] ${device.address} ${header.msgIdHex} 片${header.chunkIdx + 1}/${header.total} 数据 ${header.data.size}B")
        }
    }

    // ---- UI 工具 ----

    private fun log(line: String) {
        runOnUiThread {
            logView.append(line + "\n")
            val text = logView.text
            if (text.length > LOG_MAX_CHARS) {
                logView.text = text.subSequence(text.length - LOG_MAX_CHARS, text.length)
            }
            scrollView.post { scrollView.fullScroll(View.FOCUS_DOWN) }
        }
    }

    private fun toast(msg: String) {
        Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
    }
}
