import * as api from './api.js';
import { t, getLang, setLang } from './i18n.js';
import { getTheme, setTheme, applyTheme } from './theme.js';
import { icon } from './icons.js';
const state = {
    token: null,
    me: null,
    groups: [],
    peers: [],
    currentGroupId: null,
    card: null,
    mnemonic: null,
    view: 'thread',
    transports: null,
    net: null,
};
const app = document.getElementById('app');
function esc(s) {
    return String(s ?? '')
        .replaceAll('&', '&amp;')
        .replaceAll('<', '&lt;')
        .replaceAll('>', '&gt;')
        .replaceAll('"', '&quot;');
}
function fmtTime(ts) {
    const d = new Date(ts * 1000);
    const p = (n) => String(n).padStart(2, '0');
    return `${p(d.getHours())}:${p(d.getMinutes())}`;
}
function short(id) {
    return id.length > 16 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;
}
async function copyText(text) {
    await navigator.clipboard.writeText(text);
}
function setView(v) {
    state.view = v;
    document.body.dataset.view = v;
    document.body.scrollTop = 0;
}
function renderLogin() {
    app.innerHTML = `
    <div class="login">
      <div class="login-card">
        <div class="logo">${icon('lock', 40)}</div>
        <h1>${esc(t('app.title'))}</h1>
        <p class="sub">${esc(t('app.tagline'))}</p>
        <div class="field">
          <label for="token">${esc(t('login.token'))}</label>
          <input id="token" type="password" autocomplete="off" spellcheck="false">
        </div>
        <button id="login-btn" class="primary">${icon('shield')} ${esc(t('login.submit'))}</button>
        <div class="error" id="login-error"></div>
      </div>
    </div>`;
    const input = document.getElementById('token');
    input.focus();
    const doLogin = async () => {
        const token = input.value.trim();
        if (!token)
            return;
        try {
            await api.login(token);
            api.setToken(token);
            await boot();
        }
        catch {
            document.getElementById('login-error').textContent = t('login.error');
        }
    };
    document.getElementById('login-btn').addEventListener('click', doLogin);
    input.addEventListener('keydown', (e) => {
        if (e.key === 'Enter')
            void doLogin();
    });
}
function renderApp() {
    const transportDots = () => {
        const ts = state.transports;
        const items = [
            ['transport.ble', 'ble'],
            ['transport.lan', 'lan'],
            ['transport.net', 'net'],
            ['transport.loopback', 'loopback'],
        ];
        return `<span class="transport-dots" title="${items
            .map(([k, key]) => `${t(k)}: ${t(ts?.[key] === 'up' ? 'transport.up' : 'transport.down')}`)
            .join(' · ')}">${items
            .map(([, key]) => `<span class="dot ${ts?.[key] === 'up' ? 'up' : 'down'}" title="${esc(t(`transport.${key}`))}"></span>`)
            .join('')}</span>`;
    };
    app.innerHTML = `
    <div class="app">
      <header class="app-header">
        <div class="brand">
          <span style="color:var(--accent)">${icon('lock')}</span>
          <span class="name">${esc(t('app.title'))}</span>
        </div>
        <div class="actions">
          ${transportDots()}
          <button class="icon" id="theme-btn" title="${esc(t('settings.theme'))}"></button>
          <button class="icon" id="lang-btn" title="${esc(t('settings.language'))}">${icon('globe')}</button>
        </div>
      </header>
      <div class="main">
        <aside class="sidebar">
          <div class="sidebar-head">
            <button class="icon" id="new-group-btn" title="${esc(t('chat.newGroup'))}">${icon('plus')}</button>
            <button class="icon" id="settings-btn" title="${esc(t('nav.settings'))}">${icon('gear')}</button>
          </div>
          <div class="sidebar-list" id="group-list"></div>
        </aside>
        <section class="thread" id="thread"></section>
        <aside class="details" id="details"></aside>
      </div>
    </div>`;
    document.getElementById('theme-btn').addEventListener('click', () => {
        const order = ['system', 'light', 'dark'];
        const next = order[(order.indexOf(getTheme()) + 1) % order.length];
        setTheme(next);
        syncThemeButton();
    });
    document.getElementById('lang-btn').addEventListener('click', () => {
        const next = getLang() === 'zh-CN' ? 'en-US' : 'zh-CN';
        setLang(next);
        void syncSettingsToServer();
        void boot();
    });
    document.getElementById('new-group-btn').addEventListener('click', () => {
        const name = window.prompt(t('chat.groupName'))?.trim();
        if (name) {
            void api.createGroup(name).then(() => refreshGroups());
        }
    });
    document.getElementById('settings-btn').addEventListener('click', () => {
        setView('settings');
        void renderSettings();
    });
    syncThemeButton();
    void refreshGroups();
    void refreshTransports();
}
function syncThemeButton() {
    const btn = document.getElementById('theme-btn');
    if (!btn)
        return;
    const theme = getTheme();
    const iconName = theme === 'dark' ? 'moon' : theme === 'light' ? 'sun' : 'monitor';
    btn.innerHTML = icon(iconName);
}
function renderGroupList() {
    const list = document.getElementById('group-list');
    if (!list)
        return;
    if (state.groups.length === 0) {
        list.innerHTML = `<div class="empty-hint">${esc(t('chat.empty'))}</div>`;
        return;
    }
    list.innerHTML = state.groups
        .map((g) => `
      <div class="group-item ${g.group_id === state.currentGroupId ? 'active' : ''}" data-group="${esc(g.group_id)}">
        <span class="gicon">${icon('chat')}</span>
        <span class="gmeta">
          <span class="gname">${esc(g.name ?? short(g.group_id))}</span>
          <span class="gsub">${g.members.length} ${esc(t('chat.members'))} · e${g.epoch}</span>
        </span>
        <span>${icon('dots')}</span>
      </div>`)
        .join('');
    list.querySelectorAll('.group-item').forEach((el) => {
        el.addEventListener('click', () => {
            state.currentGroupId = el.getAttribute('data-group');
            void renderThread();
            void renderGroupList();
            renderDetails();
        });
    });
}
function renderThread() {
    const thread = document.getElementById('thread');
    if (!thread)
        return;
    const group = state.groups.find((g) => g.group_id === state.currentGroupId);
    if (!group) {
        thread.innerHTML = `<div class="empty-hint">${esc(t('chat.empty'))}</div>`;
        return;
    }
    thread.innerHTML = `
    <div class="thread-head">
      <button class="icon back-btn" id="back-btn" title="${esc(t('common.back'))}">${icon('back')}</button>
      <div class="title">${esc(group.name ?? short(group.group_id))}</div>
      <div class="sub">${group.members.length} ${esc(t('chat.members'))} · e${group.epoch}</div>
    </div>
    <div class="thread-messages" id="msg-list"></div>
    <div class="thread-input">
      <textarea id="msg-input" rows="1" placeholder="${esc(t('chat.placeholder'))}"></textarea>
      <button class="icon primary" id="send-btn" title="${esc(t('chat.send'))}">${icon('send')}</button>
    </div>`;
    document.getElementById('back-btn').addEventListener('click', () => {
        state.currentGroupId = null;
        setView('thread');
        void renderThread();
        renderGroupList();
    });
    document.getElementById('send-btn').addEventListener('click', () => void doSend());
    const input = document.getElementById('msg-input');
    input.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            void doSend();
        }
    });
    void refreshMessages();
}
async function doSend() {
    const input = document.getElementById('msg-input');
    if (!input || !state.currentGroupId)
        return;
    const text = input.value.trim();
    if (!text)
        return;
    input.value = '';
    try {
        await api.sendMessage(state.currentGroupId, text);
        await refreshMessages();
    }
    catch (err) {
        input.value = text;
        console.error('send failed', err);
    }
}
async function refreshMessages() {
    const list = document.getElementById('msg-list');
    if (!list || !state.currentGroupId)
        return;
    const msgs = await api.messages(state.currentGroupId);
    if (msgs.length === 0) {
        list.innerHTML = `<div class="empty-hint">${esc(t('chat.empty'))}</div>`;
        return;
    }
    const statusText = (s) => s === 0 ? t('chat.pending') : s === 1 ? t('chat.delivered') : s === 3 ? t('chat.failed') : '';
    list.innerHTML = msgs
        .map((m) => `
      <div class="msg ${m.direction === 1 ? 'out' : 'in'}">
        <div class="bubble">${esc(m.text ?? '')}</div>
        <div class="meta">${fmtTime(m.received_at)}${m.direction === 1 && statusText(m.status)
        ? ` · <span class="status">${icon('check', 12)} ${esc(statusText(m.status))}</span>`
        : ''}</div>
      </div>`)
        .join('');
    list.scrollTop = list.scrollHeight;
}
function renderDetails() {
    const details = document.getElementById('details');
    if (!details)
        return;
    const group = state.groups.find((g) => g.group_id === state.currentGroupId);
    details.innerHTML = `
    <div class="panel-section">
      <h3>${esc(t('chat.members'))}</h3>
      ${group
        ? group.members
            .map((m) => `<div class="kv"><span class="k">${esc(t('chat.you'))} · leaf ${m}</span></div>`)
            .join('')
        : `<div class="note">${esc(t('chat.empty'))}</div>`}
      ${group ? `<div class="kv"><span class="k">${esc(t('chat.epoch'))}</span><span class="v">${group.epoch}</span></div>` : ''}
    </div>
    <div class="panel-section">
      <h3>${esc(t('chat.invite'))}</h3>
      <p class="note" style="margin-bottom:8px">${esc(t('settings.network.desc'))}</p>
      <input id="invite-addr" type="text" placeholder="${esc(t('chat.invite.placeholder'))}" spellcheck="false">
      <div class="row" style="margin-top:8px">
        <button id="invite-btn" class="primary">${esc(t('chat.invite'))}</button>
        <span class="note" id="invite-msg"></span>
      </div>
    </div>
    <div class="panel-section">
      <h3>${esc(t('settings.identity'))}</h3>
      ${state.card ? `<div class="qr-box">${state.card.qr_svg}</div>` : ''}
      ${state.card ? `<div class="kv"><span class="k">${esc(t('settings.fingerprint'))}</span><span class="v">${esc(short(state.card.fingerprint))}</span></div>` : ''}
    </div>`;
    document.getElementById('invite-btn').addEventListener('click', async () => {
        const input = document.getElementById('invite-addr');
        const msg = document.getElementById('invite-msg');
        const addr = input.value.trim();
        if (!addr || !state.currentGroupId)
            return;
        msg.textContent = t('chat.invite.wait');
        try {
            await api.invite(state.currentGroupId, addr);
            msg.textContent = t('chat.invite.ok');
            msg.style.color = 'var(--ok)';
            void refreshGroups();
        }
        catch {
            msg.textContent = t('chat.invite.err');
            msg.style.color = 'var(--danger)';
        }
    });
}
async function renderSettings() {
    const details = document.getElementById('details');
    if (!details)
        return;
    const card = await api.card();
    state.card = card;
    const peers = await api.peers();
    details.innerHTML = `
    <div class="panel-section">
      <h3>${esc(t('settings.theme'))}</h3>
      <div class="row">
        ${['light', 'dark', 'system']
        .map((th) => `<button class="${getTheme() === th ? 'primary' : ''}" data-theme-set="${th}">${esc(t(`settings.theme.${th}`))}</button>`)
        .join('')}
      </div>
    </div>
    <div class="panel-section">
      <h3>${esc(t('settings.language'))}</h3>
      <div class="row">
        ${['zh-CN', 'en-US']
        .map((l) => `<button class="${getLang() === l ? 'primary' : ''}" data-lang-set="${l}">${l}</button>`)
        .join('')}
      </div>
    </div>
    <div class="panel-section">
      <h3>${esc(t('settings.card'))}</h3>
      <p class="note" style="margin-bottom:8px">${esc(t('settings.card.desc'))}</p>
      <div class="qr-box">${card.qr_svg}</div>
      <div class="kv"><span class="k">${esc(t('settings.fingerprint'))}</span><span class="v">${esc(card.fingerprint)}</span></div>
      <div class="kv"><span class="k">${esc(t('settings.peerId'))}</span><span class="v">${esc(short(card.peer_id))}</span></div>
      <div class="row" style="margin-top:8px">
        <button id="copy-card">${icon('copy')} ${esc(t('settings.copy'))}</button>
      </div>
    </div>
    <div class="panel-section">
      <h3>${esc(t('settings.import'))}</h3>
      <p class="note" style="margin-bottom:8px">${esc(t('settings.import.desc'))}</p>
      <textarea id="import-text" rows="2" placeholder="${esc(t('settings.import.placeholder'))}"></textarea>
      <div class="row" style="margin-top:8px">
        <button id="import-btn" class="primary">${esc(t('settings.import.submit'))}</button>
        <span class="note" id="import-msg"></span>
      </div>
    </div>
    <div class="panel-section">
      <h3>${esc(t('settings.backup'))}</h3>
      <p class="note" style="margin-bottom:8px">${esc(t('settings.backup.desc'))}</p>
      <div class="row">
        <button id="backup-btn">${icon('download')} ${esc(t('settings.backup.show'))}</button>
      </div>
      <div id="backup-area" class="hidden"></div>
      <p class="warn" id="backup-warn" style="margin-top:6px"></p>
    </div>
    <div class="panel-section">
      <h3>${esc(t('settings.network'))}</h3>
      <p class="note" style="margin-bottom:8px">${esc(t('settings.network.desc'))}</p>
      ${state.net ? `<div class="kv"><span class="k">${esc(t('settings.peerId'))}</span><span class="v">${esc(short(state.net.peer_id))}</span></div>` : ''}
      ${state.net && state.net.listen_addrs.length > 0 ? `<div class="kv"><span class="k">${esc(t('settings.listenAddr'))}</span><span class="v">${esc(short(state.net.listen_addrs[0]))}</span></div>` : ''}
      <div class="kv"><span class="k">${esc(t('settings.netPeers'))}</span><span class="v">${state.transports?.net_peers ?? 0}</span></div>
      <input id="dial-addr" type="text" placeholder="${esc(t('settings.dial.placeholder'))}" spellcheck="false" style="margin-top:8px">
      <div class="row" style="margin-top:8px">
        <button id="dial-btn">${esc(t('settings.dial'))}</button>
        <span class="note" id="dial-msg"></span>
      </div>
    </div>
    <div class="panel-section">
      <h3>${esc(t('settings.transports'))}</h3>
      ${['ble', 'lan', 'net', 'loopback']
        .map((k) => {
        const up = state.transports?.[k] === 'up';
        return `<div class="kv"><span class="k">${esc(t(`transport.${k}`))}</span><span class="v" style="color:${up ? 'var(--ok)' : 'var(--danger)'}">${esc(t(up ? 'transport.up' : 'transport.down'))}</span></div>`;
    })
        .join('')}
      ${peers.length > 0 ? `<h3 style="margin-top:14px">${esc(t('nav.chats'))}</h3>${peers.map((p) => `<div class="kv"><span class="k">${esc(p.display_name ?? short(p.fingerprint))}</span><span class="v">${p.trust_status === 1 ? '✓' : ''}</span></div>`).join('')}` : ''}
    </div>`;
    details.querySelectorAll('[data-theme-set]').forEach((b) => b.addEventListener('click', () => {
        setTheme(b.getAttribute('data-theme-set'));
        void renderSettings();
        syncThemeButton();
    }));
    details.querySelectorAll('[data-lang-set]').forEach((b) => b.addEventListener('click', () => {
        setLang(b.getAttribute('data-lang-set'));
        void syncSettingsToServer();
        void boot();
    }));
    document.getElementById('copy-card').addEventListener('click', async () => {
        await copyText(`zoe://peer/${card.peer_id}/${card.fingerprint}`);
    });
    document.getElementById('import-btn').addEventListener('click', async () => {
        const text = document.getElementById('import-text').value.trim();
        const msg = document.getElementById('import-msg');
        try {
            await api.importCard(text);
            msg.textContent = t('settings.import.ok');
            msg.style.color = 'var(--ok)';
        }
        catch {
            msg.textContent = t('settings.import.err');
            msg.style.color = 'var(--danger)';
        }
    });
    document.getElementById('dial-btn').addEventListener('click', async () => {
        const input = document.getElementById('dial-addr');
        const msg = document.getElementById('dial-msg');
        const addr = input.value.trim();
        if (!addr)
            return;
        try {
            await api.netDial(addr);
            msg.textContent = t('settings.dial.ok');
            msg.style.color = 'var(--ok)';
        }
        catch {
            msg.textContent = t('settings.dial.err');
            msg.style.color = 'var(--danger)';
        }
    });
    document.getElementById('backup-btn').addEventListener('click', async () => {
        const area = document.getElementById('backup-area');
        const warn = document.getElementById('backup-warn');
        if (!state.mnemonic) {
            state.mnemonic = (await api.backupMnemonic()).mnemonic;
        }
        area.classList.toggle('hidden');
        if (!area.classList.contains('hidden')) {
            area.innerHTML = `<div class="mnemonic">${esc(state.mnemonic)}</div>`;
            warn.textContent = t('settings.backup.warning');
        }
        else {
            warn.textContent = '';
        }
    });
}
async function refreshGroups() {
    state.groups = await api.groups();
    renderGroupList();
    if (state.currentGroupId && !state.groups.some((g) => g.group_id === state.currentGroupId)) {
        state.currentGroupId = null;
    }
    if (!state.currentGroupId && state.groups.length > 0) {
        state.currentGroupId = state.groups[0].group_id;
    }
    renderGroupList();
    void renderThread();
    renderDetails();
}
async function refreshTransports() {
    state.transports = await api.transports();
    try {
        state.net = await api.netAddr();
    }
    catch {
        state.net = null;
    }
    const dots = document.querySelector('.transport-dots');
    if (dots)
        dots.outerHTML = dots.outerHTML;
    renderAppHeader();
}
function renderAppHeader() {
    const header = document.querySelector('.app-header .actions');
    if (!header)
        return;
    const ts = state.transports;
    const items = [
        ['transport.ble', 'ble'],
        ['transport.lan', 'lan'],
        ['transport.net', 'net'],
        ['transport.loopback', 'loopback'],
    ];
    const dots = header.querySelector('.transport-dots');
    if (dots) {
        dots.innerHTML = items
            .map(([, key]) => `<span class="dot ${ts?.[key] === 'up' ? 'up' : 'down'}" title="${esc(t(`transport.${key}`))}"></span>`)
            .join('');
    }
}
async function syncSettingsToServer() {
    try {
        await api.saveSettings({ ui_theme: getTheme(), ui_language: getLang() });
    }
    catch {
    }
}
async function boot() {
    const token = api.getToken();
    if (!token) {
        renderLogin();
        return;
    }
    try {
        state.me = await api.me();
        state.card = await api.card();
        state.transports = await api.transports();
    }
    catch {
        renderLogin();
        return;
    }
    setLang(getLang());
    applyTheme();
    renderApp();
    setView('thread');
    api.connectEvents((e) => {
        if (e.type === 'message')
            void refreshMessages();
        if (e.type === 'group')
            void refreshGroups();
        if (e.type === 'transport')
            void refreshTransports();
    });
}
void boot();
