# build-soapysdrplay3-msys2.ps1 - build SoapySDRPlay3.dll from upstream
# source against MSYS2's libSoapySDR + the user-installed SDRplay API.
#
# Why this exists: MSYS2 doesn't ship `mingw-w64-x86_64-soapysdrplay3`.
# The SDRplay API itself is proprietary and license-prohibits us
# redistributing it - but the SoapySDRPlay3 *bridge module* is BSD/MIT
# licensed and easy to build ourselves. This script is the cleanest path
# to bench-test the RSP1A in NRSC5 Studio v0.3.0.
#
# PREREQUISITES
#   1. MSYS2 installed at C:\msys64 with mingw-w64-x86_64-soapysdr
#      already present (run `scripts/install-soapysdr-msys2.ps1` first).
#   2. SDRplay API v3.x installed from https://www.sdrplay.com/api
#      (default install location: C:\Program Files\SDRplay\API\).
#   3. git available in MSYS2 (the script will pacman-install it if not).
#
# OUTPUT
#   bin\SoapySDR\modules0.8\libsdrPlaySupport.dll  (the bridge module)
#   (sdrplay_api.dll is NOT bundled - must come from SDRplay's installer.
#   It's already on PATH after their installer runs.)
#
# RE-RUN SAFELY
#   Idempotent - re-running does a fresh git pull + clean CMake configure,
#   which picks up new SoapySDR releases or SDRplay API upgrades on the
#   host. Pass -Clean to wipe the build directory entirely.
#
# OVERRIDES
#   -SdrplayApiRoot 'X:\custom\path'   non-default SDRplay install
#   -Clean                              wipe build/ and reclone
#   -Ref <git-ref>                      specific upstream tag or commit
#                                       (default: master)

#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$SdrplayApiRoot = 'C:\Program Files\SDRplay\API',
    [string]$Ref = 'master',
    [switch]$Clean
)

$ErrorActionPreference = 'Stop'

# Paths --------------------------------------------------------------------
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$BuildRoot   = Join-Path $ProjectRoot 'build'
$SrcDir      = Join-Path $BuildRoot 'SoapySDRPlay3'
$CMakeBuild  = Join-Path $SrcDir 'build'
$ModulesDir  = Join-Path $ProjectRoot 'bin\SoapySDR\modules0.8'
$UpstreamUrl = 'https://github.com/pothosware/SoapySDRPlay3.git'

# MSYS2 paths --------------------------------------------------------------
$Msys64       = 'C:\msys64'
$MingwPrefix  = Join-Path $Msys64 'mingw64'
$Bash         = Join-Path $Msys64 'usr\bin\bash.exe'

# Force the MINGW64 environment so /etc/profile prepends /mingw64/bin to
# PATH inside every `bash -lc` invocation. Without this, the login shell
# falls back to plain MSYS and CMake can't find gcc/g++/ar/ld even when
# mingw-w64-x86_64-gcc is installed.
$env:MSYSTEM = 'MINGW64'
$env:CHERE_INVOKING = '1'

# === Sanity checks ========================================================

Write-Host "=== Verifying prerequisites ===" -ForegroundColor Cyan

if (-not (Test-Path $Msys64)) {
    throw "MSYS2 not found at $Msys64. Install from https://www.msys2.org/."
}
if (-not (Test-Path $MingwPrefix)) {
    throw "MSYS2 mingw64 prefix missing: $MingwPrefix. Run scripts\install-soapysdr-msys2.ps1 first."
}
if (-not (Test-Path (Join-Path $MingwPrefix 'bin\libSoapySDR.dll'))) {
    throw "libSoapySDR.dll not found in $MingwPrefix\bin. Run scripts\install-soapysdr-msys2.ps1 first."
}

$ApiInc = Join-Path $SdrplayApiRoot 'inc'
$ApiLib = Join-Path $SdrplayApiRoot 'x64'
$ApiHeader = Join-Path $ApiInc 'sdrplay_api.h'
$ApiImportLib = Join-Path $ApiLib 'sdrplay_api.lib'

if (-not (Test-Path $ApiHeader)) {
    throw @"
SDRplay API header not found: $ApiHeader

Install SDRplay API v3.x from https://www.sdrplay.com/api (free, but
requires accepting their license - we can't redistribute it). The
installer typically lands at 'C:\Program Files\SDRplay\API\'. If you
installed elsewhere, pass -SdrplayApiRoot 'X:\custom\path'.
"@
}
if (-not (Test-Path $ApiImportLib)) {
    throw "SDRplay import library missing: $ApiImportLib. Reinstall the SDRplay API."
}

Write-Host "  MSYS2 mingw64        : $MingwPrefix"
Write-Host "  SDRplay API root     : $SdrplayApiRoot"
Write-Host "  SDRplay API header   : $ApiHeader"
Write-Host "  SDRplay import lib   : $ApiImportLib"
Write-Host "  Output modules dir   : $ModulesDir"
Write-Host ""

# === Ensure MSYS2 build tools ============================================

Write-Host "=== Verifying MSYS2 build tools ===" -ForegroundColor Cyan
# cmake + make + git + pkgconf + gcc/g++/binutils - install via pacman if
# any are missing. mingw-w64-x86_64-cmake provides 'cmake' built for
# native mingw64. mingw-w64-x86_64-gcc pulls in g++ + binutils (ar, ld,
# etc) needed by CMake's compiler probe. The MSYS-side `git` package is
# fine for cloning; we don't need mingw git.
& $Bash -lc "pacman -S --needed --noconfirm mingw-w64-x86_64-cmake mingw-w64-x86_64-make mingw-w64-x86_64-gcc mingw-w64-x86_64-pkgconf git 2>&1 | tail -5"
if ($LASTEXITCODE -ne 0) { throw "pacman install of build tools failed (exit $LASTEXITCODE)" }
Write-Host ""

# === Fetch source ========================================================

if ($Clean -and (Test-Path $SrcDir)) {
    Write-Host "=== -Clean: wiping $SrcDir ===" -ForegroundColor Yellow
    Remove-Item -Recurse -Force $SrcDir
}

if (-not (Test-Path $BuildRoot)) { New-Item -ItemType Directory -Path $BuildRoot | Out-Null }

if (-not (Test-Path $SrcDir)) {
    Write-Host "=== Cloning $UpstreamUrl ===" -ForegroundColor Cyan
    & $Bash -lc "cd '$(($BuildRoot -replace '\\','/'))' && git clone --depth 1 --branch '$Ref' '$UpstreamUrl' SoapySDRPlay3"
    if ($LASTEXITCODE -ne 0) { throw "git clone failed (exit $LASTEXITCODE)" }
} else {
    Write-Host "=== Updating $SrcDir to $Ref ===" -ForegroundColor Cyan
    & $Bash -lc "cd '$(($SrcDir -replace '\\','/'))' && git fetch --depth 1 origin '$Ref' && git checkout '$Ref' && git reset --hard FETCH_HEAD"
    if ($LASTEXITCODE -ne 0) { throw "git update failed (exit $LASTEXITCODE)" }
}
Write-Host ""

# === Configure + build ===================================================

# CMake arg notes:
#   -DCMAKE_INSTALL_PREFIX -> we don't actually `cmake --install`; we
#     just pull the .dll out of the build dir. Pointing it at the MSYS2
#     prefix keeps SoapySDR's config-discovery happy.
#   -DLIBSDRPLAY_INCLUDE_DIRS / -DLIBSDRPLAY_LIBRARIES -> SoapySDRPlay3's
#     CMake search uses find_path()/find_library(); pass them explicitly
#     so it can't accidentally pick up a wrong version somewhere on PATH.
#   We build inside the MSYS2 mingw64 login shell so CMAKE_GENERATOR
#   auto-detects "MSYS Makefiles" and the compiler is mingw64 gcc/g++.

# Convert Windows paths to MSYS-style forward-slash paths. CMake on MSYS2
# accepts forward-slash absolute Windows paths just fine.
$ApiIncFwd     = $ApiInc -replace '\\','/'
$ApiLibFwd     = $ApiImportLib -replace '\\','/'
$BuildFwd      = $CMakeBuild -replace '\\','/'
$SrcFwd        = $SrcDir -replace '\\','/'

Write-Host "=== Running CMake configure ===" -ForegroundColor Cyan
# CMake 4.x removed support for cmake_minimum_required() values below 3.5.
# SoapySDRPlay3's top-level CMakeLists.txt still declares an older floor,
# so we explicitly opt in to the legacy policy version. Upstream PR
# pending; this flag is the documented workaround in the CMake 4.0 error
# message itself.
$ConfigureCmd = @"
mkdir -p '$BuildFwd' && cd '$BuildFwd' && /mingw64/bin/cmake \
    -G 'MSYS Makefiles' \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX=/mingw64 \
    -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
    -DLIBSDRPLAY_INCLUDE_DIRS='$ApiIncFwd' \
    -DLIBSDRPLAY_LIBRARIES='$ApiLibFwd' \
    '$SrcFwd' 2>&1
"@
& $Bash -lc $ConfigureCmd
if ($LASTEXITCODE -ne 0) { throw "CMake configure failed (exit $LASTEXITCODE)" }
Write-Host ""

Write-Host "=== Building (mingw32-make -jN) ===" -ForegroundColor Cyan
$Jobs = [Environment]::ProcessorCount
& $Bash -lc "cd '$BuildFwd' && /mingw64/bin/mingw32-make -j$Jobs 2>&1 | tail -20"
if ($LASTEXITCODE -ne 0) { throw "make failed (exit $LASTEXITCODE)" }
Write-Host ""

# === Stage output =========================================================

Write-Host "=== Locating built module DLL ===" -ForegroundColor Cyan
# SoapySDRPlay3's CMakeLists.txt names the output module differently
# across releases - historically `libsdrPlaySupport.dll`, sometimes
# `sdrPlaySupport.dll`. Grab whatever Soapy module-shaped DLL we
# produced; reject if there are zero or multiple matches.
$Built = Get-ChildItem -Path $CMakeBuild -Filter '*sdrPlay*.dll' -Recurse -ErrorAction SilentlyContinue
if ($null -eq $Built -or $Built.Count -eq 0) {
    throw "Build succeeded but no *sdrPlay*.dll found under $CMakeBuild - check build/SoapySDRPlay3/build/ manually."
}
if ($Built.Count -gt 1) {
    Write-Warning "Multiple module DLLs found, choosing first:"
    $Built | ForEach-Object { Write-Warning "    $($_.FullName)" }
}
$ModuleDll = $Built[0]
Write-Host "  Built: $($ModuleDll.FullName) ($([math]::Round($ModuleDll.Length/1KB, 1)) KB)"

if (-not (Test-Path $ModulesDir)) { New-Item -ItemType Directory -Path $ModulesDir -Force | Out-Null }
$DestPath = Join-Path $ModulesDir $ModuleDll.Name
Copy-Item -Path $ModuleDll.FullName -Destination $DestPath -Force
Write-Host "  Staged into: $DestPath" -ForegroundColor Green
Write-Host ""

# === Smoke check =========================================================

Write-Host "=== Smoke check: SoapySDRUtil sees sdrplay factory ===" -ForegroundColor Cyan
# Three env vars matter at runtime:
#   PATH                    - needs $MingwPrefix\bin (libSoapySDR.dll +
#                             libusb), $SdrplayApiRoot\x64
#                             (sdrplay_api.dll), and $ModulesDir's parent
#                             chain. NRSC5 Studio v0.3.0 will set these
#                             programmatically in main.rs (Phase 4.2).
#   SOAPY_SDR_PLUGIN_PATH   - point Soapy at our bundled modules dir so
#                             it loads the bridge module we just built.
$env:PATH = "$SdrplayApiRoot\x64;$MingwPrefix\bin;$env:PATH"
$env:SOAPY_SDR_PLUGIN_PATH = $ModulesDir
$Factories = & "$MingwPrefix\bin\SoapySDRUtil.exe" --info 2>&1 | Select-String -Pattern 'Available factories'
Write-Host "  $Factories"
if ($Factories -notmatch 'sdrplay') {
    Write-Warning "sdrplay factory not in Available factories - module load failed silently. Check SDRplay API install."
} else {
    Write-Host "  sdrplay factory available." -ForegroundColor Green
}
Write-Host ""

Write-Host "Done." -ForegroundColor Green
Write-Host @"
Next steps:
  1. Plug in the RSP1A (the SDRplay API service handles USB enumeration).
  2. SoapySDRUtil --find       # should list driver=sdrplay
  3. cargo run --example soapy_probe -- --driver=sdrplay --freq=97.1
                              # smoke-test IQ capture via the new path.
"@
