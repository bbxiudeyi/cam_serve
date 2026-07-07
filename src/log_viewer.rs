//! 「查看实时日志」:启动一个外部终端窗口,tail 最新日志文件。
//!
//! 思路(调研结论的方案 B):不自己 AllocConsole,而是 spawn 一个独立进程
//! 读 `logs/` 下最新的日志文件。这样日志窗口是独立进程,关掉不影响主程序,
//! 鲁棒性最好,也几乎不用 unsafe。
//!
//! 终端选择:优先 Windows Terminal(wt.exe),没有则回退 PowerShell。
//! 两者都跑 `Get-Content -Wait -Tail 200 <file>`,等价 `tail -f`。

use std::path::PathBuf;
use std::process::Command;

/// 找 exe 同级 logs/ 目录下最新的日志文件。
/// tracing_appender::rolling::daily 生成的文件名形如 `cam-stream.log.2026-07-07`,
/// 我们按修改时间取最新的一个。
fn latest_log_file() -> Option<PathBuf> {
    let log_dir = crate::exe_dir().join("logs");
    let entries = std::fs::read_dir(&log_dir).ok()?;
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        // 只看 cam-stream.log.* 这类日志文件
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("cam-stream.log") {
            continue;
        }
        let mtime = entry.metadata().and_then(|m| m.modified()).ok();
        if let Some(t) = mtime {
            if latest.as_ref().map_or(true, |(prev, _)| t > *prev) {
                latest = Some((t, entry.path()));
            }
        }
    }
    latest.map(|(_, p)| p)
}

/// 打开一个终端窗口 tail 实时日志。
///
/// 成功 spawn 返回 Ok(())(不等待子进程);终端不存在或 spawn 失败返回 Err。
/// 重复调用会打开多个窗口,由调用方决定是否防抖(目前允许重复开)。
pub fn open() -> std::io::Result<()> {
    let Some(log_file) = latest_log_file() else {
        tracing::warn!("没找到日志文件,logs/ 目录可能还没有日志");
        return Ok(());
    };

    tracing::info!("打开实时日志窗口: {}", log_file.display());

    // PowerShell 命令:Get-Content -Wait -Tail 200 <file>
    // -Encoding UTF8:tracing 写日志是 UTF-8(无 BOM)。中文 Windows 的 PowerShell
    //   默认按系统 ANSI(GBK)读,不指定就会乱码。PS 5.1/7 都接受 UTF8 这个值。
    // [Console]::OutputEncoding=UTF8:即使读对了,PowerShell 窗口默认用 GBK 显示,
    //   中文照样乱,必须把输出编码也切到 UTF-8。
    // -Wait      = 持续监控(等价 tail -f)
    // -Tail 200  = 启动时先显示最后 200 行
    let ps_cmd = format!(
        "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; \
         Get-Content -Wait -Tail 200 -Encoding UTF8 -Path '{}'",
        log_file.display()
    );

    // 优先用 Windows Terminal(wt.exe),界面好、可搜索可复制。
    // wt 不在所有 Windows 上预装,失败回退 powershell.exe。
    match try_wt(&ps_cmd) {
        Ok(()) => Ok(()),
        Err(_) => try_powershell(&ps_cmd),
    }
}

/// 用 Windows Terminal 启动。
fn try_wt(ps_cmd: &str) -> std::io::Result<()> {
    // wt 参数: powershell -NoExit -Command "<cmd>"
    // 用 start shell verb 不需要;wt 自己会开新窗口。
    Command::new("wt.exe")
        .args(["-d", "."])
        .args(["powershell.exe", "-NoExit", "-Command", ps_cmd])
        .spawn()
        .map(|_| ())
}

/// 回退:直接用 powershell.exe 开新窗口。
fn try_powershell(ps_cmd: &str) -> std::io::Result<()> {
    Command::new("powershell.exe")
        .args(["-NoExit", "-Command", ps_cmd])
        .spawn()
        .map(|_| ())
}
