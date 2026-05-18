# Build nrsc5 v3.1.0 for Windows via MSYS2 / MinGW-w64.
#
# Produces statically-linked nrsc5.exe + libnrsc5.dll at
#   C:\msys64\home\<user>\nrsc5-v3.1.0\build\src\
# which depend only on Windows system DLLs (KERNEL32, msvcrt, WINMM, WS2_32).
# Copy them into bin/ to upgrade the toolchain.
#
# Why this exists:
# Upstream theori-io/nrsc5 doesn't ship Windows binaries — only source. The
# binaries this repo originally shipped were hand-provisioned at some
# untraceable point pre-v3.0 (May 2025) and were missing the
# `_setmode(_fileno(stdin), _O_BINARY)` fix from PR #393, which made
# `nrsc5.exe -r -` unusable on Windows for binary cu8 streams. See
# /memories/session/spike0-findings.md for the full diagnosis.
#
# This script is idempotent — re-running re-clones to the same tag and
# rebuilds. It assumes you can install software (MSYS2 install requires
# admin elevation on first run).
#
# Usage:
#     scripts\build-nrsc5-msys2.ps1
#
# Optional environment overrides:
#     $env:NRSC5_TAG = "v3.1.0"       # which upstream tag to build
#     $env:NRSC5_JOBS = "8"            # make -j N
#     $env:MSYS2_ROOT = "C:\msys64"    # MSYS2 install location

[CmdletBinding()]
param(
    [string]$Tag = ($env:NRSC5_TAG, "v3.1.0" -ne $null)[0],
    [int]$Jobs = [int]($env:NRSC5_JOBS, "0" -ne $null)[0],
    [string]$Msys2Root = ($env:MSYS2_ROOT, "C:\msys64" -ne $null)[0]
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$binDir = Join-Path $repoRoot "bin"
$shell = Join-Path $Msys2Root "msys2_shell.cmd"

# --- 0. ensure MSYS2 exists ---
if (-not (Test-Path $shell)) {
    Write-Host "MSYS2 not found at $Msys2Root; installing via winget..." -ForegroundColor Cyan
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if (-not $winget) {
        throw "winget is not available; install MSYS2 manually from https://msys2.org/ and re-run."
    }
    winget install --id MSYS2.MSYS2 --silent --accept-package-agreements --accept-source-agreements
    if (-not (Test-Path $shell)) {
        throw "MSYS2 install did not land at $Msys2Root; set `$env:MSYS2_ROOT and retry."
    }
}

function Invoke-MSYS2 {
    param([string]$Command)
    & $shell -mingw64 -defterm -no-start -here -c $Command
    if ($LASTEXITCODE -ne 0) {
        throw "MSYS2 command failed (exit $LASTEXITCODE): $Command"
    }
}

# --- 1. update package db + install build deps ---
Write-Host "=== Updating MSYS2 package db ===" -ForegroundColor Cyan
Invoke-MSYS2 "pacman -Syu --noconfirm"
Invoke-MSYS2 "pacman -Syu --noconfirm"  # second pass for residuals

Write-Host "=== Installing build deps ===" -ForegroundColor Cyan
$pkgs = @(
    "autoconf", "automake", "git", "gzip", "make", "patch", "tar", "xz",
    "mingw-w64-x86_64-gcc",
    "mingw-w64-x86_64-cmake",
    "mingw-w64-x86_64-libtool",
    "mingw-w64-x86_64-pkgconf"
) -join " "
Invoke-MSYS2 "pacman -S --noconfirm --needed $pkgs"

# --- 2. clone + build ---
$buildScript = @"
set -e
set -o pipefail
cd ~
SRC_DIR=nrsc5-$Tag
rm -rf "`$SRC_DIR"
git clone --depth 1 --branch $Tag https://github.com/theori-io/nrsc5.git "`$SRC_DIR"
cd "`$SRC_DIR"
echo "=== ref ==="
git log -1 --format='%H %s'
mkdir -p build
cd build
cmake -G "MSYS Makefiles" \
    -D USE_STATIC=ON \
    -D USE_SYSTEM_LIBUSB=OFF \
    -D USE_SYSTEM_RTLSDR=OFF \
    -D USE_SYSTEM_LIBAO=OFF \
    -D USE_SYSTEM_FFTW=OFF \
    -D USE_SSE=ON \
    -D CMAKE_INSTALL_PREFIX="`$MINGW_PREFIX" \
    ..
"@

if ($Jobs -gt 0) {
    $buildScript += "`nmake -j$Jobs`n"
} else {
    $buildScript += "`nmake -j`$(nproc)`n"
}

$buildScript += "find . -name 'nrsc5.exe' -o -name 'libnrsc5*.dll'`n"

# Write to a temp file so we don't fight quoting through PowerShell + cmd + bash.
$tmpScript = Join-Path $Msys2Root "home\$env:USERNAME\build-nrsc5.sh"
Set-Content -Path $tmpScript -Value $buildScript -Encoding UTF8 -NoNewline

Write-Host "=== Building nrsc5 $Tag (this takes ~10 min) ===" -ForegroundColor Cyan
Invoke-MSYS2 "bash ~/build-nrsc5.sh"

# --- 3. harvest binaries ---
$builtExe = Join-Path $Msys2Root "home\$env:USERNAME\nrsc5-$Tag\build\src\nrsc5.exe"
$builtDll = Join-Path $Msys2Root "home\$env:USERNAME\nrsc5-$Tag\build\src\libnrsc5.dll"
if (-not (Test-Path $builtExe)) { throw "Build did not produce $builtExe" }
if (-not (Test-Path $builtDll)) { throw "Build did not produce $builtDll" }

# Always back up the existing bin\ before overwriting.
if (Test-Path $binDir) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $backup = Join-Path $repoRoot "bin.backup-$stamp"
    Copy-Item $binDir $backup -Recurse
    Write-Host "Backed up existing bin\ to $backup" -ForegroundColor Yellow
} else {
    New-Item -ItemType Directory -Path $binDir | Out-Null
}

Copy-Item $builtExe (Join-Path $binDir "nrsc5.exe") -Force
Copy-Item $builtDll (Join-Path $binDir "libnrsc5.dll") -Force

Write-Host "=== Done. New binaries in $binDir ===" -ForegroundColor Green
Get-Item (Join-Path $binDir "nrsc5.exe"), (Join-Path $binDir "libnrsc5.dll") |
    Format-Table Name, Length, LastWriteTime -AutoSize
Write-Host ""
Write-Host "Verify with:"
Write-Host "    bin\nrsc5.exe -v"
