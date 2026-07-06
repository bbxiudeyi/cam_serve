@echo off
REM ============================================================
REM  cam-stream launcher (double-click to run)
REM  Runs target\release\cam-stream.exe from the PROJECT ROOT,
REM  because the exe reads ./static via relative paths.
REM ============================================================

REM Switch to this bat's own directory (the project root)
cd /d "%~dp0"

REM Path to the exe (relative to project root)
set "EXE=target\release\cam-stream.exe"

REM Check the exe exists
if not exist "%EXE%" (
    echo [ERROR] cam-stream.exe not found at: %CD%\%EXE%
    echo         Run "cargo build --release" first to compile it.
    echo.
    pause
    exit /b 1
)

REM Run the exe. It runs in the foreground; closing this window stops it.
"%EXE%"

REM cam-stream.exe already pauses on error (its main() waits for Enter),
REM but if it exits for other reasons (e.g. Ctrl+C), keep the window open.
if errorlevel 1 (
    echo.
    pause
)
