@echo off
REM ============================================================
REM  cam-stream launcher (double-click to run)
REM  Launches target\release\cam-stream.exe DETACHED, so this cmd
REM  window closes immediately. The exe is a tray app (GUI
REM  subsystem, windows_subsystem="windows"), so no console
REM  stays open — only the tray icon remains.
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

REM start "" 启动后本 bat 立即结束,这个 cmd 窗口随之关闭,只剩托盘图标。
REM 第一个 "" 是 start 的「窗口标题」占位,不能省(否则 start 会把 exe 路径当标题)。
REM 注:exe 是 GUI 子系统,自己不弹控制台;之前看到的黑窗其实是这个 .bat 的。
start "" "%EXE%"
