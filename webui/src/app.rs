//! zoe-chat Web UI 主应用:登录 → 会话/设置。无框架结构,Leptos CSR 信号驱动。

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

use crate::api;
use crate::i18n::{t2, Lang};
use crate::icons::{Icon, IconView};
use crate::theme::Theme;

// ---------------------------------------------------------------------------
// 全局状态
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum MainView {
    Thread,
    Settings,
}

#[derive(Clone, Copy)]
struct Ctx {
    lang: RwSignal<Lang>,
    theme: RwSignal<Theme>,
    view: RwSignal<MainView>,
    me: RwSignal<Option<api::Me>>,
    card: RwSignal<Option<api::Card>>,
    groups: RwSignal<Vec<api::Group>>,
    peers: RwSignal<Vec<api::Peer>>,
    transports: RwSignal<Option<api::TransportStatus>>,
    net: RwSignal<Option<api::NetAddr>>,
    pairing: RwSignal<Option<api::PairStart>>,
    current_group: RwSignal<Option<String>>,
    messages: RwSignal<Vec<api::Msg>>,
    has_older: RwSignal<bool>,
    mnemonic: RwSignal<Option<String>>,
    /// 锁定模式:活跃用户为 PIN 保护且未解锁(423 门禁;仅 /users /unlock 开放)。
    locked: RwSignal<bool>,
    /// 首次启动引导:移动端默认用户未设 PIN 时提示设置(设置后重启即见锁定屏)。
    onboard: RwSignal<bool>,
    /// 用户注册表快照(锁定屏与设置页共用)。
    users: RwSignal<Option<api::UsersResp>>,
    /// 直接对讲对话框(扫码 + 新私聊)。
    direct_dialog: RwSignal<Option<DirectDialog>>,
}

/// 单聊对话框状态。
#[derive(Clone)]
struct DirectDialog {
    peer_id: String,
    name: String,
}

impl Ctx {
    fn t(&self, key: &str) -> String {
        t2(self.lang.get(), key)
    }

    /// 首次运行判定:移动端 + 未锁定 + 活跃用户为明文且注册表无任何 PIN 用户。
    fn first_run(&self, u: &Option<api::UsersResp>, unlocked: bool) -> bool {
        unlocked
            && !u.as_ref().map(|x| x.can_switch).unwrap_or(false)
            && u.as_ref()
                .map(|x| {
                    x.active.kind == "plain" && x.users.iter().all(|y| y.kind != "pin")
                })
                .unwrap_or(false)
    }

    fn current_group(&self) -> Option<api::Group> {
        let gid = self.current_group.get()?;
        self.groups.get().into_iter().find(|g| g.group_id == gid)
    }

    /// 联系人 → 单聊:已有私聊直接打开,否则弹出发起对话框。
    fn start_direct_with(&self, peer_id: String, name: String) {
        let ctx = *self;
        if let Some(gid) = ctx
            .groups
            .get()
            .into_iter()
            .find(|g| g.direct && g.direct_peer_id.as_deref() == Some(peer_id.as_str()))
            .map(|g| g.group_id)
        {
            ctx.current_group.set(Some(gid));
            ctx.view.set(MainView::Thread);
            ctx.refresh_messages();
            return;
        }
        ctx.direct_dialog.set(Some(DirectDialog { peer_id, name }));
    }

    fn refresh_groups(&self) {
        let ctx = *self;
        spawn_local(async move {
            if let Ok(gs) = api::groups().await {
                ctx.groups.set(gs);
            }
        });
    }

    /// 登录成功后的共同初始化:拉注册表判断锁定,未锁定再拉会话数据 + 挂事件流。
    fn finish_login(&self) {
        let ctx = *self;
        spawn_local(async move {
            let u = api::users().await.ok();
            ctx.users.set(u.clone());
            let unlocked = u.as_ref().map(|x| x.unlocked).unwrap_or(false);
            ctx.locked.set(!unlocked);
            // 首次运行引导:移动端(不可切换用户)+ 活跃用户未设 PIN → 提示设置。
            // 用户跳过则记 localStorage,不再打扰;设置成功后 active.kind 变 pin 自动消失。
            let first_run = ctx.first_run(&u, unlocked);
            ctx.onboard.set(first_run && !app_skip_onboard());
            if !unlocked {
                return;
            }
            ctx.me.set(api::me().await.ok());
            ctx.card.set(api::card().await.ok());
            ctx.transports.set(api::transports().await.ok());
            ctx.refresh_groups();
            ctx.refresh_peers();
            api::connect_events(Box::new(move |ev| {
                let ctx = ctx;
                match ev["type"].as_str() {
                    Some("message") => ctx.refresh_messages(),
                    Some("group") => ctx.refresh_groups(),
                    Some("transport") => ctx.refresh_transports(),
                    Some("peer") => ctx.refresh_peers(),
                    // 切换用户后 daemon 自重启:重启即重连;完成后重探注册表/解锁状态
                    Some("user") => ctx.finish_login(),
                    _ => {}
                }
            }));
        });
    }

    fn refresh_transports(&self) {
        let ctx = *self;
        spawn_local(async move {
            if let Ok(ts) = api::transports().await {
                ctx.transports.set(Some(ts));
            }
            match api::net_addr().await {
                Ok(n) => ctx.net.set(Some(n)),
                Err(_) => ctx.net.set(None),
            }
        });
    }

    fn refresh_messages(&self) {
        let ctx = *self;
        spawn_local(async move {
            let Some(gid) = ctx.current_group.get() else {
                return;
            };
            if let Ok(msgs) = api::messages(&gid, 100, None).await {
                let n = msgs.len();
                ctx.messages.set(msgs);
                ctx.has_older.set(n >= 100);
                scroll_messages_bottom();
            }
        });
    }

    fn refresh_peers(&self) {
        let ctx = *self;
        spawn_local(async move {
            if let Ok(ps) = api::peers().await {
                ctx.peers.set(ps);
            }
        });
    }
}

fn scroll_messages_bottom() {
    if let Some(el) = document().get_element_by_id("msg-list") {
        el.set_scroll_top(el.scroll_height());
    }
}

fn short(id: &str) -> String {
    if id.len() > 16 {
        format!("{}…{}", &id[..8], &id[id.len() - 4..])
    } else {
        id.to_string()
    }
}

fn fmt_time(ts: i64) -> String {
    let d = js_sys::Date::new(&(ts as f64 * 1000.0).into());
    format!("{:02}:{:02}", d.get_hours(), d.get_minutes())
}

fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn confirm(msg: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.confirm_with_message(msg).ok())
        .unwrap_or(false)
}

fn alert(msg: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.alert_with_message(msg);
    }
}

// ---------------------------------------------------------------------------
// 文件上传 / 下载(wasm 侧 FileReader / Blob + object URL)
// ---------------------------------------------------------------------------

/// 文件大小上限(与守护进程一致):8 MiB。
const MAX_FILE_SIZE: f64 = 8.0 * 1024.0 * 1024.0;

/// 读取 File 为 base64 + mime(data URL 剥离)。
async fn read_file_base64(file: web_sys::File) -> Result<(String, String), String> {
    let file2 = file.clone();
    let promise = js_sys::Promise::new(&mut move |resolve, reject| {
        let fr = web_sys::FileReader::new().expect("FileReader");
        let fr2 = fr.clone();
        let reject_for_load = reject.clone();
        let on_load =
            wasm_bindgen::closure::Closure::<dyn FnMut()>::once(move || match fr2.result() {
                Ok(v) => {
                    let _ = resolve.call1(&JsValue::NULL, &v);
                }
                Err(_) => {
                    let _ = reject_for_load.call1(&JsValue::NULL, &JsValue::from_str("no result"));
                }
            });
        let on_err = wasm_bindgen::closure::Closure::<dyn FnMut()>::once(move || {
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str("read error"));
        });
        fr.set_onloadend(Some(on_load.as_ref().unchecked_ref()));
        fr.set_onerror(Some(on_err.as_ref().unchecked_ref()));
        on_load.forget();
        on_err.forget();
        let _ = fr.read_as_data_url(&file2);
    });
    let v = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|e| format!("read error: {e:?}"))?;
    let s = v
        .as_string()
        .ok_or_else(|| "bad data url result".to_string())?;
    let (meta, payload) = s
        .split_once(',')
        .ok_or_else(|| "bad data url".to_string())?;
    let mime = meta
        .strip_prefix("data:")
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .to_string();
    let mime = if mime.is_empty() {
        "application/octet-stream".to_string()
    } else {
        mime
    };
    Ok((payload.to_string(), mime))
}

/// 触发浏览器保存文件(blob → object URL → <a download>)。
fn save_blob(bytes: &[u8], name: &str) {
    let u8 = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&u8.buffer());
    let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence(&parts) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    let el = document()
        .create_element("a")
        .ok()
        .and_then(|el| el.dyn_into::<web_sys::HtmlAnchorElement>().ok());
    if let Some(a) = el {
        a.set_href(&url);
        a.set_download(name);
        a.click();
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}

/// 下载文件消息(服务端落盘 + 标记),成功后刷新消息状态。
fn download_file_msg(ctx: Ctx, hash: String, name: String) {
    spawn_local(async move {
        match api::download_file(&hash).await {
            Ok(bytes) => {
                save_blob(&bytes, &name);
                ctx.refresh_messages();
            }
            Err(_) => alert(&ctx.t("file.downloadErr")),
        }
    });
}

// ---------------------------------------------------------------------------
// 根组件
// ---------------------------------------------------------------------------

#[component]
pub fn App() -> impl IntoView {
    let ctx = Ctx {
        lang: RwSignal::new(Lang::detect()),
        theme: RwSignal::new(Theme::detect()),
        view: RwSignal::new(MainView::Thread),
        me: RwSignal::new(None),
        card: RwSignal::new(None),
        groups: RwSignal::new(Vec::new()),
        peers: RwSignal::new(Vec::new()),
        transports: RwSignal::new(None),
        net: RwSignal::new(None),
        pairing: RwSignal::new(None),
        current_group: RwSignal::new(None),
        messages: RwSignal::new(Vec::new()),
        has_older: RwSignal::new(false),
        mnemonic: RwSignal::new(None),
        locked: RwSignal::new(false),
        onboard: RwSignal::new(false),
        users: RwSignal::new(None),
        direct_dialog: RwSignal::new(None),
    };
    provide_context(ctx);

    // 主题 + 语言应用到 <html>
    Effect::new(move |_| {
        let theme = ctx.theme.get();
        let lang = ctx.lang.get();
        crate::theme::apply(theme);
        if let Some(el) = document().document_element() {
            let _ = el.set_attribute("lang", lang.as_str());
        }
    });

    // body[data-view] 驱动响应式
    Effect::new(move |_| {
        let v = ctx.view.get();
        if let Some(body) = document().body() {
            let _ = body.dataset().set(
                "view",
                if v == MainView::Settings {
                    "settings"
                } else {
                    "thread"
                },
            );
        }
    });

    // 系统主题跟随
    crate::theme::watch_system(ctx.theme);

    // 启动流程:无访问令牌(token 已废弃),直接探测注册表判定锁定门
    let boot = move || {
        let ctx = ctx;
        spawn_local(async move {
            ctx.finish_login();
        });
    };

    boot();

    view! {
        <main>
            <Show
                when=move || ctx.locked.get()
                fallback=move || {
                    view! {
                        <Show
                            when=move || ctx.onboard.get()
                            fallback=move || view! { <AppShell ctx=ctx /> }
                        >
                            <OnboardView ctx=ctx />
                        </Show>
                    }
                }
            >
                <LockView ctx=ctx />
            </Show>
        </main>
    }
}

// ---------------------------------------------------------------------------
// 锁定屏(活跃用户 PIN 保护且未解锁;423 门禁期间唯一主界面)
// ---------------------------------------------------------------------------

#[component]
fn LockView(ctx: Ctx) -> impl IntoView {
    let pin_input = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let do_unlock = move || {
        let pin_input = pin_input;
        let error = error;
        let busy = busy;
        let ctx = ctx;
        spawn_local(async move {
            let pin = pin_input.get().trim().to_string();
            if pin.is_empty() || busy.get() {
                return;
            }
            busy.set(true);
            match api::unlock(&pin).await {
                Ok(()) => {
                    ctx.locked.set(false);
                    ctx.finish_login();
                }
                Err(e) => error.set(ctx.t("lock.error") + &format!(" ({e})")),
            }
            busy.set(false);
        });
    };

    let active = move || {
        ctx.users
            .get()
            .map(|u| (u.active.name.clone(), u.active.kind.clone()))
    };

    view! {
        <div class="login">
            <div class="login-card">
                <div class="logo"><IconView icon=Icon::Lock size=40 /></div>
                <h1>{move || ctx.t("lock.title")}</h1>
                <p class="sub">{move || ctx.t("lock.sub")}</p>
                {move || {
                    let a = active();
                    match a {
                        Some((name, kind)) => view! {
                            <div class="kv">
                                <span class="k">{move || ctx.t("users.current")}</span>
                                <span class="v">{format!("{name} · {kind}")}</span>
                            </div>
                        }.into_any(),
                        None => ().into_any(),
                    }
                }}
                <div class="field">
                    <label for="pin-input">{move || ctx.t("lock.pinLabel")}</label>
                    <input
                        id="pin-input"
                        type="password"
                        inputmode="numeric"
                        autocomplete="off"
                        spellcheck="false"
                        prop:value=move || pin_input.get()
                        on:input=move |ev| pin_input.set(event_target_value(&ev))
                        on:keydown=move |ev| {
                            if ev.key() == "Enter" { do_unlock(); }
                        }
                    />
                </div>
                <button class="primary" disabled=move || busy.get() on:click=move |_| do_unlock()>
                    <IconView icon=Icon::Shield size=18 />
                    {move || ctx.t("lock.unlock")}
                </button>
                <div class="error">{move || error.get()}</div>
                <p class="note" style="margin-top:12px">{move || ctx.t("lock.restartHint")}</p>
            </div>
        </div>
    }
}

// ---------------------------------------------------------------------------
// 首次运行引导(移动端默认用户未设 PIN 时):设置 PIN,重启后进入锁定屏。
// 用户跳过 → localStorage 标记,本次安装不再打扰。
// ---------------------------------------------------------------------------

const ONBOARD_SKIP_KEY: &str = "zoe.onboard.skip";

fn app_skip_onboard() -> bool {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(ONBOARD_SKIP_KEY).ok().flatten())
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn mark_onboard_skipped() {
    let Some(w) = web_sys::window() else { return };
    if let Ok(Some(s)) = w.local_storage() {
        let _ = s.set_item(ONBOARD_SKIP_KEY, "1");
    }
}

#[component]
fn OnboardView(ctx: Ctx) -> impl IntoView {
    let pin = RwSignal::new(String::new());
    let confirm = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let done = RwSignal::new(false);
    let busy = RwSignal::new(false);

    let do_set = move || {
        let ctx = ctx;
        let pin = pin;
        let confirm = confirm;
        let error = error;
        let done = done;
        let busy = busy;
        spawn_local(async move {
            if busy.get() {
                return;
            }
            let p = pin.get().trim().to_string();
            let c = confirm.get().trim().to_string();
            if p.len() < 4 {
                error.set(ctx.t("users.pinTooShort"));
                return;
            }
            if p != c {
                error.set(ctx.t("onboard.mismatch"));
                return;
            }
            busy.set(true);
            error.set(String::new());
            let uid = ctx
                .users
                .get()
                .map(|u| u.active.user_id.clone())
                .unwrap_or_default();
            if uid.is_empty() {
                error.set(ctx.t("common.failed"));
            } else {
                match api::set_pin(&uid, &p).await {
                    Ok(()) => {
                        pin.set(String::new());
                        confirm.set(String::new());
                        done.set(true);
                        ctx.finish_login();
                    }
                    Err(e) => error.set(ctx.t("common.failed") + &format!(" ({e})")),
                }
            }
            busy.set(false);
        });
    };

    let do_skip = move || {
        mark_onboard_skipped();
        ctx.onboard.set(false);
    };

    view! {
        <div class="login">
            <div class="login-card">
                <div class="logo"><IconView icon=Icon::Lock size=40 /></div>
                <h1>{move || ctx.t("onboard.title")}</h1>
                <p class="sub">{move || ctx.t("onboard.sub")}</p>
                {move || {
                    if done.get() {
                        view! {
                            <div class="kv">
                                <span class="k">{move || ctx.t("onboard.done")}</span>
                                <span class="v" style="color:var(--ok)">OK</span>
                            </div>
                            <p class="note" style="margin-top:8px">{move || ctx.t("onboard.doneHint")}</p>
                            <button
                                class="primary"
                                style="margin-top:14px"
                                on:click=move |_| do_restart(ctx)
                            >
                                {move || ctx.t("onboard.restart")}
                            </button>
                        }.into_any()
                    } else {
                        view! {
                            <div class="field">
                                <label for="onboard-pin">{move || ctx.t("onboard.pin")}</label>
                                <input
                                    id="onboard-pin"
                                    type="password"
                                    inputmode="numeric"
                                    autocomplete="off"
                                    spellcheck="false"
                                    prop:value=move || pin.get()
                                    on:input=move |ev| pin.set(event_target_value(&ev))
                                    on:keydown=move |ev| {
                                        if ev.key() == "Enter" { do_set(); }
                                    }
                                />
                            </div>
                            <div class="field">
                                <label for="onboard-confirm">{move || ctx.t("onboard.confirm")}</label>
                                <input
                                    id="onboard-confirm"
                                    type="password"
                                    inputmode="numeric"
                                    autocomplete="off"
                                    spellcheck="false"
                                    prop:value=move || confirm.get()
                                    on:input=move |ev| confirm.set(event_target_value(&ev))
                                    on:keydown=move |ev| {
                                        if ev.key() == "Enter" { do_set(); }
                                    }
                                />
                            </div>
                            <div class="error">{move || error.get()}</div>
                            <button class="primary" prop:disabled=move || busy.get() on:click=move |_| do_set()>
                                {move || if busy.get() { ctx.t("onboard.busy") } else { ctx.t("onboard.set") }}
                            </button>
                            <div style="margin-top:10px;display:flex;justify-content:center">
                                <button class="icon" on:click=move |_| do_skip()>{move || ctx.t("onboard.skip")}</button>
                            </div>
                        }.into_any()
                    }
                }}
            </div>
        </div>
    }
}

// ---------------------------------------------------------------------------
// 主界面
// ---------------------------------------------------------------------------

#[component]
fn AppShell(ctx: Ctx) -> impl IntoView {
    // 新建群组对话框(信号;头部/侧栏的 + 均触发;不依赖 window.prompt)
    let creating = RwSignal::new(false);
    let new_name = RwSignal::new(String::new());

    let open_create = move || {
        new_name.set(String::new());
        creating.set(true);
    };
    let close_create = move || creating.set(false);
    let do_create = move || {
        let ctx = ctx;
        let creating = creating;
        let new_name = new_name;
        spawn_local(async move {
            let name = new_name.get().trim().to_string();
            if name.is_empty() {
                return;
            }
            match api::create_group(&name).await {
                Ok(g) => {
                    ctx.current_group.set(Some(g.group_id));
                    ctx.view.set(MainView::Thread);
                    ctx.refresh_groups();
                    ctx.refresh_messages();
                }
                Err(_) => alert(&ctx.t("common.failed")),
            }
            creating.set(false);
            new_name.set(String::new());
        });
    };

    view! {
        <div class="app">
            <header class="app-header">
                <div class="brand">
                    <span style="color:var(--accent)"><IconView icon=Icon::Lock size=20 /></span>
                    <span class="name">{move || ctx.t("app.title")}</span>
                </div>
                <div class="actions">
                    <TransportDots ctx=ctx />
                    <button class="icon nav-action" title={move || ctx.t("chat.newGroup")} on:click=move |_| open_create()>
                        <IconView icon=Icon::Plus size=20 />
                    </button>
                    <button class="icon nav-action" title={move || ctx.t("nav.settings")} on:click=move |_| {
                        ctx.view.set(MainView::Settings);
                    }>
                        <IconView icon=Icon::Gear size=20 />
                    </button>
                    <button class="icon" title={move || ctx.t("settings.theme")} on:click=move |_| {
                        let order = [Theme::System, Theme::Light, Theme::Dark];
                        let cur = ctx.theme.get();
                        let next = order.iter().position(|t| *t == cur).map(|i| order[(i + 1) % 3]).unwrap_or(Theme::System);
                        ctx.theme.set(next);
                        save_theme_lang(ctx);
                    }>
                        {move || {
                            let ic = match ctx.theme.get() {
                                Theme::Dark => Icon::Moon,
                                Theme::Light => Icon::Sun,
                                Theme::System => Icon::Monitor,
                            };
                            view! { <IconView icon=ic size=20 /> }
                        }}
                    </button>
                    <button class="icon" title={move || ctx.t("settings.language")} on:click=move |_| {
                        let next = match ctx.lang.get() {
                            Lang::ZhCN => Lang::EnUS,
                            Lang::EnUS => Lang::ZhCN,
                        };
                        ctx.lang.set(next);
                        next.save();
                        save_theme_lang(ctx);
                    }>
                        <IconView icon=Icon::Globe size=20 />
                    </button>
                </div>
            </header>
            <div class="main">
                <aside class="sidebar">
                    <div class="sidebar-head">
                        <button class="icon" title={move || ctx.t("chat.newGroup")} on:click=move |_| open_create()>
                            <IconView icon=Icon::Plus size=20 />
                        </button>
                        <button class="icon" title={move || ctx.t("nav.settings")} on:click=move |_| {
                            ctx.view.set(MainView::Settings);
                        }>
                            <IconView icon=Icon::Gear size=20 />
                        </button>
                    </div>
                    <div class="sidebar-list">
                        <GroupList ctx=ctx />
                        <ContactsList ctx=ctx />
                    </div>
                </aside>
                <section class="thread"><ThreadView ctx=ctx /></section>
                <aside class="details">
                    <Show
                        when=move || ctx.view.get() == MainView::Settings
                        fallback=move || view! { <GroupDetails ctx=ctx /> }
                    >
                        <SettingsView ctx=ctx />
                    </Show>
                </aside>
            </div>
            <Show when=move || creating.get() fallback=move || ()>
                <div class="create-dialog" on:click=move |_| close_create()>
                    <div class="create-card" on:click=move |ev| ev.stop_propagation()>
                        <h3>{move || ctx.t("chat.newGroup")}</h3>
                        <input
                            type="text"
                            spellcheck="false"
                            placeholder={move || ctx.t("chat.groupName")}
                            prop:value=move || new_name.get()
                            on:input=move |ev| new_name.set(event_target_value(&ev))
                            on:keydown=move |ev| {
                                if ev.key() == "Enter" { do_create(); }
                                if ev.key() == "Escape" { close_create(); }
                            }
                        />
                        <div class="row" style="margin-top:10px">
                            <button class="primary" on:click=move |_| do_create()>{move || ctx.t("chat.create")}</button>
                            <button on:click=move |_| close_create()>{move || ctx.t("common.cancel")}</button>
                        </div>
                    </div>
                </div>
            </Show>
            <DirectDialogView ctx=ctx />
        </div>
    }
}

/// 发起单聊对话框(联系人已存在时直开;可填可选地址以拨号)。
#[component]
fn DirectDialogView(ctx: Ctx) -> impl IntoView {
    let addr = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let do_start = move || {
        let ctx = ctx;
        let addr = addr;
        let error = error;
        let busy = busy;
        spawn_local(async move {
            let Some(d) = ctx.direct_dialog.get() else {
                return;
            };
            if busy.get() {
                return;
            }
            busy.set(true);
            error.set(String::new());
            let a = addr.get();
            match api::start_direct(&d.peer_id, Some(&a)).await {
                Ok(dr) => {
                    ctx.direct_dialog.set(None);
                    addr.set(String::new());
                    ctx.current_group.set(Some(dr.group_id));
                    ctx.view.set(MainView::Thread);
                    ctx.refresh_groups();
                    ctx.refresh_messages();
                }
                Err(e) => {
                    error.set(if e.1.is_empty() {
                        ctx.t("chat.direct.err")
                    } else {
                        e.1
                    });
                }
            }
            busy.set(false);
        });
    };

    view! {
        {move || {
            let d = ctx.direct_dialog.get();
            match d {
                None => ().into_any(),
                Some(d) => view! {
                    <div
                        class="create-dialog"
                        on:click=move |_| {
                            if !busy.get() { ctx.direct_dialog.set(None); }
                        }
                    >
                        <div class="create-card" on:click=move |ev| ev.stop_propagation()>
                            <h3>{move || {
                                format!("{} · {}", ctx.t("chat.direct.new"), d.name.clone())
                            }}</h3>
                            <div class="field">
                                <label>{move || ctx.t("chat.direct.addr")}</label>
                                <input
                                    type="text"
                                    spellcheck="false"
                                    placeholder={move || ctx.t("chat.direct.hint")}
                                    prop:value=move || addr.get()
                                    on:input=move |ev| addr.set(event_target_value(&ev))
                                    on:keydown=move |ev| {
                                        if ev.key() == "Enter" { do_start(); }
                                        if ev.key() == "Escape" { ctx.direct_dialog.set(None); }
                                    }
                                />
                            </div>
                            <div class="error" style="color:var(--danger);font-size:13px;margin-bottom:8px">
                                {move || error.get()}
                            </div>
                            <div class="row">
                                <button class="primary" prop:disabled=move || busy.get() on:click=move |_| do_start()>
                                    {move || if busy.get() { ctx.t("chat.direct.creating") } else { ctx.t("chat.direct.start") }}
                                </button>
                                <button on:click=move |_| ctx.direct_dialog.set(None)>{move || ctx.t("common.cancel")}</button>
                            </div>
                        </div>
                    </div>
                }.into_any(),
            }
        }}
    }
}

fn save_theme_lang(ctx: Ctx) {
    spawn_local(async move {
        let _ = api::save_settings(ctx.theme.get().as_str(), ctx.lang.get().as_str()).await;
    });
}

/// 重启服务/应用(Android 冷启动 → 锁定屏;桌面无宿主钩子,报错提示)。
fn do_restart(ctx: Ctx) {
    spawn_local(async move {
        match api::system_restart().await {
            Ok(()) => {}
            Err(e) => alert(&(ctx.t("system.restartErr") + &format!(" ({e})"))),
        }
    });
}

/// 传输状态行:(API 键, i18n 键)。
const TRANSPORT_ITEMS: [(&str, &str); 5] = [
    ("ble", "transport.ble"),
    ("lan", "transport.lan"),
    ("net", "transport.net"),
    ("sigmesh", "transport.sigmesh"),
    ("loopback", "transport.loopback"),
];

fn transport_up(ts: &Option<api::TransportStatus>, key: &str) -> bool {
    ts.as_ref()
        .map(|t| match key {
            "ble" => t.ble == "up",
            "lan" => t.lan == "up",
            "net" => t.net == "up",
            "sigmesh" => t.sigmesh.as_deref() == Some("up"),
            _ => t.loopback == "up",
        })
        .unwrap_or(false)
}

#[component]
fn TransportDots(ctx: Ctx) -> impl IntoView {
    view! {
        <span class="transport-dots">
            {move || {
                let ts = ctx.transports.get();
                TRANSPORT_ITEMS.iter().map(move |(label, key)| {
                    let up = transport_up(&ts, key);
                    view! {
                        <span
                            class:up=move || up
                            class="dot"
                            title=move || {
                                let label = label;
                                ctx.t(label)
                            }
                        ></span>
                    }.into_any()
                }).collect::<Vec<AnyView>>()
            }}
        </span>
    }
}

#[component]
fn GroupList(ctx: Ctx) -> impl IntoView {
    let select = move |gid: String| {
        ctx.current_group.set(Some(gid));
        ctx.view.set(MainView::Thread);
        ctx.refresh_messages();
    };
    view! {
        <div>
            {move || {
                let groups = ctx.groups.get();
                if groups.is_empty() {
                    vec![view! { <div class="empty-hint">{move || ctx.t("chat.empty")}</div> }.into_any()]
                } else {
                    let mut items: Vec<AnyView> = Vec::new();
                    for g in groups {
                        let gid = g.group_id.clone();
                        let active = ctx.current_group.get().as_deref() == Some(g.group_id.as_str());
                        let name = g
                            .direct_name
                            .clone()
                            .or_else(|| g.name.clone())
                            .unwrap_or_else(|| short(&g.group_id));
                        let sub = if g.direct {
                            ctx.t("chat.direct")
                        } else {
                            format!("{} {} · e{}", g.members.len(), ctx.t("chat.members"), g.epoch)
                        };
                        let icon = if g.direct { Icon::Chat } else { Icon::Users };
                        items.push(view! {
                            <div class:active=move || active
                                class="group-item"
                                on:click=move |_| {
                                    let gid = gid.clone();
                                    select(gid);
                                }>
                                <span class="gicon"><IconView icon=icon size=20 /></span>
                                <span class="gmeta">
                                    <span class="gname">{name}</span>
                                    <span class="gsub">{sub}</span>
                                </span>
                            </div>
                        }.into_any());
                    }
                    items
                }
            }}
        </div>
    }
}

/// 侧栏联系人(可发起单聊;被阻止的对端不显示)。
#[component]
fn ContactsList(ctx: Ctx) -> impl IntoView {
    view! {
        {move || {
            let peers = ctx.peers.get();
            let visible: Vec<api::Peer> = peers
                .into_iter()
                .filter(|p| p.trust_status != 2)
                .collect();
            if visible.is_empty() {
                return ().into_any();
            }
            view! {
                <div class="sidebar-section">
                    <h4>{move || ctx.t("nav.contacts")}</h4>
                    {move || {
                        let peers = ctx.peers.get();
                        let visible: Vec<api::Peer> = peers
                            .into_iter()
                            .filter(|p| p.trust_status != 2)
                            .collect();
                        visible.into_iter().map(move |p| {
                            let pid = p.peer_id.clone();
                            let name = p.display_name.clone().unwrap_or_else(|| short(&p.fingerprint));
                            let chat_name = name.clone();
                            let sub = if p.trust_status == 1 {
                                ctx.t("peer.verified")
                            } else {
                                ctx.t("peer.tofu")
                            };
                            view! {
                                <div
                                    class="group-item contact-item"
                                    on:click=move |_| {
                                        let pid = pid.clone();
                                        let name = chat_name.clone();
                                        ctx.start_direct_with(pid, name);
                                    }
                                >
                                    <span class="gicon"><IconView icon=Icon::Key size=16 /></span>
                                    <span class="gmeta">
                                        <span class="gname">{name}</span>
                                        <span class="gsub">{sub}</span>
                                    </span>
                                    <span class="contact-chat" title={move || ctx.t("chat.direct.new")}>
                                        <IconView icon=Icon::Chat size=16 />
                                    </span>
                                </div>
                            }.into_any()
                        }).collect::<Vec<AnyView>>()
                    }}
                </div>
            }.into_any()
        }}
    }
}

// ---------------------------------------------------------------------------
// 消息线程
// ---------------------------------------------------------------------------

#[component]
fn ThreadView(ctx: Ctx) -> impl IntoView {
    let msg_text = RwSignal::new(String::new());
    let sending = RwSignal::new(false);
    let file_input: NodeRef<leptos::html::Input> = NodeRef::new();

    let do_send = move || {
        let ctx = ctx;
        let msg_text = msg_text;
        let sending = sending;
        spawn_local(async move {
            let Some(gid) = ctx.current_group.get() else {
                return;
            };
            let text = msg_text.get().trim().to_string();
            if text.is_empty() || sending.get() {
                return;
            }
            sending.set(true);
            msg_text.set(String::new());
            if api::send_message(&gid, &text).await.is_err() {
                msg_text.set(text);
            }
            sending.set(false);
            ctx.refresh_messages();
        });
    };

    // 附件:选择文件 → base64 → 发送文件消息
    let on_pick_file = move |ev: web_sys::Event| {
        let ctx = ctx;
        let sending = sending;
        let Some(input) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        let Some(files) = input.files() else { return };
        let Some(file) = files.item(0) else { return };
        input.set_value("");
        if file.size() > MAX_FILE_SIZE {
            alert(&ctx.t("file.tooLarge"));
            return;
        }
        let name = file.name();
        let gid = ctx.current_group.get();
        spawn_local(async move {
            let Some(gid) = gid else { return };
            match read_file_base64(file).await {
                Ok((b64, mime)) => {
                    sending.set(true);
                    if api::send_file(&gid, &name, &mime, &b64).await.is_err() {
                        alert(&ctx.t("file.failed"));
                    }
                    sending.set(false);
                    ctx.refresh_messages();
                }
                Err(e) => alert(&e),
            }
        });
    };

    let load_older = move || {
        let ctx = ctx;
        spawn_local(async move {
            let Some(gid) = ctx.current_group.get() else {
                return;
            };
            let first = ctx.messages.get().first().map(|m| m.id);
            let Some(before) = first else { return };
            if let Ok(older) = api::messages(&gid, 100, Some(before)).await {
                let mut all = older.clone();
                all.extend(ctx.messages.get());
                ctx.messages.set(all);
                ctx.has_older.set(older.len() >= 100);
            }
        });
    };

    view! {
        <Show
            when=move || ctx.current_group.get().is_some()
            fallback=move || view! { <div class="empty-hint">{move || ctx.t("chat.empty")}</div> }
        >
            {move || {
                let g = ctx.current_group();
                match g {
                    None => ().into_any(),
                    Some(g) => {
                        let gname = g.direct_name
                            .clone()
                            .or_else(|| g.name.clone())
                            .unwrap_or_else(|| short(&g.group_id));
                        let sub = if g.direct {
                            ctx.t("chat.direct")
                        } else {
                            format!("{} {} · e{}", g.members.len(), ctx.t("chat.members"), g.epoch)
                        };
                        view! {
                            <div class="thread-head">
                                <button class="icon back-btn" on:click=move |_| ctx.current_group.set(None)>
                                    <IconView icon=Icon::Back size=20 />
                                </button>
                                <div class="title">{gname}</div>
                                <div class="sub">{sub}</div>
                            </div>
                            <div class="thread-messages" id="msg-list">
                                {move || {
                                    let has_older = ctx.has_older.get();
                                    let older_btn = view! {
                                        <button
                                            class="load-more"
                                            on:click=move |_| load_older()
                                        >
                                            {move || if ctx.has_older.get() { ctx.t("chat.loadMore") } else { ctx.t("chat.noMore") }}
                                        </button>
                                    };
                                    let msgs = ctx.messages.get();
                                    let mut out: Vec<leptos::prelude::AnyView> = Vec::new();
                                    if has_older {
                                        out.push(older_btn.into_any());
                                    }
                                    if msgs.is_empty() {
                                        out.push(view! { <div class="empty-hint">{move || ctx.t("chat.empty")}</div> }.into_any());
                                    }
                                    for m in msgs {
                                        let out_m = m.direction == 1;
                                        let status = match m.status {
                                            0 => Some(ctx.t("chat.pending")),
                                            1 => Some(ctx.t("chat.delivered")),
                                            3 => Some(ctx.t("chat.failed")),
                                            _ => None,
                                        };
                                        let status_html = if out_m {
                                            status.map(|s| format!(" · <span class=\"status\">{s}</span>")).unwrap_or_default()
                                        } else { String::new() };
                                        let time = fmt_time(m.received_at);
                                        let bubble: leptos::prelude::AnyView = match m.file.clone() {
                                            Some(f) => {
                                                let fname = f.name.clone();
                                                let fname_title = fname.clone();
                                                let downloaded = m.file_downloaded;
                                                let hash = m.msg_hash.clone();
                                                let state_key = if downloaded {
                                                    "file.autoDownloaded"
                                                } else {
                                                    "file.download"
                                                };
                                                view! {
                                                    <div class="file-card" class:done=move || downloaded>
                                                        <span class="fc-icon"><IconView icon=Icon::Paperclip size=18 /></span>
                                                        <span class="fc-meta">
                                                            <span class="fc-name" title=fname_title>{fname}</span>
                                                            <span class="fc-sub">{move || format!("{} · {}", fmt_size(f.size), ctx.t(state_key))}</span>
                                                        </span>
                                                        <button
                                                            class="icon fc-btn"
                                                            title={move || ctx.t("file.download")}
                                                            on:click=move |_| {
                                                                let hash = hash.clone();
                                                                let name = f.name.clone();
                                                                download_file_msg(ctx, hash, name);
                                                            }
                                                        >
                                                            <IconView icon=Icon::Download size=16 />
                                                        </button>
                                                    </div>
                                                }.into_any()
                                            }
                                            None => {
                                                let text = esc(m.text.as_deref().unwrap_or(""));
                                                view! { <div class="bubble" inner_html=text></div> }.into_any()
                                            }
                                        };
                                        out.push(view! {
                                            <div class:out=move || out_m class="msg">
                                                {bubble}
                                                <div class="meta" inner_html=format!("{time}{status_html}")></div>
                                            </div>
                                        }.into_any());
                                    }
                                    out
                                }}
                            </div>
                            <div class="thread-input">
                                <input
                                    type="file"
                                    class="hidden"
                                    node_ref=file_input
                                    on:change=on_pick_file
                                />
                                <button class="icon" title={move || ctx.t("file.attach")} on:click=move |_| {
                                    if let Some(i) = file_input.get() {
                                        let _ = i.click();
                                    }
                                }>
                                    <IconView icon=Icon::Paperclip size=20 />
                                </button>
                                <textarea
                                    rows="1"
                                    placeholder={move || ctx.t("chat.placeholder")}
                                    prop:value=move || msg_text.get()
                                    on:input=move |ev| msg_text.set(event_target_value(&ev))
                                    on:keydown=move |ev| {
                                        if ev.key() == "Enter" && !ev.shift_key() {
                                            ev.prevent_default();
                                            do_send();
                                        }
                                    }
                                ></textarea>
                                <button class="icon primary" title={move || ctx.t("chat.send")} on:click=move |_| do_send()>
                                    <IconView icon=Icon::Send size=20 />
                                </button>
                            </div>
                        }.into_any()
                    }
                }
            }}
        </Show>
    }
}

// ---------------------------------------------------------------------------
// 群组详情
// ---------------------------------------------------------------------------

#[component]
fn GroupDetails(ctx: Ctx) -> impl IntoView {
    let invite_addr = RwSignal::new(String::new());
    let invite_msg = RwSignal::new(String::new());
    let invite_ok = RwSignal::new(false);

    let do_invite = move || {
        let ctx = ctx;
        spawn_local(async move {
            let Some(gid) = ctx.current_group.get() else {
                return;
            };
            let addr = invite_addr.get().trim().to_string();
            if addr.is_empty() {
                return;
            }
            invite_msg.set(ctx.t("chat.invite.wait"));
            match api::invite(&gid, &addr).await {
                Ok(()) => {
                    invite_msg.set(ctx.t("chat.invite.ok"));
                    invite_ok.set(true);
                    ctx.refresh_groups();
                }
                Err(_) => {
                    invite_msg.set(ctx.t("chat.invite.err"));
                    invite_ok.set(false);
                }
            }
        });
    };

    let do_leave = move || {
        let ctx = ctx;
        spawn_local(async move {
            let Some(gid) = ctx.current_group.get() else {
                return;
            };
            if !confirm(&ctx.t("chat.leave.confirm")) {
                return;
            }
            match api::leave_group(&gid).await {
                Ok(()) => {
                    ctx.current_group.set(None);
                    ctx.refresh_groups();
                }
                Err(_) => alert(&ctx.t("chat.leave.err")),
            }
        });
    };

    view! {
        {move || {
            match ctx.current_group() {
                None => ().into_any(),
                Some(g) => {
                    let members = g.members.clone();
                    let epoch = g.epoch;
                    let is_direct = g.direct;
                    let peer_id = g.direct_peer_id.clone();
                    let qr_svg = ctx.card.get().map(|c| c.qr_svg).unwrap_or_default();
                    let fp = ctx.card.get().map(|c| c.fingerprint).unwrap_or_default();
                    view! {
                        <div class="panel-section">
                            <h3>{move || if is_direct { ctx.t("chat.direct") } else { ctx.t("chat.members") }}</h3>
                            {if is_direct {
                                view! {
                                    <div class="kv">
                                        <span class="k">{move || ctx.t("settings.peerId")}</span>
                                        <span class="v">{move || peer_id.clone().map(|p| short(&p)).unwrap_or_default()}</span>
                                    </div>
                                }.into_any()
                            } else {
                                view! {
                                    {move || members.iter().map(|m| {
                                        let label = format!("{} · leaf {m}", ctx.t("chat.you"));
                                        view! { <div class="kv"><span class="k">{label}</span></div> }.into_any()
                                    }).collect::<Vec<AnyView>>()}
                                    <div class="kv"><span class="k">{move || ctx.t("chat.epoch")}</span><span class="v">{epoch}</span></div>
                                }.into_any()
                            }}
                        </div>
                        {if !is_direct {
                            view! {
                                <div class="panel-section">
                                    <h3>{move || ctx.t("chat.invite")}</h3>
                                    <p class="note" style="margin-bottom:8px">{move || ctx.t("settings.network.desc")}</p>
                                    <input
                                        type="text"
                                        spellcheck="false"
                                        placeholder={move || ctx.t("chat.invite.placeholder")}
                                        prop:value=move || invite_addr.get()
                                        on:input=move |ev| invite_addr.set(event_target_value(&ev))
                                    />
                                    <div class="row" style="margin-top:8px">
                                        <button class="primary" on:click=move |_| do_invite()>{move || ctx.t("chat.invite")}</button>
                                        <span class="note" style:color=move || if invite_ok.get() { "var(--ok)" } else { "var(--danger)" }>{move || invite_msg.get()}</span>
                                    </div>
                                </div>
                            }.into_any()
                        } else { ().into_any() }}
                        <div class="panel-section">
                            <h3>{move || ctx.t("settings.identity")}</h3>
                            <div class="qr-box" inner_html=qr_svg></div>
                            <div class="kv"><span class="k">{move || ctx.t("settings.fingerprint")}</span><span class="v">{short(&fp)}</span></div>
                        </div>
                        <div class="panel-section">
                            <button class="danger-btn" on:click=move |_| do_leave()>
                                <IconView icon=Icon::Logout size=18 />
                                {move || ctx.t("chat.leave")}
                            </button>
                        </div>
                    }.into_any()
                }
            }
        }}
    }
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

#[component]
fn SettingsView(ctx: Ctx) -> impl IntoView {
    let import_text = RwSignal::new(String::new());
    let import_msg = RwSignal::new(String::new());
    let import_ok = RwSignal::new(false);
    let dial_addr = RwSignal::new(String::new());
    let dial_msg = RwSignal::new(String::new());
    let dial_ok = RwSignal::new(false);
    let mnemonic_visible = RwSignal::new(false);
    let restore_text = RwSignal::new(String::new());
    let restore_msg = RwSignal::new(String::new());
    let restore_ok = RwSignal::new(false);

    let do_import = move || {
        let ctx = ctx;
        spawn_local(async move {
            let text = import_text.get().trim().to_string();
            if text.is_empty() {
                return;
            }
            match api::import_card(&text).await {
                Ok(()) => {
                    import_msg.set(ctx.t("settings.import.ok"));
                    import_ok.set(true);
                    ctx.refresh_peers();
                }
                Err(_) => {
                    import_msg.set(ctx.t("settings.import.err"));
                    import_ok.set(false);
                }
            }
        });
    };

    let do_dial = move || {
        let ctx = ctx;
        spawn_local(async move {
            let addr = dial_addr.get().trim().to_string();
            if addr.is_empty() {
                return;
            }
            match api::net_dial(&addr).await {
                Ok(()) => {
                    dial_msg.set(ctx.t("settings.dial.ok"));
                    dial_ok.set(true);
                }
                Err(_) => {
                    dial_msg.set(ctx.t("settings.dial.err"));
                    dial_ok.set(false);
                }
            }
        });
    };

    let toggle_pairing = move || {
        let ctx = ctx;
        spawn_local(async move {
            if ctx.pairing.get().is_some() {
                let _ = api::pair_stop().await;
                ctx.pairing.set(None);
            } else {
                ctx.pairing.set(api::pair_start().await.ok());
            }
        });
    };

    let toggle_mnemonic = move || {
        let ctx = ctx;
        let visible = mnemonic_visible;
        spawn_local(async move {
            if visible.get() {
                visible.set(false);
                return;
            }
            if ctx.mnemonic.get().is_none() {
                ctx.mnemonic.set(api::backup_mnemonic().await.ok());
            }
            visible.set(true);
        });
    };

    let do_restore = move || {
        let ctx = ctx;
        spawn_local(async move {
            let text = restore_text.get().trim().to_string();
            if text.is_empty() {
                return;
            }
            if !confirm(&ctx.t("settings.restore.confirm")) {
                return;
            }
            match api::restore(&text).await {
                Ok(()) => {
                    restore_msg.set(ctx.t("settings.restore.ok"));
                    restore_ok.set(true);
                }
                Err(_) => {
                    restore_msg.set(ctx.t("settings.restore.err"));
                    restore_ok.set(false);
                }
            }
        });
    };

    let card = ctx.card.get();
    let net = ctx.net.get();

    view! {
        <div class="settings-head">
            <button class="icon settings-back" title={move || ctx.t("common.back")} on:click=move |_| {
                ctx.view.set(MainView::Thread);
            }>
                <IconView icon=Icon::Back size=20 />
            </button>
            <span class="settings-title">{move || ctx.t("settings.title")}</span>
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.theme")}</h3>
            <div class="row">
                {move || {
                    let _cur = ctx.theme.get();
                    [Theme::Light, Theme::Dark, Theme::System].iter().map(move |th| {
                        let th = *th;
                        view! {
                            <button class:primary=move || ctx.theme.get() == th
                                on:click=move |_| {
                                    ctx.theme.set(th);
                                    save_theme_lang(ctx);
                                }>
                                {move || ctx.t(match th { Theme::Light => "settings.theme.light", Theme::Dark => "settings.theme.dark", Theme::System => "settings.theme.system" })}
                            </button>
                        }.into_any()
                    }).collect::<Vec<AnyView>>()
                }}
            </div>
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.language")}</h3>
            <div class="row">
                {move || {
                    let _cur = ctx.lang.get();
                    [Lang::ZhCN, Lang::EnUS].iter().map(move |l| {
                        let l = *l;
                        view! {
                            <button class:primary=move || ctx.lang.get() == l
                                on:click=move |_| {
                                    ctx.lang.set(l);
                                    l.save();
                                    save_theme_lang(ctx);
                                }>
                                {l.as_str()}
                            </button>
                        }.into_any()
                    }).collect::<Vec<AnyView>>()
                }}
            </div>
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.users")}</h3>
            <UsersPanel ctx=ctx />
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.pairing")}</h3>
            <p class="note" style="margin-bottom:8px">{move || ctx.t("pair.desc")}</p>
            {move || {
                let pairing = ctx.pairing.get();
                match pairing {
                    None => view! {
                        <div class="kv">
                            <span class="k">{move || ctx.t("pair.active")}</span>
                            <span class="v" style="color:var(--danger)">{move || ctx.t("pair.inactive")}</span>
                        </div>
                    }.into_any(),
                    Some(p) => view! {
                        <div class="kv">
                            <span class="k">{move || ctx.t("pair.active")}</span>
                            <span class="v" style="color:var(--ok)">{move || ctx.t("pair.active")}</span>
                        </div>
                        <div class="kv">
                            <span class="k">{move || ctx.t("pair.code")}</span>
                            <span class="v mono">{p.pair_code.clone()}</span>
                        </div>
                        {if !p.bt_advertising {
                            view! { <p class="note">{move || ctx.t("pair.btOff")}</p> }.into_any()
                        } else { ().into_any() }}
                    }.into_any(),
                }
            }}
            <div class="row" style="margin-top:8px">
                <button class="primary" on:click=move |_| toggle_pairing()>
                    <IconView icon=Icon::Radio size=18 />
                    {move || if ctx.pairing.get().is_some() { ctx.t("pair.stop") } else { ctx.t("pair.start") }}
                </button>
            </div>
            <p class="note" style="margin-top:8px">{move || ctx.t("pair.verify.desc")}</p>
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.devices")}</h3>
            <DevicesList ctx=ctx />
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.peers")}</h3>
            <PeersList ctx=ctx />
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.card")}</h3>
            <p class="note" style="margin-bottom:8px">{move || ctx.t("settings.card.desc")}</p>
            <div class="qr-box" inner_html={card.clone().map(|c| c.qr_svg).unwrap_or_default()}></div>
            {card.as_ref().map(|c| view! {
                <div class="kv"><span class="k">{move || ctx.t("settings.fingerprint")}</span><span class="v">{c.fingerprint.clone()}</span></div>
                <div class="kv"><span class="k">{move || ctx.t("settings.peerId")}</span><span class="v">{short(&c.peer_id)}</span></div>
            })}
            <div class="row" style="margin-top:8px">
                <button on:click=move |_| {
                    if let Some(c) = ctx.card.get() {
                        let text = format!("zoe://peer/{}/{}", c.peer_id, c.fingerprint);
                        if let Some(w) = web_sys::window() {
                            let cb = w.navigator().clipboard();
                            let _ = cb.write_text(&text);
                        }
                    }
                }>
                    <IconView icon=Icon::Copy size=18 />
                    {move || ctx.t("settings.copy")}
                </button>
            </div>
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.import")}</h3>
            <p class="note" style="margin-bottom:8px">{move || ctx.t("settings.import.desc")}</p>
            <textarea
                rows="2"
                spellcheck="false"
                placeholder={move || ctx.t("settings.import.placeholder")}
                prop:value=move || import_text.get()
                on:input=move |ev| import_text.set(event_target_value(&ev))
            ></textarea>
            <div class="row" style="margin-top:8px">
                <button class="primary" on:click=move |_| do_import()>{move || ctx.t("settings.import.submit")}</button>
                <span class="note" style:color=move || if import_ok.get() { "var(--ok)" } else { "var(--danger)" }>{move || import_msg.get()}</span>
            </div>
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.backup")}</h3>
            <p class="note" style="margin-bottom:8px">{move || ctx.t("settings.backup.desc")}</p>
            <div class="row">
                <button on:click=move |_| toggle_mnemonic()>
                    <IconView icon=Icon::Download size=18 />
                    {move || if mnemonic_visible.get() { ctx.t("settings.backup.hide") } else { ctx.t("settings.backup.show") }}
                </button>
            </div>
            <Show when=move || mnemonic_visible.get() fallback=move || ()>
                {move || view! {
                    <div class="mnemonic">{ctx.mnemonic.get().unwrap_or_default()}</div>
                    <p class="warn">{move || ctx.t("settings.backup.warning")}</p>
                }}
            </Show>
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.restore")}</h3>
            <p class="note" style="margin-bottom:8px">{move || ctx.t("settings.restore.desc")}</p>
            <textarea
                rows="3"
                spellcheck="false"
                placeholder={move || ctx.t("settings.restore.placeholder")}
                prop:value=move || restore_text.get()
                on:input=move |ev| restore_text.set(event_target_value(&ev))
            ></textarea>
            <div class="row" style="margin-top:8px">
                <button class="danger-btn" on:click=move |_| do_restore()>{move || ctx.t("settings.restore.submit")}</button>
                <span class="note" style:color=move || if restore_ok.get() { "var(--ok)" } else { "var(--danger)" }>{move || restore_msg.get()}</span>
            </div>
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.network")}</h3>
            <p class="note" style="margin-bottom:8px">{move || ctx.t("settings.network.desc")}</p>
            {net.as_ref().map(|n| view! {
                <div class="kv"><span class="k">{move || ctx.t("settings.peerId")}</span><span class="v">{short(&n.peer_id)}</span></div>
            })}
            {net.as_ref().and_then(|n| n.listen_addrs.first()).map(|a| view! {
                <div class="kv"><span class="k">{move || ctx.t("settings.listenAddr")}</span><span class="v">{short(a)}</span></div>
            })}
            <div class="kv">
                <span class="k">{move || ctx.t("settings.netPeers")}</span>
                <span class="v">{move || ctx.transports.get().map(|t| t.net_peers.to_string()).unwrap_or_else(|| "0".into())}</span>
            </div>
            <input
                type="text"
                spellcheck="false"
                style="margin-top:8px"
                placeholder={move || ctx.t("settings.dial.placeholder")}
                prop:value=move || dial_addr.get()
                on:input=move |ev| dial_addr.set(event_target_value(&ev))
            />
            <div class="row" style="margin-top:8px">
                <button on:click=move |_| do_dial()>{move || ctx.t("settings.dial")}</button>
                <span class="note" style:color=move || if dial_ok.get() { "var(--ok)" } else { "var(--danger)" }>{move || dial_msg.get()}</span>
            </div>
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("settings.transports")}</h3>
            {move || {
                let ts = ctx.transports.get();
                TRANSPORT_ITEMS.iter().map(move |(key, label)| {
                    let up = transport_up(&ts, key);
                    let label = *label;
                    view! {
                        <div class="kv">
                            <span class="k">{move || ctx.t(label)}</span>
                            <span class="v" style:color=move || if up { "var(--ok)" } else { "var(--danger)" }>
                                {move || if up { ctx.t("transport.up") } else { ctx.t("transport.down") }}
                            </span>
                        </div>
                    }.into_any()
                }).collect::<Vec<AnyView>>()
            }}
        </div>
        <div class="panel-section">
            <h3>{move || ctx.t("system.restartTitle")}</h3>
            <p class="note" style="margin-bottom:8px">{move || ctx.t("system.restartHint")}</p>
            <div class="row">
                <button on:click=move |_| do_restart(ctx)>
                    <IconView icon=Icon::Restart size=18 />
                    {move || ctx.t("system.restart")}
                </button>
            </div>
        </div>
    }
}

#[component]
fn UsersPanel(ctx: Ctx) -> impl IntoView {
    let new_name = RwSignal::new(String::new());
    let new_pin = RwSignal::new(String::new());
    let new_msg = RwSignal::new(String::new());
    let new_ok = RwSignal::new(false);
    let chg_pin = RwSignal::new(String::new());
    let chg_msg = RwSignal::new(String::new());
    let chg_ok = RwSignal::new(false);
    let switch_msg = RwSignal::new(String::new());

    let refresh = move || {
        let ctx = ctx;
        spawn_local(async move {
            if let Ok(u) = api::users().await {
                ctx.users.set(Some(u));
            }
        });
    };

    // 切换用户:daemon 自重启,期间持续探测直至新活跃用户生效
    let do_activate = move |user_id: String| {
        let ctx = ctx;
        let switch_msg = switch_msg;
        spawn_local(async move {
            switch_msg.set(ctx.t("users.switching"));
            match api::activate_user(&user_id).await {
                Ok(_) => {
                    for _ in 0..20 {
                        api::sleep_ms(1000).await;
                        if let Ok(u) = api::users().await {
                            if u.active.user_id == user_id {
                                ctx.users.set(Some(u.clone()));
                                ctx.locked.set(!u.unlocked);
                                if u.unlocked {
                                    ctx.finish_login();
                                }
                                switch_msg.set(String::new());
                                return;
                            }
                        }
                    }
                    switch_msg.set(ctx.t("users.switchTimeout"));
                }
                Err(e) => switch_msg.set(ctx.t("common.failed") + &format!(" ({e})")),
            }
        });
    };

    let do_create = move || {
        let ctx = ctx;
        let new_name = new_name;
        let new_pin = new_pin;
        let new_msg = new_msg;
        let new_ok = new_ok;
        let refresh = refresh;
        spawn_local(async move {
            let name = new_name.get().trim().to_string();
            let pin = new_pin.get().trim().to_string();
            if name.is_empty() || pin.len() < 4 {
                new_msg.set(ctx.t("users.pinTooShort"));
                new_ok.set(false);
                return;
            }
            match api::create_user(&name, &pin).await {
                Ok(_) => {
                    new_name.set(String::new());
                    new_pin.set(String::new());
                    new_msg.set(ctx.t("users.createOk"));
                    new_ok.set(true);
                    refresh();
                }
                Err(e) => {
                    new_msg.set(ctx.t("common.failed") + &format!(" ({e})"));
                    new_ok.set(false);
                }
            }
        });
    };

    let do_set_pin = move || {
        let ctx = ctx;
        let chg_pin = chg_pin;
        let chg_msg = chg_msg;
        let chg_ok = chg_ok;
        spawn_local(async move {
            let pin = chg_pin.get().trim().to_string();
            if pin.len() < 4 {
                chg_msg.set(ctx.t("users.pinTooShort"));
                chg_ok.set(false);
                return;
            }
            let uid = ctx
                .users
                .get()
                .map(|u| u.active.user_id.clone())
                .unwrap_or_default();
            if uid.is_empty() {
                chg_msg.set(ctx.t("common.failed"));
                chg_ok.set(false);
                return;
            }
            match api::set_pin(&uid, &pin).await {
                Ok(()) => {
                    chg_pin.set(String::new());
                    chg_msg.set(ctx.t("users.setPinOk"));
                    chg_ok.set(true);
                    refresh();
                }
                Err(e) => {
                    chg_msg.set(ctx.t("common.failed") + &format!(" ({e})"));
                    chg_ok.set(false);
                }
            }
        });
    };

    view! {
        <div>
            {move || {
                let Some(u) = ctx.users.get() else {
                    return vec![view! { <div class="note">{move || ctx.t("devices.none")}</div> }.into_any()];
                };
                let active_id = u.active.user_id.clone();
                let can_switch = u.can_switch;
                u.users.into_iter().map(move |x| {
                    let xid = x.user_id.clone();
                    let is_active = xid == active_id;
                    let kind_label = ctx.t(if x.kind == "pin" { "users.pin" } else { "users.plain" });
                    let shorted = short(&xid);
                    let do_activate = do_activate;
                    view! {
                        <div class="list-row" class:muted=move || !is_active>
                            <span class="lr-main">
                                <span class="lr-name">
                                    <IconView icon=Icon::Key size=14 />
                                    {x.name.clone()}
                                    {if is_active { format!(" · {}", ctx.t("users.current")) } else { String::new() }}
                                </span>
                                <span class="lr-sub">{format!("{kind_label} · {shorted}")}</span>
                            </span>
                            <span class="lr-actions">
                                {if can_switch && !is_active {
                                    view! {
                                        <button
                                            class="icon"
                                            title={move || ctx.t("users.switch")}
                                            on:click=move |_| {
                                                let xid = xid.clone();
                                                do_activate(xid);
                                            }
                                        >
                                            <IconView icon=Icon::Restart size=16 />
                                        </button>
                                    }.into_any()
                                } else {
                                    ().into_any()
                                }}
                            </span>
                        </div>
                    }.into_any()
                }).collect::<Vec<AnyView>>()
            }}
            <div class="note" style:color="var(--info)">{move || switch_msg.get()}</div>
            <p class="note" style="margin-top:8px">{move || ctx.t("lock.restartHint")}</p>
            <div class="row" style="margin-top:8px">
                <input
                    type="text"
                    spellcheck="false"
                    style="flex:1"
                    placeholder={move || ctx.t("users.newName")}
                    prop:value=move || new_name.get()
                    on:input=move |ev| new_name.set(event_target_value(&ev))
                />
                <input
                    type="password"
                    inputmode="numeric"
                    style="flex:1"
                    placeholder={move || ctx.t("users.newPin")}
                    prop:value=move || new_pin.get()
                    on:input=move |ev| new_pin.set(event_target_value(&ev))
                />
                <button on:click=move |_| do_create()>{move || ctx.t("users.create")}</button>
            </div>
            <div class="note" style:color=move || if new_ok.get() { "var(--ok)" } else { "var(--danger)" }>{move || new_msg.get()}</div>
            <div class="row" style="margin-top:8px">
                <input
                    type="password"
                    inputmode="numeric"
                    style="flex:1"
                    placeholder={move || ctx.t("users.setPin")}
                    prop:value=move || chg_pin.get()
                    on:input=move |ev| chg_pin.set(event_target_value(&ev))
                />
                <button on:click=move |_| do_set_pin()>{move || ctx.t("users.changePin")}</button>
            </div>
            <div class="note" style:color=move || if chg_ok.get() { "var(--ok)" } else { "var(--danger)" }>{move || chg_msg.get()}</div>
        </div>
    }
}

#[component]
fn DevicesList(ctx: Ctx) -> impl IntoView {
    let devices = RwSignal::new(Vec::<api::Device>::new());
    let self_id = ctx.me.get().map(|m| m.device.device_id).unwrap_or_default();
    spawn_local({
        async move {
            if let Ok(ds) = api::devices().await {
                devices.set(ds);
            }
        }
    });
    let revoke = move |device_id: String| {
        let ctx = ctx;
        let devices = devices;
        spawn_local(async move {
            if !confirm(&ctx.t("device.revoke.confirm")) {
                return;
            }
            if api::revoke_device(&device_id).await.is_err() {
                alert(&ctx.t("common.failed"));
                return;
            }
            if let Ok(ds) = api::devices().await {
                devices.set(ds);
            }
        });
    };
    view! {
        <div>
            {move || {
                let ds = devices.get();
                if ds.is_empty() {
                    vec![view! { <div class="note">{move || ctx.t("devices.none")}</div> }.into_any()]
                } else {
                    let self_id = self_id.clone();
                    ds.into_iter().map(move |d| {
                        let did = d.device_id.clone();
                        let is_self = d.device_id == self_id;
                        let muted = d.revoked;
                        view! {
                            <div class="list-row" class:muted=move || muted>
                                <span class="lr-main">
                                    <span class="lr-name">
                                        <IconView icon=Icon::Key size=14 />
                                        {d.name.clone()}
                                        {if is_self { format!(" · {}", ctx.t("device.this")) } else { String::new() }}
                                    </span>
                                    <span class="lr-sub">{short(&d.device_id)}</span>
                                </span>
                                <span class="lr-actions">
                                    {if d.revoked {
                                        view! { <span class="tag danger">{move || ctx.t("device.revoked")}</span> }.into_any()
                                    } else if !is_self {
                                        view! {
                                            <button class="icon" title={move || ctx.t("device.revoke")} on:click=move |_| {
                                                let did = did.clone();
                                                revoke(did);
                                            }>
                                                <IconView icon=Icon::Ban size=16 />
                                            </button>
                                        }.into_any()
                                    } else { ().into_any() }}
                                </span>
                            </div>
                        }.into_any()
                    }).collect::<Vec<AnyView>>()
                }
            }}
        </div>
    }
}

#[component]
fn PeersList(ctx: Ctx) -> impl IntoView {
    let peers = RwSignal::new(Vec::<api::Peer>::new());
    spawn_local({
        async move {
            if let Ok(ps) = api::peers().await {
                peers.set(ps);
            }
        }
    });
    let verify = move |peer_id: String| {
        let peers = peers;
        spawn_local(async move {
            if api::pair_verify(&peer_id, true).await.is_err() {
                return;
            }
            if let Ok(ps) = api::peers().await {
                peers.set(ps);
            }
        });
    };
    let block = move |peer_id: String| {
        let peers = peers;
        spawn_local(async move {
            if api::block_peer(&peer_id).await.is_err() {
                return;
            }
            if let Ok(ps) = api::peers().await {
                peers.set(ps);
            }
        });
    };
    view! {
        <div>
            {move || {
                let ps = peers.get();
                if ps.is_empty() {
                    vec![view! { <div class="note">{move || ctx.t("peer.none")}</div> }.into_any()]
                } else {
                    ps.into_iter().map(move |p| {
                        let pid = p.peer_id.clone();
                        let blocked = p.trust_status == 2;
                        let verified = p.trust_status == 1;
                        let name = p.display_name.clone().unwrap_or_else(|| short(&p.fingerprint));
                        let chat_name = name.clone();
                        let fp = short(&p.fingerprint);
                        let tag = if blocked {
                            ctx.t("peer.blocked")
                        } else if verified {
                            ctx.t("peer.verified")
                        } else {
                            ctx.t("peer.tofu")
                        };
                        view! {
                            <div class="list-row" class:muted=move || blocked>
                                <span class="lr-main">
                                    <span class="lr-name">{name}</span>
                                    <span class="lr-sub">{fp}</span>
                                </span>
                                <span class="lr-actions">
                                    <span class="tag" class:ok=move || verified class:danger=move || blocked>{tag}</span>
                                    {if !blocked {
                                        let verify_pid = pid.clone();
                                        let block_pid = pid.clone();
                                        let chat_pid = pid.clone();
                                        view! {
                                            <button class="icon" title={move || ctx.t("chat.direct.new")} on:click=move |_| {
                                                let pid = chat_pid.clone();
                                                let name = chat_name.clone();
                                                ctx.start_direct_with(pid, name);
                                            }>
                                                <IconView icon=Icon::Chat size=16 />
                                            </button>
                                            <button class="icon" title={move || ctx.t("pair.verify.match")} on:click=move |_| {
                                                let pid = verify_pid.clone();
                                                verify(pid);
                                            }>
                                                <IconView icon=Icon::Verify size=16 />
                                            </button>
                                            <button class="icon" title={move || ctx.t("peer.block")} on:click=move |_| {
                                                let pid = block_pid.clone();
                                                block(pid);
                                            }>
                                                <IconView icon=Icon::Ban size=16 />
                                            </button>
                                        }.into_any()
                                    } else { ().into_any() }}
                                </span>
                            </div>
                        }.into_any()
                    }).collect::<Vec<AnyView>>()
                }
            }}
        </div>
    }
}
