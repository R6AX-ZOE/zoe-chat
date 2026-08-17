# 把 android/(canonical Kotlin 源码)同步进 app/src-tauri/gen/android/。
# 用法: powershell scripts/sync-android-ble.ps1   (Windows 本地)
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$src = Join-Path $root "android\app\src\main\java\com\zoechat\ble"
$dst = Join-Path $root "app\src-tauri\gen\android\app\src\main\java\com\zoechat\ble"

# gen/android 不入库(gitignore),由 npx tauri android init 生成;
# 这里只校验 init 产物存在(ble/ 目录本身由本脚本创建)。
$genMain = Join-Path $root "app\src-tauri\gen\android\app\src\main"
if (-not (Test-Path $genMain)) {
    Write-Error "gen/android 不存在(先跑 npx tauri android init)"
    exit 1
}

New-Item -ItemType Directory -Force -Path $dst | Out-Null
Copy-Item (Join-Path $src "*.kt") $dst -Force
Write-Host "synced: $src -> $dst"
