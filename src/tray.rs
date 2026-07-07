//! 系统托盘模块
//!
//! 在 eframe 的事件循环线程上创建 tray-icon(Windows 上必须和事件循环同线程),
//! 通过右键菜单提供「查看实时日志 / 设置 / 退出」。
//!
//! 关键:winit 在窗口隐藏时会停止重绘,导致 eframe 的 update() 不再被调用。
//! 如果菜单事件只在 update() 里轮询,就会死锁(事件没人收)。
//! 解法:用 MenuEvent::set_event_handler 注册一个处理器,收到菜单事件时
//! 主动调 ctx.request_repaint() 唤醒 eframe,这样 update() 就会跑起来。

use eframe::egui;
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
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

/// 全局 egui Context:菜单事件处理器要用它触发重绘,唤醒 update()。
static EGUI_CTX: OnceLock<egui::Context> = OnceLock::new();

/// 在创建 eframe app 时调用一次,把 egui Context 存起来。
pub fn register_egui_ctx(ctx: egui::Context) {
    let _ = EGUI_CTX.set(ctx);
    // 注册菜单事件处理器:收到事件时触发重绘,唤醒 update()
    MenuEvent::set_event_handler(Some(move |_event: MenuEvent| {
        if let Some(ctx) = EGUI_CTX.get() {
            ctx.request_repaint();
        }
    }));
}

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

/// 在 update() 里调用,取走所有积压的菜单事件。
/// 菜单事件处理器已触发重绘,保证 update() 一定会被调用到这里。
pub fn poll_commands() -> Vec<TrayCommand> {
    let rx = MenuEvent::receiver();
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        match ev.id().0.as_str() {
            ID_VIEW_LOGS => out.push(TrayCommand::ViewLogs),
            ID_SETTINGS => out.push(TrayCommand::Settings),
            ID_QUIT => out.push(TrayCommand::Quit),
            _ => {}
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
