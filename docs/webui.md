# Rust Web UI(zoe-webui)构建与架构 v0.1

> Web UI 全部用 Rust 编写(Leptos 0.7 CSR → wasm32-unknown-unknown),**零 JS/TS 工具链**(npm/tsc/vite 已移除)。
> 产物 `webui/dist/` 由桌面守护进程与 Tauri 移动端**共用**:桌面经 zoe-daemon 编译期内嵌并服务;
> 移动端经内嵌守护进程(`zoe-daemon` lib)服务,WebView 直接加载 `http://127.0.0.1:18571/`(见 docs/tauri-mobile.md §0)。

## 1. 技术栈与文件布局

| 文件 | 职责 |
|---|---|
| `Cargo.toml` / `Cargo.lock` | 独立 crate(自带 `[workspace]` 空表隔离根 workspace;**Cargo.lock 提交入库**) |
| `src/lib.rs` | wasm 入口:`#[wasm_bindgen(start)]` + `console_error_panic_hook::set_once()` + **`mount_to(#app)`**(坑:挂 body 会被空 #app 占满视口,见 tauri-mobile.md 坑 20) |
| `src/app.rs` | 组件:启动探测(无登录页) / 会话列表 / 消息线程(分页) / 群组详情(邀请·退群) / 设置(主题·语言·配对·设备·对端·名片·导入·备份恢复·网络·传输·**用户管理**) / **锁定屏(PIN 解锁)** / 新建群组对话框 / 头部导航(`+`/齿轮常驻) |
| `src/api.rs` | HTTP/WS 客户端:DTO;`request<T>` 统一解析;**响应一律显式 `Value`**(坑 19);`connect_events` WS 自动重连(2s);**无令牌**(无 Authorization 头,无 `tauri_boot_token`) |
| `src/i18n.rs` | 键驱动词典,zh-CN/en-US 各 **145 键**,单测断言键集合一致(`key_sets_identical`) |
| `src/theme.rs` | `data-theme` + `prefers-color-scheme` 跟随 + 手动切换;`watch_system` 用 `add_listener_with_opt_callback` |
| `src/icons.rs` | 自绘 SVG 图标集(24×24,stroke 1.5,圆滑路径,**禁止 emoji**);svg 元素不支持 `inner_html`,以 `<span inner_html=完整svg字符串>` 注入 |
| `static/index.html` | 外壳:CSS 链接 + `<div id="app">` + wasm 加载器(`import init from "/assets/zoe_webui.js"; await init("/assets/zoe_webui_bg.wasm")`) |
| `static/styles.css` | CSS 变量主题(light/dark,WCAG AA);响应式断点 640/1024;`.nav-action` 头部导航(所有宽度可见) |
| `scripts/build.sh` / `build.ps1` | `cargo build --release --target wasm32-unknown-unknown` + wasm-bindgen → `dist/`;bindgen 版本**从 Cargo.lock 自锁**(`awk`/正则提取),加 `--no-typescript` |
| `dist/` | **构建产物提交入库**(index.html + styles.css + assets/zoe_webui.js + zoe_webui_bg.wasm ~1.3MB);CI 校验与源码一致 |

依赖:leptos 0.7(csr)、gloo-net 0.6(http+ws futures)、gloo-timers 0.3(futures feature)、wasm-bindgen、wasm-bindgen-futures、js-sys、web-sys(Storage/Location/MediaQueryList/Clipboard 等)、futures-util、serde/serde_json。

## 2. 构建与开发工作流

```sh
# 一次性前置
rustup target add wasm32-unknown-unknown          # rustup 分发不可达时用 rsproxy.cn 镜像(见 tauri-mobile.md 坑 1)
WBG=$(awk '/^name = "wasm-bindgen"$/{getline; getline; print $3; exit}' Cargo.lock | tr -d '"')
cargo install wasm-bindgen-cli --version "$WBG" --locked

# 构建产物 → webui/dist/
bash scripts/build.sh        # 或 Windows: .\scripts\build.ps1

# 校验(本地 + CI 均执行)
cargo test --manifest-path webui/Cargo.toml       # 原生单测(i18n 键集合一致等)
cargo clippy --manifest-path webui/Cargo.toml
git diff --exit-code -- webui/dist                # dist 新鲜度(CI 双 workflow)

# 改 UI 后必须重新生成 dist 并提交(daemon 编译期内嵌 dist,CI 校验不一致会红)
```

桌面联调:`cargo build --release -p zoe-daemon && target/release/zoe-daemon.exe --data-dir zoe-data --port 18888` → 浏览器打开 `http://127.0.0.1:18888`(**无登录步骤**;端口持久化于 `zoe-data/port`,重启/切换用户后地址不变)。

## 3. 消费方

- **桌面 zoe-daemon**:`crates/zoe-daemon/src/api.rs` 以 `include_str!/include_bytes!` 内嵌 `webui/dist` 的
  index.html / styles.css / zoe_webui.js / zoe_webui_bg.wasm(asset 路由 `/assets/{file}`,wasm 的 MIME 为 `application/wasm`)。
- **Tauri 移动端**:内嵌守护进程提供同源 HTTP/WS;UI 启动直连(无需登录/令牌);APK 内另打包
  `frontendDist: ../../webui/dist` 作兜底(实际窗口加载外部 URL 不读取)。

## 4. 平台与已知注意

- **Android WebView**:`window.prompt` 不可靠 → 建群用内联对话框;`window.confirm` 行为待真机确认
  (如不可用改内联确认);`http://127.0.0.1` 明文需 manifest `usesCleartextTraffic="true"(仅回环)`。
- **锁定屏与用户管理(2026-08-20)**:激活用户为 PIN 且守护进程未带 `--pin` 启动时,UI 进入锁定态——所有非放行请求返回 423,前罩显示 **LockView**(单输入 PIN 窗体);提交走 `POST /unlock`(api.rs `unlock()`),失败提示 `lock.error`,成功即刷新为完整应用。设置页新增 **UsersPanel**(`settings.users`):列出现有用户(PIN 标记)、创建用户(`users.create`)、为激活用户设置 PIN(`users.setPin`)、**切换用户**(`users.switch`,仅桌面 `can_switch=true`,`POST /users/:id/activate` → daemon 自重启 → UI 轮询直到新用户生效并重探锁定态)。新建用户/设置 PIN/切换均有 i18n(新增 3 键 ×2,login 5 键 ×2 已移除)。
- **i18n**:新增文案须同时加 zh-CN 与 en-US 键(键集合一致由单测 + CI 强制;键数变更须同步 `key_sets_identical` 断言的下限值)。
- **源码编码**:所有 .rs/.html/.css 必须为 UTF-8;**严禁用 PS 5.1 默认编码的 Get-Content/Set-Content 批量改**
  (GBK 往返损坏非 ASCII,见 tauri-mobile.md 坑 18)。
- **wasm-bindgen 版本**:crate 依赖与 CLI 必须同版本(build 脚本自动校验,不一致即失败)。

## 5. 已修复问题记录(2026-08-19)

1. login 泛型 `()` 解析 `{"ok":true}` 失败 → 误报"invalid token"(坑 19;login 已随令牌移除而删除,2026-08-20)。
2. `mount_to_body` 导致登录页顶部大片空白(坑 20;登录页已移除,2026-08-20)。
3. 窄窗口(<640px)无"新建群组/设置"入口 → 顶栏 `.nav-action` 常驻;设置视图 <1024px 占满主区域 + 返回按钮。
4. `window.prompt` 建群 → 内联对话框。
5. PS 5.1 编码损坏(app.rs/api.rs 乱码)→ 整体重写 UTF-8(坑 18)。
