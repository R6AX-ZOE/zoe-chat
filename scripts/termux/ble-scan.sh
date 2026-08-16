#!/data/data/com.termux/files/usr/bin/env bash
# zoe-chat BLE 真机联调:用 termux-api 扫描附近 BLE 设备,验证对端广播。
#
# 用法:
#   ble-scan.sh [--count N] [--interval SECS] [--filter STR] [--raw]
#   ble-scan.sh --wait-for STR [--timeout SECS] [--interval SECS]
#
# 选项:
#   --count N       扫描次数(默认 1)
#   --interval SECS 两次扫描间隔(默认 3)
#   --filter STR    只显示名称或 MAC 含 STR(大小写不敏感)的设备
#   --wait-for STR  循环扫描直到发现名称/MAC 含 STR 的设备(退出码 0)
#   --timeout SECS  wait 模式总超时(默认 60);其余模式单次扫描时长
#   --raw           直接输出 termux-bluetooth-scan 的原始 JSON
#
# 退出码: 0 成功/找到;2 termux-api 缺失;3 蓝牙不可用;4 超时未找到
#
# 前提:已 pkg install termux-api 并授予 Termux:API 『附近设备/位置』权限;
#       蓝牙与定位开关已开启,屏幕保持亮起(Android 8.1+ 熄屏停止扫描)。

set -uo pipefail
PREFIX="${PREFIX:-/data/data/com.termux/files/usr}"

if ! command -v termux-bluetooth-scan >/dev/null 2>&1; then
  echo "错误: 未找到 termux-bluetooth-scan。请先: pkg install termux-api" >&2
  exit 2
fi
if ! command -v termux-bluetooth-turn-on >/dev/null 2>&1; then
  echo "错误: 未找到 termux-bluetooth-turn-on。请 pkg install termux-api" >&2
  exit 2
fi

COUNT=1; INTERVAL=3; FILTER=""; WAIT_FOR=""; TIMEOUT=60; RAW=0
while [ $# -gt 0 ]; do
  case "$1" in
    --count)    COUNT="${2:-1}"; shift 2 ;;
    --interval) INTERVAL="${2:-3}"; shift 2 ;;
    --filter)   FILTER="${2:-}"; shift 2 ;;
    --wait-for) WAIT_FOR="${2:-}"; shift 2 ;;
    --timeout)  TIMEOUT="${2:-60}"; shift 2 ;;
    --raw)      RAW=1; shift ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

echo "== 开启蓝牙 =="
termux-bluetooth-turn-on 2>/dev/null || true
sleep 1
if ! termux-bluetooth-scan >/dev/null 2>&1; then
  echo "错误: 蓝牙扫描失败。请检查: 蓝牙已开、定位已开、Termux:API 权限已授予。" >&2
  exit 3
fi

scan_once() {
  local out
  out="$(termux-bluetooth-scan 2>/dev/null)"
  if command -v python3 >/dev/null 2>&1; then
    printf '%s\n' "$out" | python3 -c '
import json, sys
try:
    devs = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for d in devs or []:
    addr = d.get("address", "?")
    name = d.get("name") or ""
    rssi = d.get("rssi", "?")
    print(f"{addr}\t{name}\t{rssi}")
'
  elif command -v jq >/dev/null 2>&1; then
    printf '%s\n' "$out" | jq -r '.[]? | [.address, (.name // ""), (.rssi // "?")] | @tsv'
  else
    printf '%s\n' "$out"
  fi
}

if [ "$RAW" -eq 1 ]; then
  termux-bluetooth-scan
  exit 0
fi

if [ -n "$WAIT_FOR" ]; then
  echo "等待设备(名称/MAC 含 '${WAIT_FOR}'),最多 ${TIMEOUT}s ..."
  DEADLINE=$(( $(date +%s) + TIMEOUT ))
  while [ "$(date +%s)" -lt "$DEADLINE" ]; do
    while IFS=$'\t' read -r addr name rssi; do
      [ -z "$addr" ] && continue
      if echo "$addr $name" | grep -qi "$WAIT_FOR"; then
        echo "已找到: $addr  $name  (RSSI ${rssi:-?})"
        exit 0
      fi
    done < <(scan_once)
    sleep "$INTERVAL"
  done
  echo "超时: ${TIMEOUT}s 内未发现匹配 '${WAIT_FOR}' 的设备。" >&2
  exit 4
fi

echo "== BLE 扫描(共 ${COUNT} 次,间隔 ${INTERVAL}s)=="
i=0
while [ "$i" -lt "$COUNT" ]; do
  i=$((i + 1))
  echo "--- 第 $i 次扫描 ---"
  FOUND=0
  while IFS=$'\t' read -r addr name rssi; do
    [ -z "$addr" ] && continue
    if [ -n "$FILTER" ] && ! echo "$addr $name" | grep -qi "$FILTER"; then
      continue
    fi
    printf "%-18s  %-24s  RSSI %s\n" "$addr" "$name" "${rssi:-?}"
    FOUND=1
  done < <(scan_once)
  [ "$FOUND" -eq 0 ] && echo "(未发现设备,确认对端已开始广播、屏幕亮起)"
  [ "$i" -lt "$COUNT" ] && sleep "$INTERVAL"
done

echo
echo "提示: zoe 节点广播名默认为 zoe-device,可用 --filter zoe 过滤;"
echo "      服务 UUID 为 7a5e0001-2e4c-4a31-9b6c-3c2a0e5f6a01。"
