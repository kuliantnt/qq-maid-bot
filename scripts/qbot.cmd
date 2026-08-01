@echo off
rem qq-maid-bot Windows 便捷入口：只负责把参数原样转发给 qbot.ps1，
rem 所有安装/更新/下载逻辑都在 PowerShell 端实现，本文件不复制任何逻辑。
setlocal
if not exist "%~dp0qbot.ps1" (
    echo qbot.ps1 not found next to qbot.cmd: %~dp0qbot.ps1
    exit /b 1
)
rem %* 完整透传参数；引号保证 qbot.ps1 路径含空格时可用；
rem 直接以 PowerShell 的退出码返回，保证失败时调用方拿到相同非零值。
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0qbot.ps1" %*
exit /b %errorlevel%
