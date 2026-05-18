@echo off
REM Diagnostic 2: type cu8 file directly into nrsc5 -r -.
REM `type` is the cmd equivalent of cat; bytes flow through the pipe untouched.
REM nrsc5 should sync and emit stderr like Test B did.
del /q target\diag2.log 2>nul
echo Piping via type...
type target\spike0-iq.cu8 | bin\nrsc5.exe -r - 0 2> target\diag2.log
echo nrsc5 exit: %ERRORLEVEL%
dir target\diag2.log
