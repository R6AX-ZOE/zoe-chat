#!/data/data/com.termux/files/usr/bin/env bash
# zoe-chat Termux 构建与测试脚本。
#
# 用法:
#   bash build.sh            # 构建 zoe-cli + zoe-daemon(debug)
#   bash build.sh --release  # release 构建
#   bash build.sh --test     # 额外跑 BLE mesh mock 测试(无硬件依赖,真机上验证协议栈)
#   bash build.sh --all      # 全 workspace 构建
#
# 说明:
#   - Termux 的 Rust 目标为 aarch64-linux-android(target_os=android),
#     因此 ble-linux(bluer/BlueZ)驱动不会在手机上编译——BLE 真机链路验证
#     请配合 Linux 端 `zoe-cli ble adv --echo` 与 tools/ble-gatt-test 使用,
#     详见 docs/termux-ble.md。
set -euo pipefail

cd "$(dirname "$0")/../.."   # 仓库根目录

MODE=""
if [ "${1:-}" = "--release" ]; then MODE="--release"; fi

echo "== 构建 zoe-cli + zoe-daemon ${MODE:-}(debug) =="
cargo build $MODE -p zoe-cli -p zoe-daemon

if [ "${1:-}" = "--test" ] || [ "${2:-}" = "--test" ]; then
  echo "== BLE mesh overlay mock 测试(纯协议栈,无需硬件)=="
  cargo test -p zoe-transport --features ble-linux
fi

if [ "${1:-}" = "--all" ] || [ "${2:-}" = "--all" ]; then
  echo "== 全 workspace 构建 =="
  cargo build $MODE --workspace
fi

echo
echo "完成。可执行文件: $PREFIX/bin 或 target/*/zoe-cli,zoe-daemon"
echo "下一步见 docs/termux-ble.md(守护进程运行: bash scripts/termux/run-daemon.sh)"
