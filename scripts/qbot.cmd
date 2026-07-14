@echo off
setlocal
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0qbot.ps1" %*
exit /b %errorlevel%
