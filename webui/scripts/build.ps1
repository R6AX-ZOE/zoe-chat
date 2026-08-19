# 构建 zoe-webui 的 wasm 产物到 dist/(与 zoe-daemon 与 Tauri 移动端共用)。
# 前置:rustup target add wasm32-unknown-unknown;wasm-bindgen-cli 版本 = Cargo.lock 中 wasm-bindgen 版本。
# 注意:PS 5.1 下 ErrorActionPreference=Stop 会把原生命令的 stderr 当作错误,故只查 $LASTEXITCODE。
$ErrorActionPreference = "Continue"
Set-Location "$PSScriptRoot\.."

$Target = "wasm32-unknown-unknown"
$Out = "dist"
$Bin = "zoe_webui"

cargo build --release --target $Target
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# 从 Cargo.lock 取 wasm-bindgen 版本(要求 wasm-bindgen-cli 已装同版本)
$lock = Get-Content Cargo.lock -Raw
$m = [regex]::Match($lock, 'name = "wasm-bindgen"\r?\nversion = "([^"]+)"')
if (-not $m.Success) { throw "wasm-bindgen not found in Cargo.lock" }
$wbg = $m.Groups[1].Value
$cliVer = (& wasm-bindgen --version 2>$null) -join ''
if ($cliVer -notmatch [regex]::Escape($wbg)) {
  throw "wasm-bindgen-cli version mismatch (need $wbg): $cliVer"
}

New-Item -ItemType Directory -Force -Path "$Out\assets" | Out-Null
& wasm-bindgen --target web --out-dir "$Out\assets" --no-typescript --out-name $Bin "target\$Target\release\$Bin.wasm"
if ($LASTEXITCODE -ne 0) { throw "wasm-bindgen failed" }
Copy-Item static\index.html, static\styles.css $Out
Write-Output "dist ready"