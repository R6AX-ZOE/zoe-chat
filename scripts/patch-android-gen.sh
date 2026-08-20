#!/usr/bin/env bash
# CI:init 之后、build 之前,把 android/gen-patches/ 的 MainActivity.kt 与 AndroidManifest.xml
# cp 覆盖到 gen/android(M1 起生效),并给 release buildType 配置 CI 签名(2026-08-20)。
#
# 背景:gen/android 不入库(gitignore),由 CI 每次 `npx tauri android init` 生成(坑 4),
# 所以 MainActivity/Manifest 的修改不能直接改文件,只能走本 patch 脚本在 CI 覆盖。
# patch 内容以 tauri-cli 官方模板为底(MainActivity 加权限申请 + Bridge.start;
# Manifest 加 BLE 权限/uses-feature),与模板同构 —— 见 android/gen-patches/ 两文件。
#
# 签名:tauri 模板 release buildType 不设 signingConfig → release APK 未签名,侧载报
# "未找到签名/packageInfo is null"。仓库提交了 android/zoe-ci-release.keystore
# (PKCS12,密码 zoe-release-ci,别名 zoe)——CI 专用开发签名,非生产密钥
# (与 Android SDK 自带 debug keystore 同等级;发布正式版时由发布者另配自有 keystore)。
# 固定 keystore 提交入库保证各 CI run 签名一致(可覆盖安装,免卸载)。
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

for f in "$MAIN_SRC" "$MANIFEST_SRC" "android/zoe-ci-release.keystore"; do
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

# --- release 签名注入 ---
GEN_APP="app/src-tauri/gen/android/app"
BUILD_GRADLE="$GEN_APP/build.gradle.kts"
cp -v "android/zoe-ci-release.keystore" "$GEN_APP/zoe-ci-release.keystore"

cat > "$GEN_APP/release-signing.gradle.kts" <<'EOF'
// CI 开发签名(android/zoe-ci-release.keystore,见 patch-android-gen.sh 头注)。
// 仅开发/侧载;发布正式版由发布者配置自有 keystore 替换本文件。
android {
  signingConfigs {
    create("release") {
      storeFile = file("zoe-ci-release.keystore")
      storeType = "PKCS12"
      storePassword = "zoe-release-ci"
      keyAlias = "zoe"
      keyPassword = "zoe-release-ci"
    }
  }
  buildTypes {
    getByName("release") {
      signingConfig = signingConfigs.getByName("release")
    }
  }
}
EOF

if ! grep -q 'release-signing.gradle.kts' "$BUILD_GRADLE"; then
  cat >> "$BUILD_GRADLE" <<'EOF'

// (patch-android-gen.sh 注入)release 签名
apply(from = "release-signing.gradle.kts")
EOF
fi
grep -q 'release-signing.gradle.kts' "$BUILD_GRADLE"
echo "patched: MainActivity.kt / AndroidManifest.xml / release signing"
