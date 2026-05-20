# Install SoapySDR + permissively-licensed device modules from MSYS2 and
# stage them for both link-time (bindgen + cargo) and run-time (bin\).
#
# Why this exists:
# v0.3.0 unifies the SDR backend on libSoapySDR.dll, replacing the direct
# librtlsdr binding. Every supported device (RTL-SDR, SDRplay, Airspy,
# HackRF, Lime, Pluto, network via SoapyRemote) goes through one Rust
# `SoapySdr` impl that links against the C library. This script:
#
#   1. Installs MSYS2 if absent (mirrors build-nrsc5-msys2.ps1).
#   2. Installs SoapySDR + module pacman packages from mingw-w64.
#   3. Copies runtime DLLs into bin\ and bin\SoapySDR\modules0.8\ for
#      the portable distribution.
#   4. Copies the import library and headers into the bundled
#      llvm-mingw toolchain so `cargo build` links cleanly without any
#      manual PATH gymnastics.
#
# Idempotent: re-running re-installs (pacman --needed) and re-copies.
#
# Usage:
#     scripts\install-soapysdr-msys2.ps1
#
# Optional environment overrides:
#     $env:MSYS2_ROOT = "C:\msys64"      # MSYS2 install location
#     $env:NRSC5_LLVM_MINGW = ".toolchains\llvm-mingw-20260505-ucrt-x86_64"

[CmdletBinding()]
param(
    [string]$Msys2Root = ($env:MSYS2_ROOT, "C:\msys64" -ne $null)[0],
    [string]$LlvmMingwRel = ($env:NRSC5_LLVM_MINGW, ".toolchains\llvm-mingw-20260505-ucrt-x86_64" -ne $null)[0]
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$binDir = Join-Path $repoRoot "bin"
$modulesDir = Join-Path $binDir "SoapySDR\modules0.8"
$shell = Join-Path $Msys2Root "msys2_shell.cmd"
$mingw = Join-Path $Msys2Root "mingw64"
$llvmMingw = Join-Path $repoRoot $LlvmMingwRel

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

# --- 1. update package db + install SoapySDR + modules ---
Write-Host "=== Updating MSYS2 package db ===" -ForegroundColor Cyan
Invoke-MSYS2 "pacman -Syu --noconfirm"
Invoke-MSYS2 "pacman -Syu --noconfirm"  # second pass for residuals

# SoapySDRPlay3 is NOT on MSYS2 (SDRplay license forbids redistribution
# of their API service binaries). Users who want SDRplay support need
# to install the SDRplay API from sdrplay.com and the matching
# SoapySDRPlay3.dll from a third-party build. Documented in README.
#
# Airspy / Remote / Pluto / Lime modules are also NOT on MSYS2 mingw64
# (verified 2026-05). For 0.3.0 we ship only what MSYS2 provides
# (rtlsdr + hackrf) plus user-installed SDRplay; the rest can be staged
# manually into bin\SoapySDR\modules0.8\ from the PothosSDR Windows
# installer if desired. Documented in README.
Write-Host "=== Installing SoapySDR + modules ===" -ForegroundColor Cyan
$pkgs = @(
    "mingw-w64-x86_64-soapysdr",
    "mingw-w64-x86_64-soapyrtlsdr",
    "mingw-w64-x86_64-soapyhackrf",
    "mingw-w64-x86_64-libusb"
) -join " "
Invoke-MSYS2 "pacman -S --noconfirm --needed $pkgs"

# --- 2. stage runtime DLLs into bin\ for portable distribution ---
Write-Host "=== Copying runtime DLLs into $binDir ===" -ForegroundColor Cyan

New-Item -ItemType Directory -Path $binDir -Force | Out-Null
New-Item -ItemType Directory -Path $modulesDir -Force | Out-Null

# Core libraries that the app loads at runtime. libwinpthread/libgcc/libstdc++
# are transitive dependencies of the MSYS2 SoapySDR build; missing any of them
# causes a "Failed to load module" error at SoapySDR::enumerate() time.
# MSYS2 names the main SoapySDR library `libSoapySDR.dll` (with the `lib`
# prefix), not `SoapySDR.dll` — matches the Unix convention.
$coreDlls = @(
    "libSoapySDR.dll",
    "libusb-1.0.dll",
    "libwinpthread-1.dll",
    "libgcc_s_seh-1.dll",
    "libstdc++-6.dll"
)
foreach ($dll in $coreDlls) {
    $src = Join-Path $mingw "bin\$dll"
    if (-not (Test-Path $src)) {
        Write-Warning "Missing $src; skipping (some devices may fail to load)"
        continue
    }
    Copy-Item -Path $src -Destination $binDir -Force
}

# Module DLLs are looked up by SoapySDR via SOAPY_SDR_PLUGIN_PATH; we
# point that env var at $modulesDir from src/main.rs in portable mode.
$srcModules = Join-Path $mingw "lib\SoapySDR\modules0.8"
if (Test-Path $srcModules) {
    Get-ChildItem -Path $srcModules -Filter "*.dll" | ForEach-Object {
        Copy-Item -Path $_.FullName -Destination $modulesDir -Force
    }
} else {
    Write-Warning "MSYS2 SoapySDR modules dir not found at $srcModules"
}

# --- 3. stage import lib + headers into llvm-mingw for linking ---
# The bundled llvm-mingw toolchain is what `cargo build` actually uses
# (build.rs prepends its bin\ to PATH). It doesn't know about MSYS2's
# install tree by default, so we copy the linker artifacts into its
# x86_64-w64-mingw32/ sysroot. Equivalent to "pacman -S into llvm-mingw"
# for the one library we need at link time.
if (-not (Test-Path $llvmMingw)) {
    Write-Warning "llvm-mingw not found at $llvmMingw; cargo will need PATH adjustments to link SoapySDR."
} else {
    Write-Host "=== Staging SoapySDR linker artifacts into $llvmMingw ===" -ForegroundColor Cyan
    $sysroot = Join-Path $llvmMingw "x86_64-w64-mingw32"
    $dstLib = Join-Path $sysroot "lib"
    $dstInc = Join-Path $sysroot "include\SoapySDR"
    New-Item -ItemType Directory -Path $dstLib -Force | Out-Null
    New-Item -ItemType Directory -Path $dstInc -Force | Out-Null

    # Import library (libSoapySDR.dll.a is what the linker resolves
    # `-lSoapySDR` against).
    $srcImportLib = Join-Path $mingw "lib\libSoapySDR.dll.a"
    if (Test-Path $srcImportLib) {
        Copy-Item -Path $srcImportLib -Destination $dstLib -Force
    } else {
        Write-Warning "libSoapySDR.dll.a not found; cargo may fail to link."
    }

    # Headers — the soapysdr Rust crate runs bindgen at build time and
    # needs every header under include/SoapySDR/.
    $srcInc = Join-Path $mingw "include\SoapySDR"
    if (Test-Path $srcInc) {
        Get-ChildItem -Path $srcInc -Filter "*.h*" | ForEach-Object {
            Copy-Item -Path $_.FullName -Destination $dstInc -Force
        }
    } else {
        Write-Warning "SoapySDR headers not found at $srcInc; cargo may fail to compile."
    }
}

# --- 4. summary ---
Write-Host ""
Write-Host "Done." -ForegroundColor Green
Write-Host "  Runtime DLLs    : $binDir"
Write-Host "  Soapy modules   : $modulesDir"
Write-Host "  Link artifacts  : $llvmMingw\x86_64-w64-mingw32\{lib,include}"
Write-Host ""
Write-Host "Next: cargo build (should now link libSoapySDR successfully)."
Write-Host "      cargo run --example soapy_probe -- driver=rtlsdr"
Write-Host ""
Write-Host "Note: SDRplay support requires installing the SDRplay API from"
Write-Host "      sdrplay.com (license forbids us bundling it) plus a matching"
Write-Host "      SoapySDRPlay3.dll in $modulesDir."
