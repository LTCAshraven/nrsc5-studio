@echo off
REM Diagnostic 3: what does nrsc5 think -r - means?
echo --- nrsc5 --help ---
bin\nrsc5.exe --help 2>&1
echo.
echo --- try nrsc5 -r - with no input (will hang or error) ---
echo. | bin\nrsc5.exe -r - 0 2>&1
echo nrsc5 (empty input) exit: %ERRORLEVEL%
