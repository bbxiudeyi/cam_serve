@echo off
REM ============================================================
REM  cam-stream launcher (double-click to run)
REM  Launches cam-stream.exe DETACHED, so this cmd window closes
REM  immediately. The exe is a tray app (GUI subsystem,
REM  windows_subsystem="windows"), so no console stays open.
REM ============================================================

REM Switch to this bat's own directory (the project root)
cd /d "%~dp0"

REM Resolve absolute path to the exe
set "EXE=%~dp0target\release\cam-stream.exe"

REM Check the exe exists
if not exist "%EXE%" (
    echo [ERROR] cam-stream.exe not found at: %EXE%
    echo         Run "cargo build --release" first to compile it.
    echo.
    pause
    exit /b 1
)

REM Use start with absolute path + explicit working directory.
REM /D "%~dp0" sets the child's working directory to the project root,
REM matching what double-clicking the bat itself does.
REM The first "" is start's window-title placeholder (required when
REM the path is quoted, otherwise start treats the path as the title).
start "" /D "%~dp0" "%EXE%"