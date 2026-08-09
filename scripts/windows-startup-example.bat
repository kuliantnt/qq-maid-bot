@echo off
setlocal

rem 如果把本文件放在发布包根目录，默认使用脚本所在目录作为运行目录。
rem 如果把本文件复制到 Windows 启动文件夹，请把下一行改成真实发布包目录。
set "QQ_MAID_RUNTIME_DIR=%~dp0"

if not exist "%QQ_MAID_RUNTIME_DIR%botctl.cmd" (
  echo botctl.cmd not found: "%QQ_MAID_RUNTIME_DIR%botctl.cmd"
  pause
  exit /b 1
)

call "%QQ_MAID_RUNTIME_DIR%botctl.cmd" start
if errorlevel 1 (
  pause
  exit /b 1
)

echo 程序控制台链接：http://127.0.0.1:8787/console/
pause
