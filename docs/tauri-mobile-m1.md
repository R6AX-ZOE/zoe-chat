# zoe-mobile M1 交接（新会话开工文本）

> 用途：新会话从零开始执行 **M1（回环 TCP 桥 + Kotlin BLE 接入）**。
> 先读 `docs/tauri-mobile.md`（设计总纲 + 17 条关键坑 + M0 验收记录），再按本文件执行。
> 当前 HEAD：`aaa5d80`（M0 已闭环：CI 绿 + 真机验收通过，见主文档"验收记录"）。

## 1. 仓库现状速览

```
android-old/            # 旧纯 Kotlin 方案 B 后备工程（android.yml CI 继续构建）
android/                # canonical Kotlin：ZoeFrame/ZoeBleServer/ZoeAdvertiser/Bridge.kt（M1 已就绪）
app/                    # Tauri 2 移动端（前端 Vite + src-tauri Rust）
  src-tauri/src/lib.rs  # run() + app_info/hello_frame（#[tauri::command]，注意：函数不要 pub，坑 16）
  src-tauri/tauri.conf.json      # 有 label:"main" 窗口（坑 17，空数组=黑屏）
  src-tauri/tauri.android.conf.json  # { "bundle": { "android": { "minSdkVersion": 26 } } }
  src-tauri/gen/android/         # CI 生成，不入库（.gitignore）！M1 的 Kotlin 接入须走 patch 脚本（见 §4.4）
scripts/sync-android-ble.sh|.ps1  # android/ble/*.kt → gen/android（M1 起 CI 用）
.github/workflows/android-tauri.yml  # 共享单测 + init + build + 验 APK + 报告推 ci/report
crates/zoe-transport/    # ble-mobile feature 已加；帧/MeshOverlay 为 mobile 与 Linux 共用
```

## 2. 环境事实（实测，勿重踩）

1. **网络**：crates.io / GitHub REST API / dl.google.com 均不可达；`git push` 走
   `-c http.proxy=http://127.0.0.1:58591` 且需提权（Git Credential Manager 进程被沙箱拦，
   用 `sandbox_permissions=danger-full-access` 重试一次同命令即可，已在会话中多次获批）。
2. **npm**：`npm_config_proxy=http://127.0.0.1:58591` + 默认 npmmirror 镜像可用；
   必须 `--cache <项目内目录>`（全局 npm 缓存被 DSH 占用）+ `--ignore-scripts`
   （esbuild/tauri postinstall 在沙箱 EPERM）。`app/package-lock.json` 已提交。
3. **cargo 本地**：offline 可用（CARGO_HOME=`.cargo-home`），但 bluer 的 bluetoothd 依赖
   无索引缓存 → 本地验证时**临时去掉** `crates/zoe-transport/Cargo.toml` 里 bluer 的
   `features=["bluetoothd"]`，跑完**恢复并 restore Cargo.lock**（offline 解析会改写锁文件，勿提交）。
   已验证：`cargo test -p zoe-transport --no-default-features --features ble-mobile --lib` 5/5 绿。
4. **CI**：`android-tauri.yml` 每轮约 15-20 分钟（cargo 缓存已配）。结果经 **`ci/report` 分支**回传
   （force push 覆盖，可能有滞后；真机/日志验证为准）。轮询写法：

   ```powershell
   git -c http.proxy=http://127.0.0.1:58591 fetch -q origin ci/report:refs/remotes/origin/ci/report
   git show refs/remotes/origin/ci/report:ci/m0-report.txt   # job/run/apk/sha256/badging/失败时构建日志尾
   ```

5. **真机 adb**：本机 `E:\_Victor_Programming\adb\adb.exe`，手机已连接（小米 Android 15/16，
   `hqqkbaqsqsdmbaaq`）。可全程自测：`adb install -r`、`adb logcat`、`adb shell uiautomator dump`、
   `adb shell am start -n com.zoechat.mobile/.MainActivity`、`adb shell pidof com.zoechat.mobile`。
6. **关键坑速记**（详见主文档）：坑 16 `#[tauri::command]` 勿 pub；坑 17 `app.windows` 不能为空；
   坑 13 平台配置文件名是 `tauri.android.conf.json`（`bundle.android` 结构）；坑 4/13 gen/android 不入库。

## 3. M1 目标与验收（主文档原文要点）

| 内容 | 验收 |
|---|---|
| 回环 TCP 桥 + Kotlin BLE 服务接入（广播+GATT+echo） | 功能等价现有 APK；Windows `ble connect --send` 回显 |

1. 装新 APK，点"启动 BLE 服务"：`zoe-cli ble scan` 看到 `zoe-device` 广播，GATT 可连
   （服务/写/通知 UUID 齐全，CCCD 可订阅——ZoeBleServer 已验证）。
2. Windows：`zoe-cli ble connect <MAC> --send 5a010203040506070809000001ab` → 手机 echo 通知帧，
   手机日志显示 `帧 msg_id=0102030405060708 ttl=9 片 1/1 数据 1B`。
3. App 控制台日志区实时显示 `[收]/[发]`（K→R→UI 全链路）。
4. App 内 echo 开关即时生效（关闭后不再回显）。

## 4. M1 任务清单（按顺序执行）

### 4.1 zoe-transport 无改动（ble-mobile 已就绪，共享代码直接复用）

### 4.2 Rust：`app/src-tauri/src/bridge.rs`（新建）

规格（主文档"桥协议完整规格"原文）：

- 传输：tokio `TcpListener` 只绑 `127.0.0.1:18570`；Kotlin 侧主动连接，断线 2s 重连（幂等）。
- 成帧：**每行一个 JSON**（newline-delimited，UTF-8，字节载荷 hex 小写）；坏行丢弃+记日志；
  未知类型忽略+记日志。
- 消息表：

| 方向 | 消息 | 说明 |
|---|---|---|
| K→R | `{"t":"hello","v":1}` | 连接建立后 Kotlin 立即发；收到后状态置 Connected，发 bridge-log |
| K→R | `{"t":"frame","a":"<mac>","d":"<hex>"}` | BLE 收到完整帧字节（含 13B 头） |
| K→R | `{"t":"log","d":"<text>"}` | 任意日志行 → 转发 bridge-log 事件 |
| R→K | `{"t":"start","n":"<广播名>"}` | 启动广播+GATT；`n` 可选，缺省 `zoe-device` |
| R→K | `{"t":"stop"}` | 停止（保持 TCP） |
| R→K | `{"t":"send","a":"<mac>","d":"<hex>"}` | 向指定设备发通知帧 |
| R→K | `{"t":"echo","v":bool}` | 对应 `ZoeBleServer.setEcho` |

- 状态机：`BridgeState { Disconnected, Connected }`（Mutex 单例 + 命令通道）；accept 循环常驻；
  accept 后等 Kotlin hello（超时 5s 断开等重连）；写失败/关闭 → Disconnected + bridge-log，继续 accept。
- 命令（tauri command，异步、幂等）：`start_bridge()`（幂等起监听）、`stop_bridge()`（发 stop）、
  `set_echo(v: bool)`、`bridge_status() -> {connected: bool, last_error: Option<String>}`。
- 事件：`app.emit("bridge-log", line)`（tauri `Emitter` trait）。

### 4.3 `app/src-tauri/src/lib.rs` 增补

- `mod bridge;`（或按 crate 结构放入）+
  `invoke_handler` 增加 `start_bridge / stop_bridge / set_echo / bridge_status`。
- **命令函数保持非 pub**（坑 16）。
- 前端 `app/src/main.js` 增加按钮：启动 BLE（start_bridge）、停止（stop_bridge）、echo 开关
  （set_echo）+ `bridge_status` 轮询显示；`listen("bridge-log")` 已预留。

### 4.4 Kotlin 接入 —— 关键决策：gen/android 不入库，走 CI patch 脚本

gen/android 由 CI 每次 `npx tauri android init` 生成（本地无 cmdline-tools 生成不了，坑 4），
所以 **MainActivity/Manifest 的修改不能直接改文件，用 patch 脚本在 CI 覆盖**：

新建 `scripts/patch-android-gen.sh`（CI ubuntu，在 init 之后、build 之前运行），做两件事：

1. **覆盖 `gen/android/app/src/main/java/com/zoechat/mobile/MainActivity.kt`** 为：

   ```kotlin
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
   ```

   注意：`TauriActivity` 基类与生成的模板一致（无 import，与生成版同包解析；生成版能编译，
   patch 版保持同构即可）。`Bridge` 在 `com.zoechat.ble` 包（由 sync 脚本注入）。

2. **覆盖 `gen/android/app/src/main/AndroidManifest.xml`**：在模板基础上加权限
   （模板结构：application + MainActivity + FileProvider，来自 tauri-cli templates/mobile/android）：

   ```xml
   <uses-permission android:name="android.permission.BLUETOOTH_SCAN"/>
   <uses-permission android:name="android.permission.BLUETOOTH_CONNECT"/>
   <uses-permission android:name="android.permission.BLUETOOTH_ADVERTISE"/>
   <uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" android:maxSdkVersion="30"/>
   <uses-feature android:name="android.hardware.bluetooth_le" android:required="true"/>
   ```

   建议把这两个 patch 文件放 `android/gen-patches/`（MainActivity.kt / AndroidManifest.xml），
   patch 脚本 cp 覆盖——避免在脚本里内嵌大段内容。

### 4.5 CI（`android-tauri.yml`）修改

- init 步骤后依次加：
  1. `bash scripts/sync-android-ble.sh`（android/ble/*.kt → gen/android）
  2. `bash scripts/patch-android-gen.sh`（MainActivity/Manifest 覆盖）
- 顺序：init → sync → patch → build。M1 起生效。
- 构建命令维持 `--apk --debug`（release 切换是未决事项，见 §6）。

### 4.6 自测（本机 adb，手机已连接）

1. CI 绿后下载 artifact（用户从 Actions 下载 zip 到本机路径）→ `adb install -r`（或直接 `adb install -r <zip 内 apk 路径>`）。
2. 启动 App → 日志区应出现 `[桥] 已连接 Rust(127.0.0.1:18570)` 与 `[桥] BLE 已启动...`。
3. `adb shell uiautomator dump` 检查按钮/日志文本。
4. 问题排查：`adb logcat -d --pid=<pid>`、`adb shell uiautomator dump`、crash buffer
   （参考 M0 黑屏排查路径，主文档坑 17）。

## 5. M1 验收执行（需要用户配合 Windows 侧）

1. Windows 构建/使用已验证的 `zoe-cli`（feature ble-windows）：
   ```sh
   zoe-cli ble scan --timeout 10                 # 应看到手机广播(zoe 服务 UUID)
   zoe-cli ble connect <MAC> --send 5a010203040506070809000001ab   # 手机 echo 回显
   ```
2. 手机 App 日志区同时显示 `[收]`（Kotlin onLog → TCP → Rust → bridge-log → UI）。
3. echo 开关验证：App 内关 echo 后 `ble connect --send` 不再回显。
4. 验收通过后更新主文档：M1 状态 + 验收记录 + 新坑（如有）。

## 6. 未决事项

- **release APK 切换**：用户提过"为何不出 release"，已解释（debug 迭代快、签名后续配），
  用户暂未确认是否切换；M1 验证期建议维持 debug，通过后切换
  （workflow 一行：`tauri android build --apk` 去掉 `--debug`）。
- **M2 预览**：`mobile_driver.rs`（BleDriver 走 TCP 桥）+ MeshOverlay 装配 +
  zoe-cli `--send-env`/`ble mesh` 小改；见主文档 M2 章节（含 M2a/M2b 分阶段验收）。

## 7. 参考文件

- `docs/tauri-mobile.md`：总纲 + 17 坑 + M0 验收记录（必读）
- `android/app/src/main/java/com/zoechat/ble/Bridge.kt`：M1 Kotlin 桥（已就绪，勿重写；
  如需改动按主文档 M1 协议表）
- `scripts/sync-android-ble.sh` / `.ps1`：Kotlin 同步脚本（已就绪）
- `.github/workflows/android-tauri.yml`：CI（M1 需加 sync/patch 步骤）
