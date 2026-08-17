# zoe-mobile：Tauri 2 集成 zoe-core 设计交接

> 状态：**设计定稿，尚未动工**。本文档供新上下文直接执行，避免重新决策。

## 目标

用 Tauri 2 把 `zoe-core`（E2E/MLS）+ `zoe-transport` 的 MeshOverlay（分片/重组/去重/TTL）
集成进手机 App，替代纯 Kotlin 方案 B（`android/` 保留作后备）。
最终真机验证：Windows `zoe-cli ble connect` ↔ 手机 App 全栈 MeshOverlay 互通。

## 已定架构（勿改，除非有充分理由）

- 新工程 **`app/`**：Tauri 2 mobile。**独立于根 cargo workspace**（`app/src-tauri/Cargo.toml`
  内放空 `[workspace]` 表隔离；自带独立 Cargo.lock，由 CI/有网络机器生成后提交）。
- **Rust 核心**（`app/src-tauri/src`）：嵌入 `zoe-core` + `zoe-transport`
  （需给 zoe-transport 加 feature **`ble-mobile = []`**，并把 lib.rs 门控放开：
  `#[cfg(any(feature = "ble-linux", feature = "ble-windows", feature = "ble-mobile"))] pub mod ble;`
  —— ble 模块的帧/MeshOverlay 是纯逻辑，android 目标可编译）。
- **BLE 传输**：Kotlin 侧**复用已验证的** `ZoeBleServer.kt` / `ZoeAdvertiser.kt` / `ZoeFrame.kt`
  （拷贝进 Tauri 生成的 `app/src-tauri/gen/android/...` 工程）。Rust ↔ Kotlin 经
  **127.0.0.1 回环 TCP** 桥接（Kotlin 起 socket 客户端连 Rust 的 tokio TcpListener）：
  - Kotlin→Rust：`{"t":"frame","a":"<mac>","d":"<hex>"}`、`{"t":"log","d":"..."}`
  - Rust→Kotlin：`{"t":"start"}`、`{"t":"stop"}`、`{"t":"send","a":"<mac>","d":"<hex>"}`、`{"t":"echo","v":bool}`
  - 理由：零 JNI，规避 Tauri 移动插件桥（`mobile_entry_point` 插件宏）的学习曲线与 CI 反复。
- **前端**：Vite + 原生 JS 控制台 UI（`@tauri-apps/api` 的 `invoke`/`listen`）。
- **构建：仅走 CI**（本地沙箱无 crates.io 网络，装不了 cargo-ndk / tauri CLI / Android targets）。
  新 workflow **`.github/workflows/android-tauri.yml`**，APK 从 Actions 下载。

## 里程碑

| 里程碑 | 内容 | 验收 |
|---|---|---|
| M0 | Tauri android 骨架；zoe-core/zoe-transport 编进 APK；`hello_frame` 冒烟命令（帧 构造→解析 回路） | CI 出 APK，装机能显示版本+帧 hex |
| M1 | 回环 TCP 桥 + Kotlin BLE 服务接入（广播+GATT+echo） | 功能等价现有 APK；Windows `ble connect --send` 回显 |
| M2 | MeshOverlay + zoe-core 全栈：大信封分片重组、去重/TTL、E2E | Windows ↔ 手机 全栈互通（消息收发） |

## M0 文件清单

```
app/package.json            # deps: @tauri-apps/api ^2; devDeps: @tauri-apps/cli ^2, vite ^6
app/vite.config.js          # port 1420, strictPort
app/index.html              # 控制台 UI 骨架
app/src/main.js             # invoke("app_info") / invoke("hello_frame") / listen 日志
app/src/style.css
app/src-tauri/Cargo.toml    # [lib] crate-type = ["staticlib","cdylib","rlib"]; tauri 2;
                            # zoe-core/zoe-transport path 依赖(features=["ble-mobile"]); hex; 空 [workspace]
app/src-tauri/build.rs      # tauri_build::build()
app/src-tauri/tauri.conf.json  # identifier com.zoechat.mobile; frontendDist ../dist;
                            # beforeDevCommand npm run dev; "android": {"minSdkVersion": 26}
app/src-tauri/capabilities/default.json  # permissions: ["core:default"]
app/src-tauri/icons/icon.png   # 1024x1024 源图(其余尺寸由 CI `npx tauri icon` 生成)
app/src-tauri/src/main.rs   # fn main() { zoe_mobile_lib::run() }
app/src-tauri/src/lib.rs    # #[cfg_attr(mobile, tauri::mobile_entry_point)] pub fn run();
                            # commands: app_info / hello_frame
.github/workflows/android-tauri.yml
```

## 关键坑（实测结论，勿重走）

1. **本地离线构建**：沙箱无 crates.io 网络；cargo 离线时即使锁完整也要求索引候选（索引缓存为空），
   bluer 的 bluetoothd 依赖（custom_debug/dbus-crossroads 等）无法离线解析。
   → 本地构建 Windows CLI 时**临时去掉** `crates/zoe-transport/Cargo.toml` 里 bluer 的
   `features = ["bluetoothd"]`，构建后恢复（勿提交）。已提交的 Cargo.lock 是完整一致的。
2. **图标**：只提交 `icon.png`（1024²），CI 里 `npx tauri icon src-tauri/icons/icon.png` 生成全套。
3. **NDK**：CI 用 `sdkmanager "ndk;27.0.12077973" --sdk_root="$ANDROID_HOME"`（配合
   android-actions/setup-android@v3）。
4. **gen/android 提交入库**：`npx tauri android init` 生成的 `src-tauri/gen/android/` 要 git 提交，
   后续 Kotlin BLE 代码（M1）写在那里。
5. **git push**：需 `-c http.proxy=http://127.0.0.1:58591` 且工作区写权限不够时提权（Git Credential Manager）。
6. CI 的 cargo 命令**均未用 --locked**，runner 自动补锁；锁文件最终在有网络机器上
   `cargo build -p zoe-transport --features ble-linux` 生成并提交。

## 现有资产（可直接复用）

- `android/app/src/main/java/com/zoechat/ble/`：ZoeFrame.kt / ZoeBleServer.kt / ZoeAdvertiser.kt
  （零依赖、已验证：CCCD 显式描述符 + onDescriptorWriteRequest 应答均已修好）
- `crates/zoe-transport/src/ble/mod.rs`：`frame_chunks` / `parse_frame` / `MeshOverlay<D: BleDriver>`
- `zoe-cli ble scan/connect`（Windows，已验证）+ `tools/ble-gatt-test/`（Chrome 备用）
