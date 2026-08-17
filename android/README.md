# zoe-mobile Android 侧源码（canonical Kotlin）

手机端 zoe-mobile（Tauri 2，见 `docs/tauri-mobile.md`）的 **Android 侧源码目录**：
Kotlin BLE 物理层 + 回环 TCP 桥客户端。纯 Kotlin + Android SDK + `org.json`，**零第三方依赖**。

## 布局

```
android/
  README.md
  app/src/main/java/com/zoechat/ble/
    ZoeFrame.kt        # 帧协议(与 crates/zoe-transport/src/ble/mod.rs 严格一致)
    ZoeBleServer.kt    # GATT server(写/通知 + echo,CCCD 显式描述符已修)
    ZoeAdvertiser.kt   # BLE 广播(zoe 服务 UUID)
    Bridge.kt          # 回环 TCP 桥客户端(连 Rust 127.0.0.1:18570,断线 2s 重连)
```

桥协议消息表、线程模型、生命周期见 `docs/tauri-mobile.md` §M1（Kotlin 侧设计）。

## 与 Tauri 工程的衔接（M1）

本目录是 canonical 源码；`app/src-tauri/gen/android/` 由 `npx tauri android init`
生成并提交，其中的 `com/zoechat/ble/*.kt` 是**同步产物（勿手改）**。改动流程：

1. 改本目录 `android/` 下的 Kotlin；
2. 运行同步脚本把 `ble/*.kt` 覆盖进 `gen/android`（CI 构建前也会跑一次）：

   ```sh
   bash scripts/sync-android-ble.sh        # CI(ubuntu)/Linux
   powershell scripts/sync-android-ble.ps1 # Windows 本地
   ```

3. 另需在 Tauri 生成的工程里改两处（不入本目录，避免与 tauri CLI 再生成冲突）：
   - `gen/android/app/src/main/java/com/zoechat/mobile/MainActivity.kt`：
     onCreate 里申请权限 + `Bridge.start(this)`；权限流程照抄 `android-old/` 的
     MainActivity.checkPermissions（BLUETOOTH_SCAN/CONNECT/ADVERTISE，≤30 用定位）；
   - `gen/android/app/src/main/AndroidManifest.xml` 增加权限：

   ```xml
   <uses-permission android:name="android.permission.BLUETOOTH_SCAN"/>
   <uses-permission android:name="android.permission.BLUETOOTH_CONNECT"/>
   <uses-permission android:name="android.permission.BLUETOOTH_ADVERTISE"/>
   <uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" android:maxSdkVersion="30"/>
   <uses-feature android:name="android.hardware.bluetooth_le" android:required="true"/>
   ```

## 与旧工程的关系

- `android-old/`：旧纯 Kotlin APK 工程（方案 B，广播 + GATT + echo 的独立 App），
  保留作后备；其 `android.yml` CI 继续构建。
- `android/`：新架构（Tauri + zoe-core）的 Kotlin 侧，不独立出 APK，
  随 `app/` 的 Tauri Android 构建一起进包。

## 联调命令（Windows central ↔ 手机 peripheral）

```sh
zoe-cli ble scan --timeout 10                          # 找到手机 MAC
zoe-cli ble connect AA:BB:CC:DD:EE:FF --send 5a010203040506070809000001ab   # 合法测试帧 → echo
```

详见 `docs/termux-ble.md`（方案 A'）与 `docs/tauri-mobile.md`（M1 验收）。
