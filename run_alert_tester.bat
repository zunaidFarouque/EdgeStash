@echo off
setlocal
cd /d "%~dp0"

echo ============================================================
echo   EdgeStash - Window Notification Alert Simulator
echo ============================================================
echo.

if exist "target\release\examples\alert_tester.exe" (
    echo Launching alert_tester.exe...
    start "" "target\release\examples\alert_tester.exe"
    goto :done
)

echo Pre-built binary not found. Building with cargo...
cargo build --example alert_tester --release
if %ERRORLEVEL% equ 0 (
    echo Build succeeded! Launching alert_tester.exe...
    start "" "target\release\examples\alert_tester.exe"
) else (
    echo [ERROR] Failed to compile alert_tester. Please verify Rust is installed.
    pause
)

:done
endlocal
