#!/data/data/com.termux/files/usr/bin/env bash
# zoe-chat Termux 守护进程运行脚本(无 BLE 依赖,走 libp2p net 传输)。
#
# 用法:
#   bash run-daemon.sh [--port N] [--token STR] [--dir PATH]
#
# 手机浏览器访问:守护进程只监听 127.0.0.1,需 SSH 端口转发:
#   ssh -L 127.0.0.1:PORT:127.0.0.1:PORT 用户@手机IP   # 在电脑上执行
#   然后电脑浏览器打开 http://127.0.0.1:PORT
# 或直接在手机浏览器打开 http://127.0.0.1:PORT(Termux 内可行)。
set -euo pipefail

cd "$(dirname "$0")/../.."

PORT=""; TOKEN=""; DIR="zoe-data"
while [ $# -gt 0 ]; do
  case "$1" in
    --port)   PORT="${2:-}"; shift 2 ;;
    --token)  TOKEN="${2:-}"; shift 2 ;;
    --dir)    DIR="${2:-}"; shift 2 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

BIN="target/debug/zoe-daemon"
[ -x "$BIN" ] || { echo "未找到 $BIN,先执行 bash scripts/termux/build.sh" >&2; exit 1; }

echo "== 启动 zoe-daemon(data-dir: $DIR)=="
exec "$BIN" --data-dir "$DIR" ${PORT:+--port "$PORT"} ${TOKEN:+--token "$TOKEN"}
