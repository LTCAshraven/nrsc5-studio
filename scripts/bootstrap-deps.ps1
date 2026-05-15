$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$SourceRoot = Split-Path -Parent $Root
$SourceBin = Join-Path $SourceRoot "bin"
$DestBin = Join-Path $Root "bin"

if (-not (Test-Path $SourceBin)) {
    throw "Source bin folder not found: $SourceBin"
}

New-Item -ItemType Directory -Path $DestBin -Force | Out-Null
Copy-Item -Path (Join-Path $SourceBin "*") -Destination $DestBin -Recurse -Force

Write-Host "Copied NRSC5 runtime files from $SourceBin to $DestBin"
