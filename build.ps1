# Build pluginů z workspace macfs-plugins a příprava instalačních balíčků do dist\.
#   .\build.ps1                    -> všechny pluginy
#   .\build.ps1 -Plugin wcx        -> jen Total Commander  (dist\macfs_wcx.wcx64 + dist\macfs_wcx.zip)
#   .\build.ps1 -Plugin spl        -> jen Salamander       (dist\macfs.spl)
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
# vendor\hfs-reader není členem workspace (GPL, lokální patche), jeho testy se pouští zvlášť
cargo test --release --target $target --manifest-path vendor\hfs-reader\Cargo.toml --features testutil
if ($LASTEXITCODE -ne 0) { throw 'hfs-reader tests failed' }

$packages = @()
if ($Plugin -in 'all', 'wcx') { $packages += '-p', 'macfs-wcx' }
if ($Plugin -in 'all', 'spl') { $packages += '-p', 'macfs-spl' }
cargo build --release --target $target @packages
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

$rel  = "target\$target\release"
$dist = Join-Path $PSScriptRoot 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null

if ($Plugin -in 'all', 'wcx') {
    Copy-Item "$rel\macfs_wcx.dll" (Join-Path $dist 'macfs_wcx.wcx64') -Force
    Copy-Item 'crates\macfs-wcx\pluginst.inf' $dist -Force
    # Instalační balíček: Total Commander nabídne instalaci po otevření zipu.
    Compress-Archive -Force -DestinationPath (Join-Path $dist 'macfs_wcx.zip') `
        -Path (Join-Path $dist 'macfs_wcx.wcx64'), (Join-Path $dist 'pluginst.inf')
    Write-Host "Done: $dist\macfs_wcx.wcx64"
}
if ($Plugin -in 'all', 'spl') {
    Copy-Item "$rel\macfs_spl.dll" (Join-Path $dist 'macfs.spl') -Force
    Write-Host "Done: $dist\macfs.spl"
    Write-Host 'Zkopíruj ho do <Salamander>\plugins\macfs\ a přidej v Plugins > Plugins Manager.'
}
