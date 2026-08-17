import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const logView = document.getElementById("log");
const MAX_CHARS = 40000;

function log(line) {
  const t = new Date().toLocaleTimeString();
  logView.textContent += `[${t}] ${line}\n`;
  if (logView.textContent.length > MAX_CHARS) {
    logView.textContent = logView.textContent.slice(-MAX_CHARS);
  }
  logView.scrollTop = logView.scrollHeight;
}

async function refreshInfo() {
  try {
    const info = await invoke("app_info");
    log(`zoe-mobile v${info.appVersion} / zoe-core ${info.zoeCoreVersion}`);
    log(`示例帧: ${info.frameExampleHex}`);
  } catch (e) {
    log(`app_info 失败: ${e}`);
  }
}

async function runHello() {
  try {
    const out = await invoke("hello_frame");
    log(out);
  } catch (e) {
    log(`hello_frame 失败: ${e}`);
  }
}

// M1:回环 TCP 桥 —— 启动 BLE / 停止 / echo 开关 / 状态轮询(见 docs/tauri-mobile.md)
const statusEl = document.getElementById("bridgeStatus");
const btnEcho = document.getElementById("btnEcho");
let echoOn = true;
let lastConnected = null;

async function refreshBridgeStatus() {
  try {
    const s = await invoke("bridge_status");
    if (s.connected !== lastConnected) {
      lastConnected = s.connected;
      log(`[桥] 状态: ${s.connected ? "已连接(Kotlin)" : "未连接"}`);
    }
    statusEl.textContent = `桥: ${s.connected ? "已连接" : "未连接"}${
      s.lastError ? ` · ${s.lastError}` : ""
    }`;
  } catch (e) {
    statusEl.textContent = "桥: 状态查询失败";
  }
}

document.getElementById("btnBridgeStart").addEventListener("click", async () => {
  try {
    const r = await invoke("start_bridge");
    log(`[桥] start_bridge → ${r}`);
  } catch (e) {
    log(`start_bridge 失败: ${e}`);
  }
});

document.getElementById("btnBridgeStop").addEventListener("click", async () => {
  try {
    const r = await invoke("stop_bridge");
    log(`[桥] stop_bridge → ${r}`);
  } catch (e) {
    log(`stop_bridge 失败: ${e}`);
  }
});

btnEcho.addEventListener("click", async () => {
  echoOn = !echoOn;
  try {
    await invoke("set_echo", { v: echoOn });
    btnEcho.textContent = `echo: ${echoOn ? "开" : "关"}`;
    log(`[桥] echo=${echoOn ? "开" : "关"}`);
  } catch (e) {
    echoOn = !echoOn; // 失败回滚按钮状态
    log(`set_echo 失败: ${e}`);
  }
});

// M1 预留:回环 TCP 桥日志(见 docs/tauri-mobile.md)
listen("bridge-log", (e) => log(`[桥] ${e.payload}`)).catch((e) =>
  log(`listen 失败: ${e}`)
);

document.getElementById("btnInfo").addEventListener("click", refreshInfo);
document.getElementById("btnHello").addEventListener("click", runHello);

log("zoe-mobile 控制台就绪(M1)");
refreshInfo();
setInterval(refreshBridgeStatus, 2000);
