@echo off
setlocal

rem 如果把本文件放在发布包根目录，默认使用脚本所在目录作为运行目录。
rem 如果把本文件复制到 Windows 启动文件夹，请把下一行改成真实发布包目录。
set "QQ_MAID_RUNTIME_DIR=%~dp0"

set "BOT_BINARY=%QQ_MAID_RUNTIME_DIR%qq-maid-bot.exe"
set "BOT_ENV_FILE=%QQ_MAID_RUNTIME_DIR%config\.env"

if not exist "%BOT_BINARY%" (
  echo qq-maid-bot.exe not found: "%BOT_BINARY%"
  pause
  exit /b 1
)

if not exist "%BOT_ENV_FILE%" (
  echo env file not found: "%BOT_ENV_FILE%"
  echo Copy config\.env.example to config\.env and edit it first.
  pause
  exit /b 1
)

cd /d "%QQ_MAID_RUNTIME_DIR%"
"%BOT_BINARY%"
