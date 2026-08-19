//! zoe-chat Web UI(Rust/Leptos CSR,编译为 wasm32-unknown-unknown)。
//!
//! 与守护进程的通信走 HTTP/WS(与桌面 daemon 同一份 API 契约,
//! 移动端经内嵌守护进程的 127.0.0.1 服务访问)。构建产物
//! (dist/index.html + styles.css + assets/zoe_webui.{js,wasm})
//! 由 zoe-daemon 与 Tauri 移动端共用嵌入。

pub mod api;
pub mod app;
pub mod i18n;
pub mod icons;
pub mod theme;

use wasm_bindgen::prelude::*;

/// wasm 入口:挂载 Leptos 根组件到 `#app`(CSS 的 `#app{height:100%}` 是布局基座;
/// 挂到 body 会让空 #app 占满视口、内容被挤到下方)。
#[wasm_bindgen(start)]
pub fn run() {
    console_error_panic_hook::set_once(); // 调试:panic 转 console.error
    let element = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("app"));
    match element {
        Some(el) => {
            let el: web_sys::HtmlElement = el.unchecked_into();
            leptos::mount::mount_to(el, app::App).forget();
        }
        None => leptos::mount::mount_to_body(app::App),
    }
}

use wasm_bindgen::JsCast;
