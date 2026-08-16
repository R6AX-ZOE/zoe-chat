// 守护进程 API 客户端:Bearer token 认证,统一错误处理。

export const TOKEN_KEY = 'zoe.token';

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string): void {
  localStorage.setItem(TOKEN_KEY, token);
}

export function clearToken(): void {
  localStorage.removeItem(TOKEN_KEY);
}

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const token = getToken();
  const headers: Record<string, string> = { ...(init?.headers as Record<string, string>) };
  if (token) headers['Authorization'] = `Bearer ${token}`;
  const res = await fetch(path, { ...init, headers });
  if (res.status === 401) {
    clearToken();
    throw new ApiError(401, 'unauthorized');
  }
  const body = await res.json().catch(() => null);
  if (!res.ok) {
    throw new ApiError(res.status, body?.error?.message ?? `HTTP ${res.status}`);
  }
  return body as T;
}

export function login(token: string): Promise<{ ok: boolean }> {
  return request('/api/v1/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ token }),
  });
}

export interface Me {
  user_id: string;
  fingerprint: string;
  created_at: number;
  device: { name: string; signature_public_key: string };
  started_at: number;
}

export function me(): Promise<Me> {
  return request('/api/v1/me');
}

export interface Card {
  peer_id: string;
  fingerprint: string;
  qr_svg: string;
}

export function card(): Promise<Card> {
  return request('/api/v1/card');
}

export function importCard(text: string): Promise<{ ok: boolean }> {
  return request('/api/v1/card/import', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text }),
  });
}

export interface Peer {
  peer_id: string;
  fingerprint: string;
  display_name: string | null;
  trust_status: number;
}

export function peers(): Promise<Peer[]> {
  return request('/api/v1/peers');
}

export interface Group {
  group_id: string;
  name: string | null;
  epoch: number;
  coordinator: string | null;
  members: number[];
  created_at: number;
}

export function groups(): Promise<Group[]> {
  return request('/api/v1/groups');
}

export function createGroup(name: string): Promise<Group> {
  return request('/api/v1/groups', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name }),
  });
}

export interface Message {
  id: number;
  seq: number | null;
  direction: number; // 0=in 1=out
  status: number; // 0=pending 1=delivered 2=read 3=failed
  text: string | null;
  received_at: number;
}

export function messages(groupId: string, limit = 100): Promise<Message[]> {
  return request(`/api/v1/groups/${encodeURIComponent(groupId)}/messages?limit=${limit}`);
}

export function sendMessage(groupId: string, text: string): Promise<{ id: number }> {
  return request(`/api/v1/groups/${encodeURIComponent(groupId)}/messages`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text }),
  });
}

export function backupMnemonic(): Promise<{ mnemonic: string }> {
  return request('/api/v1/backup/mnemonic');
}

export interface Settings {
  ui_theme: string | null;
  ui_language: string | null;
}

export function getSettings(): Promise<Settings> {
  return request('/api/v1/settings');
}

export function saveSettings(s: Partial<Settings>): Promise<Settings> {
  return request('/api/v1/settings', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(s),
  });
}

export interface TransportStatus {
  ble: string;
  lan: string;
  net: string;
  loopback: string;
  net_peers?: number;
}

export function transports(): Promise<TransportStatus> {
  return request('/api/v1/transports');
}

export interface NetAddr {
  peer_id: string;
  listen_addrs: string[];
}

export function netAddr(): Promise<NetAddr> {
  return request('/api/v1/net/addr');
}

export function netDial(addr: string): Promise<{ ok: boolean }> {
  return request('/api/v1/net/dial', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ addr }),
  });
}

export function invite(groupId: string, addr: string): Promise<{ ok: boolean }> {
  return request(`/api/v1/groups/${encodeURIComponent(groupId)}/invite`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ addr }),
  });
}

/** 事件流(WS)。断线自动重连。 */
export function connectEvents(onEvent: (e: { type: string; [k: string]: unknown }) => void): void {
  let ws: WebSocket | null = null;
  let closed = false;

  const open = (): void => {
    if (closed) return;
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const token = getToken();
    ws = new WebSocket(`${proto}://${location.host}/api/v1/events?token=${encodeURIComponent(token ?? '')}`);
    ws.onmessage = (m) => {
      try {
        onEvent(JSON.parse(String(m.data)));
      } catch {
        /* 忽略坏帧 */
      }
    };
    ws.onclose = () => {
      ws = null;
      setTimeout(open, 2000); // 2s 后重连
    };
  };

  open();
}
