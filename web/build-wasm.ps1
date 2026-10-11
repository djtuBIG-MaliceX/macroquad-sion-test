# Builds the browser target and assembles web/ for static serving.
#   usage: pwsh -File web\build-wasm.ps1 [-NoBuild]
#
# macroquad's web backend only works on wasm32-unknown-unknown: the binary is
# a plain module whose `env` imports are satisfied by miniquad's gl.js plus
# the macroquad_audio plugin embedded in web/index.html (audio_* WebAudio host
# calls; gl.js itself ships none). The JS MUST come from the same miniquad
# version as Cargo.lock pins (older bundles lack e.g. renderbuffer imports).
param([switch]$NoBuild)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

if (-not $NoBuild) {
    rustup target list --installed | Select-String -Quiet 'wasm32-unknown-unknown' |
        ForEach-Object { if (-not $_) { rustup target add wasm32-unknown-unknown } }

    Push-Location $root
    try { cargo build --release --target wasm32-unknown-unknown } finally { Pop-Location }
}

$release = Join-Path $root 'target/wasm32-unknown-unknown/release'
foreach ($f in @("title_menu.wasm")) {
    if (-not (Test-Path -LiteralPath (Join-Path $release $f))) { throw "missing artifact: $f (run without -NoBuild)" }
}

# JS bundle must match the miniquad version in Cargo.lock, byte-for-byte API.
$lock = Get-Content (Join-Path $root 'Cargo.lock') -Raw
$mqVer = [regex]::Match($lock, '(?m)^name = "miniquad"\r?\nversion = "([^"]+)"').Groups[1].Value
if (-not $mqVer) { throw 'could not locate miniquad version in Cargo.lock' }
$gljs = Get-ChildItem (Join-Path $env:USERPROFILE ".cargo/registry/src") -Directory |
    ForEach-Object { Get-ChildItem $_.FullName -Directory -Filter "miniquad-$mqVer" } |
    ForEach-Object { Join-Path $_.FullName 'js/gl.js' } |
    Where-Object { Test-Path -LiteralPath $_ } |
    Select-Object -First 1
if (-not $gljs) { throw "miniquad-$mqVer js/gl.js not found in the cargo registry" }

New-Item -ItemType Directory -Force (Join-Path $PSScriptRoot 'js') | Out-Null
New-Item -ItemType Directory -Force (Join-Path $PSScriptRoot 'assets/fonts') | Out-Null
New-Item -ItemType Directory -Force (Join-Path $PSScriptRoot 'assets/audio') | Out-Null
New-Item -ItemType Directory -Force (Join-Path $PSScriptRoot 'assets/sfx') | Out-Null
New-Item -ItemType Directory -Force (Join-Path $PSScriptRoot 'assets/i18n') | Out-Null
New-Item -ItemType Directory -Force (Join-Path $PSScriptRoot 'assets/scenario') | Out-Null

Copy-Item (Join-Path $release 'title_menu.wasm') (Join-Path $PSScriptRoot 'title_menu.wasm') -Force
Copy-Item $gljs (Join-Path $PSScriptRoot 'js/mq_js_bundle.js') -Force
Copy-Item (Join-Path $root 'assets/fonts/NotoSansJP-VF.ttf') (Join-Path $PSScriptRoot 'assets/fonts/') -Force
Copy-Item (Join-Path $root 'assets/audio/*')                  (Join-Path $PSScriptRoot 'assets/audio/') -Force
Copy-Item (Join-Path $root 'assets/sfx/*')                  (Join-Path $PSScriptRoot 'assets/sfx/')   -Force
Copy-Item (Join-Path $root 'assets/i18n/*.loc')             (Join-Path $PSScriptRoot 'assets/i18n/')  -Force
Copy-Item (Join-Path $root 'assets/scenario/*.yaml')        (Join-Path $PSScriptRoot 'assets/scenario/') -Force
if (Test-Path (Join-Path $root 'assets/voice')) {
    New-Item -ItemType Directory -Force (Join-Path $PSScriptRoot 'assets/voice') | Out-Null
    Copy-Item (Join-Path $root 'assets/voice/*')              (Join-Path $PSScriptRoot 'assets/voice/') -Force
}
if (Test-Path (Join-Path $root 'assets/images')) {
    New-Item -ItemType Directory -Force (Join-Path $PSScriptRoot 'assets/images') | Out-Null
    Copy-Item (Join-Path $root 'assets/images/*')             (Join-Path $PSScriptRoot 'assets/images/') -Force
}

Write-Host ''
Write-Host "web/ staged (gl.js from miniquad $mqVer). Serve with:  python -m http.server 8000 -d web"
Write-Host 'scenario YAML, all MML (title+options themes), the MIDI demo and images/ are staged too.'
Write-Host 'then open  http://localhost:8000/  and click once to unlock audio.'
