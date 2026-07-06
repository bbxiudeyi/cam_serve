@echo off
REM ============================================================
REM  Demo web server launcher (double-click to run)
REM  Runs serve.py with Python, hosts the camera demo page.
REM  Uses python.exe directly, ignoring .py file association.
REM ============================================================

REM Switch to this bat's own directory (CWD may differ when double-clicked)
cd /d "%~dp0"

REM Prefer python from PATH; show clear message if not found
where python >nul 2>nul
if errorlevel 1 (
    echo [ERROR] python not found in PATH.
    echo         Please install Python and add it to PATH.
    echo.
    pause
    exit /b 1
)

REM Run serve.py (-u disables output buffering for real-time logs)
python -u serve.py %*

REM Pause on error so user can read the message
if errorlevel 1 (
    echo.
    pause
)
