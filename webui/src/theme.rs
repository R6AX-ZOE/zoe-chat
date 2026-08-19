//! 深色/浅色主题:data-theme 驱动 CSS 变量;默认跟随系统,用户选择持久化。

use leptos::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
    System,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
            Theme::System => "system",
        }
    }

    pub fn detect() -> Theme {
        if let Ok(Some(storage)) = local_storage() {
            if let Ok(Some(v)) = storage.get_item(STORAGE_KEY) {
                return match v.as_str() {
                    "light" => Theme::Light,
                    "dark" => Theme::Dark,
                    _ => Theme::System,
                };
            }
        }
        Theme::System
    }
}

const STORAGE_KEY: &str = "zoe.theme";

fn local_storage() -> Result<Option<web_sys::Storage>, ()> {
    let w = web_sys::window().ok_or(())?;
    w.local_storage().map_err(|_| ())
}

fn prefers_dark() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
        .map(|m| m.matches())
        .unwrap_or(false)
}

/// 应用主题到 `<html data-theme>` + color-scheme。
pub fn apply(theme: Theme) {
    let effective = match theme {
        Theme::System => {
            if prefers_dark() {
                "dark"
            } else {
                "light"
            }
        }
        Theme::Light => "light",
        Theme::Dark => "dark",
    };
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        if let Some(el) = doc.document_element() {
            let _ = el.set_attribute("data-theme", effective);
            let _ = el.set_attribute("style", &format!("color-scheme: {effective}"));
        }
    }
}

/// 系统主题变化时跟随(system 模式)。
pub fn watch_system(theme: RwSignal<Theme>) {
    if let Some(win) = web_sys::window() {
        if let Ok(Some(mql)) = win.match_media("(prefers-color-scheme: dark)") {
            let cb = Closure::wrap(Box::new(move |_event: web_sys::Event| {
                if theme.get() == Theme::System {
                    apply(Theme::System);
                }
            }) as Box<dyn FnMut(web_sys::Event)>);
            let _ = mql.add_listener_with_opt_callback(Some(cb.as_ref().unchecked_ref()));
            cb.forget();
        }
    }
}

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
