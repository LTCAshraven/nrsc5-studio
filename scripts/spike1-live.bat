@echo off
REM ============================================================================
REM  spike1-live.bat
REM ----------------------------------------------------------------------------
REM  Spike 1 acceptance runner: live RTL-SDR -> Rust producer -> nrsc5 pipe.
REM
REM  Captures 30 seconds of cu8 IQ from device 0 at 97.1 MHz (KEGL "97.1 The
REM  Eagle", Dallas), gain 15 dB, and decodes it with the bundled nrsc5 v3.1.0
REM  reading from stdin.
REM
REM  The PATH prepend is required so iq_capture.exe can find the llvm-mingw
REM  C++ runtime DLLs (libc++.dll, libunwind.dll, libwinpthread-1.dll). The
REM  binary was linked against them by rustc; without them you get
REM  STATUS_DLL_NOT_FOUND (0xC0000135) before main() ever runs.
REM
REM  Acceptance criteria (all met in 5/17/2026 run):
REM    - nrsc5 emits 'Synchronized' within a few seconds
REM    - MER around 10 dB
REM    - BER < 0.001
REM    - Title/Artist/Slogan updates flowing
REM
REM  Output:
REM    target\spike1-iqcap.log  - iq_capture stderr (config + cancel result)
REM    target\spike1-nrsc5.log  - nrsc5 stderr (sync, MER, BER, title, artist, LOT)
REM ============================================================================

setlocal
set "ROOT=%~dp0.."
pushd "%ROOT%" >nul

set "PATH=%ROOT%\.toolchains\llvm-mingw-20260505-ucrt-x86_64\bin;%PATH%"

if not exist "target\iq_capture.exe" (
    echo [spike1-live] ERROR: target\iq_capture.exe is missing.
    echo                       Build it first with:
    echo                         rustc -O scripts\iq_capture.rs -o target\iq_capture.exe
    popd >nul
    exit /b 1
)
if not exist "bin\nrsc5.exe" (
    echo [spike1-live] ERROR: bin\nrsc5.exe is missing.
    popd >nul
    exit /b 1
)
if not exist "bin\librtlsdr.dll" (
    echo [spike1-live] ERROR: bin\librtlsdr.dll is missing.
    popd >nul
    exit /b 1
)

if exist "target\spike1-iqcap.log" del "target\spike1-iqcap.log"
if exist "target\spike1-nrsc5.log" del "target\spike1-nrsc5.log"

echo [spike1-live] Streaming 90 MB (~30 s) of live cu8 from RTL-SDR at 97.1 MHz...
echo [spike1-live] Press Ctrl-C to stop early.
echo.

target\iq_capture.exe --freq 97.1 --gain 15 --bytes 90000000 2> target\spike1-iqcap.log | bin\nrsc5.exe -l 1 -r - 0 2> target\spike1-nrsc5.log

echo.
echo [spike1-live] --- iq_capture stderr ---
type target\spike1-iqcap.log
echo.
echo [spike1-live] --- nrsc5 stderr (first 25 lines) ---
powershell -NoProfile -Command "Get-Content target\spike1-nrsc5.log -TotalCount 25"
echo.
echo [spike1-live] --- nrsc5 stderr (last 10 lines) ---
powershell -NoProfile -Command "Get-Content target\spike1-nrsc5.log -Tail 10"
echo.
for %%I in (target\spike1-nrsc5.log) do echo [spike1-live] nrsc5 log size: %%~zI bytes

popd >nul
endlocal
