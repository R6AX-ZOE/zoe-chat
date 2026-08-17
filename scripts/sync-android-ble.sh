#!/usr/bin/env bash
# 把 android/(canonical Kotlin 源码)同步进 app/src-tauri/gen/android/。
# 用法: bash scripts/sync-android-ble.sh   (CI ubuntu / Linux)
set -euo pipefail
cd "$(dirname "$0")/.."

SRC="android/app/src/main/java/com/zoechat/ble"
DST="app/src-tauri/gen/android/app/src/main/java/com/zoechat/ble"

# gen/android 不入库(gitignore),CI 由 npx tauri android init 生成;
# 这里只校验 init 产物存在(ble/ 目录本身由本脚本创建)。
if [ ! -d "app/src-tauri/gen/android/app/src/main" ]; then
  echo "error: gen/android 不存在(先跑 npx tauri android init)" >&2
  exit 1
fi

mkdir -p "$DST"
for f in "$SRC"/*.kt; do
  cp -v "$f" "$DST"/
done
echo "synced: $SRC -> $DST"
