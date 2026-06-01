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

# Native dependencies. Layout:
#
#   <exe_dir>\nrsc5-studio.exe
#   <exe_dir>\libSoapySDR.dll        -- load-time import of the exe
#   <exe_dir>\libunwind.dll          -- load-time import of the exe
#   <exe_dir>\libgcc_s_seh-1.dll     -- transitive load-time dep of libSoapySDR
#   <exe_dir>\libwinpthread-1.dll    -- transitive load-time dep of libSoapySDR
#   <exe_dir>\libstdc++-6.dll        -- transitive load-time dep of libSoapySDR
#   <exe_dir>\bin\libnrsc5.dll       -- in-process HD Radio decoder (loaded by us)
#   <exe_dir>\bin\librtlsdr.dll      -- loaded by SoapyRTLSDR (PATH at runtime)
#   <exe_dir>\bin\libusb-1.0.dll     -- transitive dep of librtlsdr
#   <exe_dir>\bin\libao-4.dll        -- transitive dep of libnrsc5
#   <exe_dir>\bin\libgcc_s_dw2-1.dll -- transitive dep of libnrsc5
#   <exe_dir>\bin\SoapySDR\modules0.8\*.dll
#
# Why the split: Windows resolves the exe's load-time imports BEFORE
# any of our Rust code runs, so any DLL the exe links statically
# (libSoapySDR.dll + the MSYS2 C/C++ runtime libs it pulls in) must
# sit in a directory Windows' default DLL search covers -- i.e. next
# to the exe, since we don't want to require System32 installs. The
# remaining DLLs (`bin\...`) are loaded by libSoapySDR's modules or
# by `libnrsc5.dll` AFTER `main.rs::install_bundled_dll_paths()` has
# prepended `<exe>\bin` to PATH and pointed `SOAPY_SDR_PLUGIN_PATH`
# at `<exe>\bin\SoapySDR\modules0.8`.
$LoadTimeDlls = @(
    "libSoapySDR.dll",
    "libunwind.dll",
    "libgcc_s_seh-1.dll",
    "libwinpthread-1.dll",
    "libstdc++-6.dll"
)
$BinSrc = Join-Path $Root "bin"
$BinDst = Join-Path $OutDir "bin"
if (Test-Path $BinSrc) {
    # 1) Mirror the whole bin\ tree into <exe_dir>\bin\ (preserves
    #    the `SoapySDR\modules0.8\` subfolder structure that
    #    `paths::bundled_soapy_modules_dir()` looks for).
    Copy-Item -Path $BinSrc -Destination $OutDir -Recurse -Force
    # 2) Promote the load-time DLLs up to the exe root. Each is
    #    *also* left in `bin\` as a copy so a flat `bin\` lookup
    #    from any future helper still finds them.
    foreach ($dll in $LoadTimeDlls) {
        $src = Join-Path $BinSrc $dll
        if (Test-Path $src) {
            Copy-Item -Path $src -Destination $OutDir -Force
        } else {
            Write-Warning "Expected load-time DLL '$dll' not found in bin\ -- exe may fail to start on a clean machine."
        }
    }
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

# SDR support summary. Confirms every module DLL we expect for the
# v0.3.0 release is present in the staged bundle and reminds the
# packager about the runtime dependency the SDRplay module has on
# Xperi/SDRplay's proprietary sdrplay_api.dll (which cannot be
# redistributed — end users must install the SDRplay API themselves
# from sdrplay.com).
$ModulesDir = Join-Path $OutDir "bin\SoapySDR\modules0.8"
$ExpectedModules = @(
    @{ File = "librtlsdrSupport.dll";  Display = "RTL-SDR"  },
    @{ File = "libHackRFSupport.dll";  Display = "HackRF"   },
    @{ File = "libsdrPlaySupport.dll"; Display = "SDRplay"  }
)
Write-Host "Bundled SoapySDR modules:"
foreach ($m in $ExpectedModules) {
    $present = Test-Path (Join-Path $ModulesDir $m.File)
    $marker = if ($present) { "[OK]   " } else { "[MISS] " }
    Write-Host "  $marker$($m.Display) ($($m.File))"
}
Write-Host ""
Write-Host "NOTE: libsdrPlaySupport.dll requires SDRplay API v3.x at runtime."
Write-Host "      Users without an SDRplay receiver can ignore. Users WITH"
Write-Host "      one must install the SDRplay API service from sdrplay.com"
Write-Host "      (free; can't be bundled per Xperi/SDRplay licensing)."

if ($Zip) {
    $ZipPath = Join-Path $Root "dist\$OutName.zip"
    if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
    Compress-Archive -Path (Join-Path $OutDir "*") -DestinationPath $ZipPath -CompressionLevel Optimal
    Write-Host "Zip archive: $ZipPath"
}
