param(
    [string]$Configuration = "release"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$TargetDir = Join-Path $Root "target\$Configuration"
$OutDir = Join-Path $Root "dist\nrsc5-studio-portable"

New-Item -ItemType Directory -Path $OutDir -Force | Out-Null

Copy-Item -Path (Join-Path $TargetDir "nrsc5-studio.exe") -Destination $OutDir -Force
if (Test-Path (Join-Path $Root "bin")) {
    Copy-Item -Path (Join-Path $Root "bin\*") -Destination $OutDir -Recurse -Force
}
if (Test-Path (Join-Path $Root "res\config.toml")) {
    Copy-Item -Path (Join-Path $Root "res\config.toml") -Destination $OutDir -Force
}

Write-Host "Portable package created at: $OutDir"
