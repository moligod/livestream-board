@echo off
chcp 65001 >nul
cd /d "%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
where cargo >nul 2>nul
if errorlevel 1 (
  echo [Error] Rust Cargo was not found. Install Rust or restart Windows after installation.
  pause
  exit /b 1
)
echo Starting TokenUsage web client...
call start-web.cmd
