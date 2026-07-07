//! 系统托盘模块
//!
//! 在 eframe 的事件循环线程上创建 tray-icon(Windows 上必须和事件循环同线程),
//! 通过右键菜单提供「查看实时日志 / 设置 / 退出」。
//!
//! 菜单事件通过 muda 的全局 receiver(`MenuEvent::receiver()`)派发,
//! 在 eframe 的 `update()` 里轮询(`poll_commands`),转发成内部的 `TrayCommand`。
//!
//! 设计:托盘模块只负责「构建图标 + 菜单 + 把菜单事件转成枚举」,
//! 具体动作(开日志窗口、开设置窗口、退出)由上层 settings_app 执行。

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// 托盘菜单触发的命令。上层(egui app)从 MenuEvent 转成这个枚举后分发执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// 「查看实时日志」:打开一个外部终端 tail 日志文件
    ViewLogs,
    /// 「设置」:显示摄像头选择窗口
    Settings,
    /// 「退出」:结束整个程序
    Quit,
}

/// 菜单项 ID。muda 用 MenuId 匹配点击事件。
const ID_VIEW_LOGS: &str = "view_logs";
const ID_SETTINGS: &str = "settings";
const ID_QUIT: &str = "quit";

/// 持有 tray-icon 和它引用的菜单项。
///
/// 注意:TrayIconBuilder::with_menu 会拿走 Menu 的所有权,但 TrayIcon 内部对菜单是弱引用,
/// 所以菜单项(MenuItem / separator)必须自己保活 —— 否则它们一 drop,菜单就空了。
pub struct Tray {
    _icon: TrayIcon,
    // 这些字段只为了保活,不读取
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

/// 轮询 muda 全局菜单事件,转成 TrayCommand 返回。
/// 在 eframe update() 里每帧调一次。取走所有当前积压的事件。
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

/// 生成一个 32x32 的占位图标:深蓝背景 + 白色镜头简笔。
/// 真正的图标你之后可以用 image::open("icon.png") 替换这里。
fn make_icon() -> Icon {
    const W: u32 = 32;
    const H: u32 = 32;
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);

    for y in 0..H {
        for x in 0..W {
            // 背景:深蓝
            let bg = [30u8, 60, 130, 255];
            // 镜头外圈:圆心 (16,16),半径约 9,描边
            let dx = x as i32 - 16;
            let dy = y as i32 - 16;
            let dist2 = dx * dx + dy * dy;
            let on_ring = (70..=110).contains(&dist2); // r²≈81 外圈,取个带描边的范围
            // 镜头中心点:半径 3
            let on_dot = dist2 <= 9;

            let px = if on_dot {
                [60u8, 60, 70, 255] // 深灰镜头心
            } else if on_ring {
                [240u8, 240, 240, 255] // 白色镜头圈
            } else {
                bg
            };
            rgba.extend_from_slice(&px);
        }
    }

    Icon::from_rgba(rgba, W, H).expect("占位图标像素数据非法")
}
