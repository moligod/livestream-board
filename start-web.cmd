@echo off
chcp 65001 >nul
cd /d "%~dp0"
echo Building TokenUsage web client...
call npm run build
if errorlevel 1 (
  echo [Error] Web build failed.
  pause
  exit /b 1
)
echo Building local usage collector...
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cargo build --release --quiet --manifest-path src-tauri\Cargo.toml --bin dashboard-json
if errorlevel 1 (
  echo [Error] Local collector build failed.
  pause
  exit /b 1
)
echo Starting TokenUsage web client...
python -B -m tokenusage.web_server
