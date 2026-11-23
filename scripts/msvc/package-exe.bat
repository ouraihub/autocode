@echo off
setlocal
rem Local portable package. Keep this file ASCII with CRLF line endings.
call "%~dp0msvc-env.bat"
if errorlevel 1 exit /b 1
set "PACKAGE_POWERSHELL="
for /f "delims=" %%P in ('where pwsh.exe 2^>nul') do if not defined PACKAGE_POWERSHELL set "PACKAGE_POWERSHELL=%%P"
if not defined PACKAGE_POWERSHELL set "PACKAGE_POWERSHELL=%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe"
"%PACKAGE_POWERSHELL%" -NoProfile -ExecutionPolicy Bypass -File "%~dp0package-exe.ps1" %*
exit /b %ERRORLEVEL%
