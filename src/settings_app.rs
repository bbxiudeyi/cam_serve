//! egui 主应用 + 设置窗口。
//!
//! 设计:主窗口平时隐藏(无头运行,所有交互走托盘)。点托盘「设置」时,
//! 主窗口变可见并渲染摄像头选择 UI;点「关闭」重新隐藏。
//! 这样所有状态都在 CamApp 里,不用跨 deferred viewport 闭包传值。
//!
//! 摄像头热切换:设置里选设备 → 点「应用」→
//!   control.current_index.store(new)  // 立即更新(下次重连读)
//!   control.switch_tx.send(new)       // 通知 camera_manager_loop 立即切换

use eframe::egui;

use crate::camera::CameraInfo;
use crate::tray::{self, TrayCommand};
use crate::SharedControl;

pub struct CamApp {
    #[allow(dead_code)]
    _tray: tray::Tray,
    control: SharedControl,
    /// 后端致命错误的共享槽位。update 里轮询,有错就弹设置窗口显示。
    backend_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// 设置窗口是否可见。托盘「设置」→ true;窗口关闭按钮 → false。
    settings_visible: bool,
    /// 窗口当前是否在屏幕内(用于位置显隐的状态机,只在翻转时发 OuterPosition)。
    /// 与 settings_visible 配对:settings_visible 表达「想要」,shown_on_screen 表达「现状」。
    shown_on_screen: bool,
    /// 是否正在退出(点托盘「退出」→ Close)。置 true 后放行 X/Close,不再 CancelClose。
    quitting: bool,
    /// 设置里当前选中的设备 index(本地编辑状态,点「应用」才提交)
    selected: Option<u32>,
    /// 应用后的短暂提示(3 秒)
    toast: Option<(String, bool, std::time::Instant)>, // (text, is_success, t0)
}

impl CamApp {
    pub fn new(
        tray: tray::Tray,
        control: SharedControl,
        backend_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> Self {
        Self {
            _tray: tray,
            control,
            backend_error,
            settings_visible: false,
            shown_on_screen: false,
            quitting: false,
            selected: None,
            toast: None,
        }
    }

    /// 打开设置:刷新设备列表,默认选中当前 index。
    fn open_settings(&mut self) {
        let cams = crate::camera::list_cameras();
        *self.control.cameras.write() = cams;
        let cur = self
            .control
            .current_index
            .load(std::sync::atomic::Ordering::Relaxed);
        self.selected = Some(cur);
        self.settings_visible = true;
    }

    /// 提交当前选择,发切换命令。
    fn apply(&mut self) {
        let Some(idx) = self.selected else { return };
        self.control
            .current_index
            .store(idx, std::sync::atomic::Ordering::Relaxed);
        match self.control.switch_tx.send(idx) {
            Ok(()) => {
                self.toast = Some((
                    format!("✓ 已切换到 index={idx},正在重连…"),
                    true,
                    std::time::Instant::now(),
                ));
                tracing::info!("设置:用户选择 index={idx},已发切换命令");
            }
            Err(e) => {
                self.toast = Some((format!("✗ 切换失败: {e}"), false, std::time::Instant::now()));
            }
        }
    }

    /// 渲染设置窗口内容。
    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.heading("摄像头设备");
        ui.add_space(8.0);

        let cams: Vec<CameraInfo> = self.control.cameras.read().clone();
        let cur_idx = self
            .control
            .current_index
            .load(std::sync::atomic::Ordering::Relaxed);

        if cams.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.label("⚠️ 没有检测到摄像头设备");
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("请检查 USB 连接和 Windows 隐私设置")
                        .small()
                        .color(egui::Color32::GRAY),
                );
            });
        } else {
            // 单选列表
            let mut new_sel = self.selected;
            ui.vertical(|ui| {
                for cam in &cams {
                    let checked = self.selected == Some(cam.index);
                    if ui.radio(checked, &cam.name).clicked() {
                        new_sel = Some(cam.index);
                    }
                }
            });
            self.selected = new_sel;

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("当前使用:index = {cur_idx}"))
                    .small()
                    .color(egui::Color32::GRAY),
            );
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        // toast 提示(3 秒内显示)
        let mut toast_expired = false;
        if let Some((text, is_success, t0)) = &self.toast {
            if t0.elapsed().as_secs() < 3 {
                ui.horizontal(|ui| {
                    let color = if *is_success {
                        egui::Color32::from_rgb(40, 120, 40)
                    } else {
                        egui::Color32::from_rgb(180, 40, 40)
                    };
                    ui.label(egui::RichText::new(text).color(color));
                });
                ui.add_space(4.0);
            } else {
                toast_expired = true;
            }
        }
        if toast_expired {
            self.toast = None;
        }

        ui.horizontal(|ui| {
            // 右对齐按钮
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("关闭").clicked() {
                    self.settings_visible = false;
                }
                ui.add_space(8.0);
                let apply_enabled = self.selected.is_some() && self.selected != Some(cur_idx);
                ui.add_enabled_ui(apply_enabled, |ui| {
                    if ui.button("应用并切换").clicked() {
                        self.apply();
                    }
                });
            });
        });
    }
}

impl eframe::App for CamApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. 轮询托盘菜单事件。窗口常驻可见,后台线程的 request_repaint() 能可靠
        //    唤醒 update(),所以这里一定能取到命令。register 只在 main.rs 的 app
        //    creator 里做一次;update 里不再重复注册(否则每帧 spawn 一个阻塞线程)。
        for cmd in tray::poll_commands() {
            match cmd {
                TrayCommand::Quit => {
                    tracing::info!("收到退出命令,结束程序");
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                TrayCommand::ViewLogs => {
                    if let Err(e) = crate::log_viewer::open() {
                        tracing::warn!("打开日志窗口失败: {e}");
                    }
                }
                TrayCommand::Settings => {
                    self.open_settings();
                }
            }
        }

        // 2. 检查后端致命错误。有错 → 弹出设置窗口显示错误。
        let backend_err = self.backend_error.lock().ok().and_then(|mut g| g.take());
        if let Some(msg) = backend_err {
            self.toast = Some((format!("✗ 后端错误: {msg}"), false, std::time::Instant::now()));
            self.settings_visible = true;
        }

        // 3. 关闭(X / Close)的处理:
        //    - quitting(点托盘「退出」触发的 Close):放行 → 真正退出。
        //    - 否则(用户手点 X 等):只收起设置,CancelClose 让程序继续驻留托盘。
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.quitting {
            self.settings_visible = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }

        // 4. 位置显隐:仅在 settings_visible 与 shown_on_screen 不一致时发命令,
        //    把窗口在「屏内」与「屏外(-2000,-2000)」间搬动。从不 toggle Visible
        //    (egui#3655:Visible(false) 后再 Visible(true)/Close 对隐藏窗口无效,
        //     故改用 OuterPosition 实现「隐形」,窗口始终可见 → update() 可被唤醒)。
        if self.settings_visible != self.shown_on_screen {
            if self.settings_visible {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(120.0, 120.0)));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(-2000.0, -2000.0)));
            }
            self.shown_on_screen = self.settings_visible;
        }

        // 5. 渲染设置 UI(只在屏内时才有意义,但 egui 总要画点东西)
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.settings_visible {
                self.settings_ui(ui);
            }
        });

        // 6. 持续重绘(设置窗口打开时刷新 Toast;托盘事件由后台线程 request_repaint 唤醒)
        if self.settings_visible {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}
