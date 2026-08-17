#!/usr/bin/env bash
# CI:init 之后、build 之前,把 android/gen-patches/ 的 MainActivity.kt 与 AndroidManifest.xml
# cp 覆盖到 gen/android(M1 起生效)。
#
# 背景:gen/android 不入库(gitignore),由 CI 每次 `npx tauri android init` 生成(坑 4),
# 所以 MainActivity/Manifest 的修改不能直接改文件,只能走本 patch 脚本在 CI 覆盖。
# patch 内容以 tauri-cli 官方模板为底(MainActivity 加权限申请 + Bridge.start;
# Manifest 加 BLE 权限/uses-feature),与模板同构 —— 见 android/gen-patches/ 两文件。
#
# 顺序:init → sync-android-ble.sh → patch-android-gen.sh → build。
set -euo pipefail
cd "$(dirname "$0")/.."

SRC="android/gen-patches"
MAIN_SRC="$SRC/MainActivity.kt"
MANIFEST_SRC="$SRC/AndroidManifest.xml"

GEN="app/src-tauri/gen/android/app/src/main"
MAIN_DST="$GEN/java/com/zoechat/mobile/MainActivity.kt"
MANIFEST_DST="$GEN/AndroidManifest.xml"

for f in "$MAIN_SRC" "$MANIFEST_SRC"; do
  if [ ! -f "$f" ]; then
    echo "error: 缺少 patch 文件 $f" >&2
    exit 1
  fi
done
if [ ! -f "$MANIFEST_DST" ] || [ ! -f "$MAIN_DST" ]; then
  echo "error: gen/android 不存在或结构不符(先跑 npx tauri android init)" >&2
  exit 1
fi

cp -v "$MAIN_SRC" "$MAIN_DST"
cp -v "$MANIFEST_SRC" "$MANIFEST_DST"
echo "patched: MainActivity.kt / AndroidManifest.xml"
