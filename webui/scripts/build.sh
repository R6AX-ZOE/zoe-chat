#!/usr/bin/env bash
# 构建 zoe-webui 的 wasm 产物到 dist/(与 zoe-daemon 与 Tauri 移动端共用)。
# 前置:rustup target add wasm32-unknown-unknown;wasm-bindgen-cli 版本 = Cargo.lock 中 wasm-bindgen 版本。
# 注意:本文件必须无 BOM(Linux bash 会拒绝带 BOM 的 shebang)。
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET=wasm32-unknown-unknown
OUT=dist
BIN="zoe_webui"

cargo build --release --target "$TARGET"

# 从 Cargo.lock 取 wasm-bindgen 版本(要求 wasm-bindgen-cli 已装同版本)
WBG=$(awk '/^name = "wasm-bindgen"$/{getline; getline; print $3; exit}' Cargo.lock | tr -d '"')
if ! wasm-bindgen --version | grep -q "$WBG"; then
  echo "error: wasm-bindgen-cli version mismatch (need $WBG)" >&2
  exit 1
fi

mkdir -p "$OUT/assets"
wasm-bindgen --target web --out-dir "$OUT/assets" --no-typescript --out-name "$BIN" \
  "target/$TARGET/release/${BIN}.wasm"
cp static/index.html static/styles.css "$OUT/"
echo "dist ready: $(ls -la "$OUT" "$OUT/assets" | grep -E '\.(wasm|js|html|css)' | awk '{print $9, $5}')"