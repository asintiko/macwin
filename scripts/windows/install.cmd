@echo off
setlocal
rem Runs install.ps1 even where PowerShell scripts are blocked by default.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
set "installExitCode=%ERRORLEVEL%"
pause
exit /b %installExitCode%
