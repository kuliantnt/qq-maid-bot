@echo off
setlocal
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0botctl.ps1" %*
exit /b %errorlevel%
