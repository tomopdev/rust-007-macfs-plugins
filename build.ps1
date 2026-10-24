# Build pluginů z workspace macfs-plugins a příprava instalačních balíčků do dist\.
#   .\build.ps1                    -> všechny pluginy
#   .\build.ps1 -Plugin wcx        -> jen Total Commander  (dist\hfsplus_wcx.wcx64 + dist\hfsplus_wcx.zip)
#   .\build.ps1 -Plugin spl        -> jen Salamander       (dist\hfsplus.spl)
#   .\build.ps1 -Native            -> optimalizace pro tento CPU (nepřenosné na starší CPU)
param(
    [ValidateSet('all', 'wcx', 'spl')][string]$Plugin = 'all',
    [switch]$Native
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$target = 'x86_64-pc-windows-msvc'

if ($Native) { $env:RUSTFLAGS = '-C target-cpu=native' }

rustup target add $target | Out-Null
cargo test --workspace --release --target $target
if ($LASTEXITCODE -ne 0) { throw 'tests failed' }

$packages = @()
if ($Plugin -in 'all', 'wcx') { $packages += '-p', 'hfsplus-wcx' }
if ($Plugin -in 'all', 'spl') { $packages += '-p', 'hfsplus-spl' }
cargo build --release --target $target @packages
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

$rel  = "target\$target\release"
$dist = Join-Path $PSScriptRoot 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null

if ($Plugin -in 'all', 'wcx') {
    Copy-Item "$rel\hfsplus_wcx.dll" (Join-Path $dist 'hfsplus_wcx.wcx64') -Force
    Copy-Item 'crates\hfsplus-wcx\pluginst.inf' $dist -Force
    # Instalační balíček: Total Commander nabídne instalaci po otevření zipu.
    Compress-Archive -Force -DestinationPath (Join-Path $dist 'hfsplus_wcx.zip') `
        -Path (Join-Path $dist 'hfsplus_wcx.wcx64'), (Join-Path $dist 'pluginst.inf')
    Write-Host "Done: $dist\hfsplus_wcx.wcx64"
}
if ($Plugin -in 'all', 'spl') {
    Copy-Item "$rel\hfsplus_spl.dll" (Join-Path $dist 'hfsplus.spl') -Force
    Write-Host "Done: $dist\hfsplus.spl"
    Write-Host 'Zkopíruj ho do <Salamander>\plugins\hfsplus\ a přidej v Plugins > Plugins Manager.'
}
