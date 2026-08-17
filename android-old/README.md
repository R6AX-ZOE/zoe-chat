# zoe BLE 服务端(Android)

> 注:本工程已移至 **`android-old/`** —— 旧纯 Kotlin 方案 B,保留作后备;
> 新架构(Tauri 2 + zoe-core)的 Kotlin 侧源码在 `android/`,见 `docs/tauri-mobile.md`。

手机端 zoe **peripheral**(广播 + GATT 服务端)App,让电脑端 `zoe-cli ble` /
Chrome Web Bluetooth 直接连接手机联调。**纯 Kotlin + Android SDK,零第三方依赖**。

与 Rust 侧协议严格一致(`crates/zoe-transport/src/ble/mod.rs`):

```
服务   7a5e0001-2e4c-4a31-9b6c-3c2a0e5f6a01
写     7a5e0002-2e4c-4a31-9b6c-3c2a0e5f6a01(电脑→手机,收帧)
通知   7a5e0003-2e4c-4a31-9b6c-3c2a0e5f6a01(手机→电脑,发帧/echo)
帧     [0x5A | msg_id 8B | ttl 1B | chunk_idx 1B | total 2B BE | data ≤499B]
```

## 构建

方式一(推荐):**Android Studio** 打开本目录(`android-old/`)→ 等待 Gradle Sync →
连接手机(开启 USB 调试)→ Run。

方式二(命令行,需 JDK 17+ 与 Android SDK):

```sh
cd android-old
# 生成 wrapper(或直接用本机 gradle 8.7+)
gradle wrapper
./gradlew assembleDebug
# 产物:app/build/outputs/apk/debug/app-debug.apk
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

## 使用

1. 打开 App → 授权(Android 12+ 会请求"附近的设备"权限;Android 12 以下需开启定位);
2. 点**启动**:App 开始广播(名称 = 手机系统蓝牙名,可在 设置→蓝牙 修改)并托管 GATT 服务;
3. 电脑端联调(二选一):

```sh
# 方式一:zoe-cli(Windows/Linux)
zoe-cli ble scan --timeout 10                 # 找到手机 MAC
zoe-cli ble connect AA:BB:CC:DD:EE:FF --send 5a01020304050607080100000168656c6c6f
# 手机 App 日志显示 [收] 帧...;[发] 通知已送达 = echo 成功

# 方式二:电脑 Chrome 打开 tools/ble-gatt-test/index.html → 扫描并连接 → 发帧/echo 测试
```

4. echo 开关:开启时,收到的每一帧会原样通过通知特性回发(对应 `zoe-cli ble adv --echo`);
5. App 日志显示:连接/断开、收帧(解析 msg_id/分片)、通知发送结果。

## 已知限制

- 广播名使用手机系统蓝牙名(Android 广告 API 不支持自定义 LocalName);
  对端按服务 UUID 过滤即可,不影响联调;
- 单帧 ≤ 512B(13B 头 + 499B 数据);分片/重组逻辑在电脑端 `zoe-cli` 与
  Rust MeshOverlay 中完成,App 目前只做单帧收发与 echo;
- Android 8.1+ 熄屏后 BLE 广告会停止:调试时保持屏幕亮起(或开"开发者选项 →
  屏幕常亮")。

## 后续(可选)

- 将 `zoe-core`(帧/身份/MLS)通过 cargo-ndk + JNI 编译进 App,在手机端跑完整
  MeshOverlay(参考 blew 的 Android 集成方式:`JNI_OnLoad` + Kotlin 桥接);
- 增加手动"发送测试通知"按钮与分片发送,方便无电脑场景自测。
