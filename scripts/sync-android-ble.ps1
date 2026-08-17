# 把 android/(canonical Kotlin 源码)同步进 app/src-tauri/gen/android/。
# 用法: powershell scripts/sync-android-ble.ps1   (Windows 本地)
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$src = Join-Path $root "android\app\src\main\java\com\zoechat\ble"
$dst = Join-Path $root "app\src-tauri\gen\android\app\src\main\java\com\zoechat\ble"

if (-not (Test-Path $dst)) {
    Write-Error "$dst 不存在(先完成 M0:npx tauri android init 并提交 gen/android)"
    exit 1
}

New-Item -ItemType Directory -Force -Path $dst | Out-Null
Copy-Item (Join-Path $src "*.kt") $dst -Force
Write-Host "synced: $src -> $dst"
