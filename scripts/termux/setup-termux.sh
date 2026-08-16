#!/data/data/com.termux/files/usr/bin/env bash
# zoe-chat Termux 真机联调环境初始化(Android 手机端)。
#
# 用法:  bash setup-termux.sh
# 说明:
#   - 安装构建依赖(rust/cargo/clang/openssl/termux-api 等);
#   - bluez 为尽力安装:Android 无 BlueZ 内核栈,通常不可用,失败不阻塞;
#   - 完成后见 docs/termux-ble.md 的联调流程。
set -euo pipefail

if [ ! -x "$PREFIX/bin/pkg" ]; then
  echo "错误: 这不是 Termux 环境(未找到 pkg)。请先在手机安装 Termux。" >&2
  exit 1
fi

echo "== 1/4 pkg update(仅刷新索引,不做 upgrade)=="
pkg update -y

echo "== 2/4 安装构建依赖 =="
pkg install -y \
  git \
  rust \
  cargo \
  clang \
  binutils \
  pkg-config \
  openssl \
  python \
  termux-api \
  || { echo "部分包安装失败,请检查网络后重试" >&2; exit 1; }

echo "== 3/4 尽力安装 bluez(Android 通常不可用,失败可忽略)=="
if pkg install -y bluez; then
  echo "bluez 已安装。若 bluetoothctl 报 No default controller,属 Android 内核限制,"
  echo "BLE 链路测试请走 termux-api 扫描 + 手机浏览器 Web Bluetooth(tools/ble-gatt-test)。"
else
  echo "bluez 不可用(Android 无 BlueZ 栈,属预期)。BLE 链路测试请走:"
  echo "  - scripts/termux/ble-scan.sh   (termux-api 扫描验证广播)"
  echo "  - tools/ble-gatt-test 页面      (Chrome Web Bluetooth GATT 收发)"
fi

echo "== 4/4 权限与提示 =="
# 尽力授予 Termux:API 运行时权限(无 root 时 pm 不可用,需手动在系统设置授予)
if command -v pm >/dev/null 2>&1; then
  pm grant com.termux.api android.permission.BLUETOOTH_SCAN       2>/dev/null || true
  pm grant com.termux.api android.permission.BLUETOOTH_CONNECT    2>/dev/null || true
  pm grant com.termux.api android.permission.ACCESS_FINE_LOCATION 2>/dev/null || true
  pm grant com.termux.api android.permission.ACCESS_COARSE_LOCATION 2>/dev/null || true
fi
echo "提示:"
echo "  1. 若未自动授予,请到 系统设置 → 应用 → Termux:API → 权限,授予"
echo "     『附近设备』/『位置』权限(Android 12+ 为 Nearby devices)。"
echo "  2. 扫描 BLE 需:蓝牙已开启、定位开关已开启、屏幕保持亮起。"
echo "  3. 建议 pkg install -y openssh 并运行 sshd,用于 SSH 端口转发访问守护进程 UI。"
echo
echo "下一步:"
echo "  git clone https://github.com/R6AX-ZOE/zoe-chat.git && cd zoe-chat"
echo "  bash scripts/termux/build.sh        # 构建 + 跑 BLE mesh mock 测试"
echo "  bash scripts/termux/ble-scan.sh     # 扫描验证对端广播"
echo "  详细流程见 docs/termux-ble.md"
