@echo off
REM Spike 2 closed-loop AGC runner.
REM
REM Prepends the llvm-mingw runtime bin/ to PATH (libc++.dll, libunwind.dll,
REM libwinpthread-1.dll) and execs target\agc_pipe.exe with whatever args
REM you pass. Examples:
REM
REM   scripts\spike2-agc.bat                              (defaults: 97.1, 90s, target 12)
REM   scripts\spike2-agc.bat --freq 96.3 --initial-gain 20 --max-seconds 60
REM   scripts\spike2-agc.bat --freq 102.9 --mer-target 8 --log target\mix.log
REM
REM See `target\agc_pipe.exe --help` for the full option list.

setlocal
set "ROOT=%~dp0.."
pushd "%ROOT%" >nul
set "PATH=%ROOT%\.toolchains\llvm-mingw-20260505-ucrt-x86_64\bin;%PATH%"
target\agc_pipe.exe %*
set "RC=%ERRORLEVEL%"
popd >nul
exit /b %RC%
