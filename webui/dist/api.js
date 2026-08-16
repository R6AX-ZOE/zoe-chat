export const TOKEN_KEY = 'zoe.token';
export function getToken() {
    return localStorage.getItem(TOKEN_KEY);
}
export function setToken(token) {
    localStorage.setItem(TOKEN_KEY, token);
}
export function clearToken() {
    localStorage.removeItem(TOKEN_KEY);
}
export class ApiError extends Error {
    constructor(status, message) {
        super(message);
        this.status = status;
    }
}
async function request(path, init) {
    const token = getToken();
    const headers = { ...init?.headers };
    if (token)
        headers['Authorization'] = `Bearer ${token}`;
    const res = await fetch(path, { ...init, headers });
    if (res.status === 401) {
        clearToken();
        throw new ApiError(401, 'unauthorized');
    }
    const body = await res.json().catch(() => null);
    if (!res.ok) {
        throw new ApiError(res.status, body?.error?.message ?? `HTTP ${res.status}`);
    }
    return body;
}
export function login(token) {
    return request('/api/v1/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ token }),
    });
}
export function me() {
    return request('/api/v1/me');
}
export function card() {
    return request('/api/v1/card');
}
export function importCard(text) {
    return request('/api/v1/card/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ text }),
    });
}
export function peers() {
    return request('/api/v1/peers');
}
export function groups() {
    return request('/api/v1/groups');
}
export function createGroup(name) {
    return request('/api/v1/groups', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
    });
}
export function messages(groupId, limit = 100) {
    return request(`/api/v1/groups/${encodeURIComponent(groupId)}/messages?limit=${limit}`);
}
export function sendMessage(groupId, text) {
    return request(`/api/v1/groups/${encodeURIComponent(groupId)}/messages`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ text }),
    });
}
export function backupMnemonic() {
    return request('/api/v1/backup/mnemonic');
}
export function getSettings() {
    return request('/api/v1/settings');
}
export function saveSettings(s) {
    return request('/api/v1/settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(s),
    });
}
export function transports() {
    return request('/api/v1/transports');
}
export function netAddr() {
    return request('/api/v1/net/addr');
}
export function netDial(addr) {
    return request('/api/v1/net/dial', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ addr }),
    });
}
export function invite(groupId, addr) {
    return request(`/api/v1/groups/${encodeURIComponent(groupId)}/invite`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ addr }),
    });
}
export function connectEvents(onEvent) {
    let ws = null;
    let closed = false;
    const open = () => {
        if (closed)
            return;
        const proto = location.protocol === 'https:' ? 'wss' : 'ws';
        const token = getToken();
        ws = new WebSocket(`${proto}://${location.host}/api/v1/events?token=${encodeURIComponent(token ?? '')}`);
        ws.onmessage = (m) => {
            try {
                onEvent(JSON.parse(String(m.data)));
            }
            catch {
            }
        };
        ws.onclose = () => {
            ws = null;
            setTimeout(open, 2000);
        };
    };
    open();
}
