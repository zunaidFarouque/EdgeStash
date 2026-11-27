@echo off
setlocal
cd /d "%~dp0"

title EdgeStash Alert Tester

echo ============================================================
echo   EdgeStash - Window Notification Alert Simulator
echo ============================================================
echo.

if not exist "target\release\examples\alert_tester.exe" (
    echo Pre-built binary not found. Compiling release binary with cargo...
    cargo build --example alert_tester --release
    if errorlevel 1 (
        echo.
        echo [ERROR] Failed to compile alert_tester.
        echo Please ensure Rust/cargo is installed and in your PATH.
        echo.
        pause
        exit /b 1
    )
    echo Build succeeded!
    echo.
)

echo Starting Alert Tester...
"target\release\examples\alert_tester.exe"

if errorlevel 1 (
    echo.
    echo [ERROR] alert_tester exited with code %ERRORLEVEL%.
    pause
)

endlocal
