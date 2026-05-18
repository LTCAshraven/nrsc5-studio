@echo off
REM Diagnostic 1: iq_replay alone, redirect inside cmd.
del /q target\diag1-out.bin 2>nul
del /q target\diag1-err.log 2>nul
echo Running iq_replay with cmd redirect...
target\iq_replay.exe target\synthetic-30s.cu8 > target\diag1-out.bin 2> target\diag1-err.log
echo iq_replay exit: %ERRORLEVEL%
dir target\diag1-out.bin target\diag1-err.log
