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
        if let Some((text, _ok, t0)) = &self.toast {
            if t0.elapsed().as_secs() < 3 {
                ui.horizontal(|ui| {
                    let color = if text.starts_with('✓') {
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
        // 1. 轮询托盘菜单事件
        for cmd in tray::poll_commands() {
            match cmd {
                TrayCommand::Quit => {
                    tracing::info!("收到退出命令,结束程序");
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

        // 1.5 检查后端致命错误。有错 → 弹出设置窗口显示错误。
        // (取走错误,只显示一次)
        let backend_err = self.backend_error.lock().ok().and_then(|mut g| g.take());
        if let Some(msg) = backend_err {
            self.toast = Some((format!("✗ 后端错误: {msg}"), false, std::time::Instant::now()));
            self.settings_visible = true;
        }

        // 2. 用户点窗口 X 关闭 → 改成隐藏设置(不退出整个 app)
        //    egui 通过 ViewportInput 报告 close 请求;拦截它并重新隐藏。
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && self.settings_visible {
            self.settings_visible = false;
            // 取消关闭(否则会退出整个 app)
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }

        // 3. 根据设置可见性切换主窗口显隐
        //    设置可见 → 主窗口显示并渲染设置;否则 → 隐藏
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(self.settings_visible));

        // 4. 渲染设置 UI(只在可见时才有意义,但 egui 总要画点东西)
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.settings_visible {
                self.settings_ui(ui);
            }
        });

        // 5. 持续重绘,让托盘事件及时响应
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}
