#!/usr/bin/env bash
# 把 android/(canonical Kotlin 源码)同步进 app/src-tauri/gen/android/。
# 用法: bash scripts/sync-android-ble.sh   (CI ubuntu / Linux)
set -euo pipefail
cd "$(dirname "$0")/.."

SRC="android/app/src/main/java/com/zoechat/ble"
DST="app/src-tauri/gen/android/app/src/main/java/com/zoechat/ble"

if [ ! -d "$DST" ]; then
  echo "error: $DST 不存在(先完成 M0:npx tauri android init 并提交 gen/android)" >&2
  exit 1
fi

mkdir -p "$DST"
for f in "$SRC"/*.kt; do
  cp -v "$f" "$DST"/
done
echo "synced: $SRC -> $DST"
