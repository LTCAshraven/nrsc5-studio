param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [string]$Target = "x86_64-pc-windows-gnullvm",
    [switch]$Zip
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot

# Read version from Cargo.toml so the zip is named consistently
$CargoToml = Get-Content (Join-Path $Root "Cargo.toml") -Raw
$Version = ([regex]::Match($CargoToml, '(?m)^\s*version\s*=\s*"([^"]+)"')).Groups[1].Value
if (-not $Version) { $Version = "0.0.0" }

$TargetDir = Join-Path $Root "target\$Target\$Configuration"
if (-not (Test-Path (Join-Path $TargetDir "nrsc5-studio.exe"))) {
    throw "Binary not found: $TargetDir\nrsc5-studio.exe. Build it first with: .\scripts\cargo-gnu.ps1 -Configuration $Configuration"
}

$OutName = "nrsc5-studio-$Version-windows-x64"
$OutDir  = Join-Path $Root "dist\$OutName"

if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
New-Item -ItemType Directory -Path $OutDir -Force | Out-Null

# Application binary
Copy-Item -Path (Join-Path $TargetDir "nrsc5-studio.exe") -Destination $OutDir -Force

# Native dependencies (nrsc5.exe + DLLs)
if (Test-Path (Join-Path $Root "bin")) {
    Copy-Item -Path (Join-Path $Root "bin\*") -Destination $OutDir -Recurse -Force
}

# Default configuration
if (Test-Path (Join-Path $Root "res\config.toml")) {
    Copy-Item -Path (Join-Path $Root "res\config.toml") -Destination $OutDir -Force
}

# Resources required at runtime. `res\map.png` is the full US base map
# the WeatherMap crops cached basemaps from — without it on disk the
# weather radar pipeline silently no-ops because no basemap can be
# produced. `find_map_file` walks up from the exe dir looking for
# `res/map.png`, so we preserve the `res\` subfolder in the portable
# layout.
$ResOutDir = Join-Path $OutDir "res"
if (-not (Test-Path $ResOutDir)) {
    New-Item -ItemType Directory -Path $ResOutDir -Force | Out-Null
}
if (Test-Path (Join-Path $Root "res\map.png")) {
    Copy-Item -Path (Join-Path $Root "res\map.png") -Destination $ResOutDir -Force
}

# Portable-mode marker. Its presence beside nrsc5-studio.exe makes the
# app write all persistent state (config, art cache, play log, dock
# layout, traffic/weather scratch) into a `data\` folder next to the
# executable instead of into %APPDATA% / %LOCALAPPDATA%. Shipping it by
# default makes the released zip fully self-contained on extract; users
# who prefer the standard "writes to user profile" behavior can delete
# this file.
if (Test-Path (Join-Path $Root "res\portable.txt")) {
    Copy-Item -Path (Join-Path $Root "res\portable.txt") -Destination $OutDir -Force
}

# Licensing / documentation (required by MIT and GPL bundled DLLs)
foreach ($doc in @("README.md", "LICENSE", "THIRD_PARTY_NOTICES.md")) {
    $src = Join-Path $Root $doc
    if (Test-Path $src) {
        Copy-Item -Path $src -Destination $OutDir -Force
    }
}

Write-Host "Portable package created at: $OutDir"

if ($Zip) {
    $ZipPath = Join-Path $Root "dist\$OutName.zip"
    if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
    Compress-Archive -Path (Join-Path $OutDir "*") -DestinationPath $ZipPath -CompressionLevel Optimal
    Write-Host "Zip archive: $ZipPath"
}
