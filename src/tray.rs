//! 系统托盘模块
//!
//! 在 eframe 的事件循环线程上创建 tray-icon(Windows 上必须和事件循环同线程),
//! 通过右键菜单提供「查看实时日志 / 设置 / 退出」。
//!
//! 事件分发(关键设计):
//! muda 的 MenuEvent 有两种消费方式,且互斥(见 muda 文档):
//!   - set_event_handler(Some(f))  → 事件进 f,receiver() 收不到
//!   - receiver()                  → 事件进通道,handler 必须是 None
//! 我们用 receiver() + 独立线程阻塞 recv(),因为窗口隐藏时 update() 不跑,
//! 在 update 里 try_recv 会漏事件。线程拿到事件后:
//!   1. ctx.request_repaint() 唤醒 eframe(让 update 跑,渲染设置 UI)
//!   2. 写入 pending_commands(Crossbeam unbounded),update 里取走执行
//! 对于 ViewLogs/Quit/Settings 这类需要立即响应的命令,线程直接处理或通过标志传递。

use eframe::egui;
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// 托盘菜单触发的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ViewLogs,
    Settings,
    Quit,
}

const ID_VIEW_LOGS: &str = "view_logs";
const ID_SETTINGS: &str = "settings";
const ID_QUIT: &str = "quit";

/// 待处理的命令队列:事件线程 send,update 里 take。
/// tx 和 rx 都存进 OnceLock,保证线程和 update 能各取所需。
static PENDING_TX: OnceLock<crossbeam_channel::Sender<TrayCommand>> = OnceLock::new();
static PENDING_RX: OnceLock<crossbeam_channel::Receiver<TrayCommand>> = OnceLock::new();

/// 持有 tray-icon 和它引用的菜单项。
pub struct Tray {
    _icon: TrayIcon,
    _items: Vec<MenuItem>,
    _seps: Vec<PredefinedMenuItem>,
}

/// 在当前线程(必须是 eframe 事件循环线程)创建托盘图标 + 菜单。
pub fn build() -> Tray {
    let menu = Menu::new();

    let item_logs = MenuItem::with_id(ID_VIEW_LOGS, "查看实时日志", true, None);
    let item_settings = MenuItem::with_id(ID_SETTINGS, "设置...", true, None);
    let sep1 = PredefinedMenuItem::separator();
    let sep2 = PredefinedMenuItem::separator();
    let item_quit = MenuItem::with_id(ID_QUIT, "退出", true, None);

    let items: [&dyn muda::IsMenuItem; 5] = [
        &item_logs,
        &item_settings,
        &sep1,
        &sep2,
        &item_quit,
    ];
    menu.append_items(&items).expect("无法构建托盘菜单");

    let icon = make_icon();
    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("cam-stream 视频流服务")
        .with_icon(icon)
        .build()
        .expect("无法创建托盘图标");

    Tray {
        _icon: tray_icon,
        _items: vec![item_logs, item_settings, item_quit],
        _seps: vec![sep1, sep2],
    }
}

/// 后台轮询线程是否已启动(幂等保护)。register_egui_ctx 只应在 app creator
/// 里调一次,这里再加一道全局守卫,防止误被重复调用时每次 spawn 一个阻塞线程。
static THREAD_STARTED: AtomicBool = AtomicBool::new(false);

/// 在 eframe app_creator 闭包里调用一次。
///
/// 启动独立线程阻塞 recv() MenuEvent,把命令塞进 PENDING 通道,
/// 并 request_repaint() 唤醒 eframe 的 update()。
/// 不用 set_event_handler(会和 receiver() 互斥,导致事件丢失)。
/// 幂等:多次调用也只起一个轮询线程(PENDING 通道由 OnceLock 保证只建一次)。
pub fn register_egui_ctx(ctx: egui::Context) {
    let (tx, rx) = crossbeam_channel::unbounded::<TrayCommand>();
    let _ = PENDING_TX.set(tx);
    let _ = PENDING_RX.set(rx);

    // 幂等:已起过线程就直接返回。swap 返回旧值,旧值为 true 表示已经起过。
    if THREAD_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    std::thread::Builder::new()
        .name("tray-event-poller".into())
        .spawn(move || {
            // muda 的 receiver() 返回 &'static,跨线程用安全。
            let menu_rx = MenuEvent::receiver();
            loop {
                // 阻塞等待菜单事件
                let Ok(event) = menu_rx.recv() else {
                    return; // 通道关闭,退出线程
                };
                let cmd = match event.id().0.as_str() {
                    ID_VIEW_LOGS => Some(TrayCommand::ViewLogs),
                    ID_SETTINGS => Some(TrayCommand::Settings),
                    ID_QUIT => Some(TrayCommand::Quit),
                    _ => None,
                };
                if let Some(cmd) = cmd {
                    if let Some(tx) = PENDING_TX.get() {
                        let _ = tx.send(cmd);
                    }
                    // 关键:唤醒 eframe 重绘。窗口常驻可见时,update() 会被可靠唤起。
                    ctx.request_repaint();
                }
            }
        })
        .expect("无法创建托盘事件轮询线程");
}

/// update() 里调用:取走所有待处理的菜单命令。
pub fn poll_commands() -> Vec<TrayCommand> {
    let mut out = Vec::new();
    if let Some(rx) = PENDING_RX.get() {
        while let Ok(cmd) = rx.try_recv() {
            out.push(cmd);
        }
    }
    out
}

/// 生成一个 32x32 的占位图标。
fn make_icon() -> Icon {
    const W: u32 = 32;
    const H: u32 = 32;
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);

    for y in 0..H {
        for x in 0..W {
            let bg = [30u8, 60, 130, 255];
            let dx = x as i32 - 16;
            let dy = y as i32 - 16;
            let dist2 = dx * dx + dy * dy;
            let on_ring = (70..=110).contains(&dist2);
            let on_dot = dist2 <= 9;

            let px = if on_dot {
                [60u8, 60, 70, 255]
            } else if on_ring {
                [240u8, 240, 240, 255]
            } else {
                bg
            };
            rgba.extend_from_slice(&px);
        }
    }

    Icon::from_rgba(rgba, W, H).expect("占位图标像素数据非法")
}
