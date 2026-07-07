//! USB 摄像头实时视频流服务
//!
//! 启动：
//!   cargo run --release
//!
//! 浏览器访问 http://localhost:3000 即可看到实时画面。
//!
//! 端点：
//!   GET  /ws            WebSocket 传输 JPEG 帧（前端可用 <canvas> 渲染 + 显示 FPS）
//!   GET  /api/cameras   列出所有可用摄像头
//!   GET  /api/health    健康检查

// release 构建在 Windows 上用 GUI 子系统,双击 exe 不弹黑窗。
// debug 构建保留控制台,方便看 eprintln! / 日志输出。
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod camera;
mod log_viewer;
mod settings_app;
mod tray;

use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use eframe::egui;
use camera::JpegFrame;
use std::{
    sync::atomic::{AtomicI64, AtomicU8},
    sync::Arc,
    time::Duration,
};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tracing_subscriber::{prelude::*, EnvFilter};

const CHANNEL_CAPACITY: usize = 8; // 最多缓存 8 帧，避免慢客户端拖累采集线程

/// 摄像头相关、需要跨「后台 axum 线程」和「主线程 GUI」共享的控制状态。
///
/// 这是两个世界的桥梁:GUI(设置窗口)读写它来枚举/选择摄像头,
/// 后台 camera_manager_loop 读它决定用哪个设备、收切换命令。
///
/// - `cameras`:设备列表,设置窗口打开时刷新。RwLock 因为读多写少。
/// - `current_index`:当前选中的摄像头 index。AtomicU32 因为只读写一个 u32。
/// - `switch_tx`:切换命令通道。设置窗口 send,管理循环 recv_timeout。
#[derive(Clone)]
pub(crate) struct SharedControl {
    pub cameras: Arc<parking_lot::RwLock<Vec<camera::CameraInfo>>>,
    pub current_index: Arc<std::sync::atomic::AtomicU32>,
    pub switch_tx: std::sync::mpsc::Sender<u32>,
}

/// 全局应用状态(axum router 用)
#[derive(Clone)]
struct AppState {
    /// 帧广播通道
    tx: broadcast::Sender<JpegFrame>,
    /// 摄像头共享控制(含设备列表、当前 index、切换通道)
    control: SharedControl,
    /// 摄像头连接状态(0=Disconnected, 1=Connecting, 2=Streaming)
    camera_status: Arc<AtomicU8>,
}

/// 返回 exe 所在目录。取不到(如非 Windows)则回退到当前工作目录。
/// 用来定位 logs/ 目录,保证无论从哪里双击 exe,日志都落在 exe 旁边。
pub(crate) fn exe_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn main() -> eframe::Result<()> {
    // 架构:主线程跑 GUI 事件循环(eframe + tray-icon);axum + 摄像头放到后台线程的
    // tokio runtime 里。两个世界通过 SharedControl(通道 + 原子)通信。

    // ---------- 创建共享控制状态(在 main 里,两个世界都能 clone) ----------
    let initial_index: u32 = std::env::var("CAM_INDEX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (switch_tx, switch_rx) = std::sync::mpsc::channel::<u32>();
    let control = SharedControl {
        cameras: Arc::new(parking_lot::RwLock::new(Vec::new())),
        current_index: Arc::new(std::sync::atomic::AtomicU32::new(initial_index)),
        switch_tx,
    };

    // ---------- 后台线程:tokio runtime + axum + 摄像头采集 ----------
    // 后端错误通过 backend_error 传回主线程,在设置窗口里显示。
    let backend_error: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    let backend = std::thread::Builder::new()
        .name("cam-backend".into())
        .spawn({
            let control = control.clone();
            let err_slot = backend_error.clone();
            move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let msg = format!("创建 tokio runtime 失败: {e}");
                        *err_slot.lock().unwrap() = Some(msg.clone());
                        tracing::error!("{msg}");
                        return;
                    }
                };
                if let Err(e) = runtime.block_on(run(control, switch_rx)) {
                    // 后端致命错误(端口被占用、摄像头初始化异常等)。
                    // 落盘日志 + 传回主线程显示。
                    let msg = format!("{e:#}");
                    tracing::error!("后端退出: {msg}");
                    *err_slot.lock().unwrap() = Some(msg);
                }
            }
        })
        .expect("无法创建后端线程");

    // ---------- 主线程:eframe 事件循环 ----------
    // 窗口配置:主窗口默认隐藏(无头运行,交互走托盘),设置时变可见。
    // - with_visible(false):启动时不显示
    // - with_inner_size:设置 UI 的尺寸(显示时用)
    // - with_resizable(false):设置窗口固定大小
    // 任务栏:默认隐藏时不占位;egui 在 with_visible 时会处理任务栏显示
    let viewport = egui::ViewportBuilder::default()
        .with_visible(false)
        .with_inner_size([420.0, 280.0])
        .with_resizable(false)
        .with_title("cam-stream 设置");

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "cam-stream",
        native_options,
        Box::new(move |_cc| {
            // 托盘必须在创建 eframe app 的同一线程(事件循环线程)里 build,
            // 否则 Windows 上收不到托盘消息。
            let tray = tray::build();
            Ok(Box::new(settings_app::CamApp::new(
                tray,
                control.clone(),
                backend_error.clone(),
            )))
        }),
    )?;

    // eframe 退出后(用户点了托盘「退出」),后端线程可能还在跑 axum。
    // 直接 exit 最干净 —— 后台的 axum::serve 会被强制中断,但日志已落盘。
    drop(backend); // detach
    std::process::exit(0);
}

async fn run(control: SharedControl, switch_rx: std::sync::mpsc::Receiver<u32>) -> Result<()> {
    // ---------- 日志 ----------
    // 输出到两处:① 控制台(双击 exe 时实时滚动);② 文件(按天滚动,exe 同级 logs/)。
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // 文件 appender:日志写到 exe 所在目录的 logs/ 子目录,按天滚动。
    // tracing_appender::rolling::daily 返回的 guard 必须存活到程序结束,
    // 否则后台 flush 线程被提前 drop,末尾日志会丢。这里用 _guard 持有到 main 返回。
    let log_dir = exe_dir().join("logs");
    let file_layer_guard = tracing_appender::rolling::daily(&log_dir, "cam-stream.log");
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_layer_guard)
        .with_ansi(false); // 文件里不要 ANSI 颜色码,纯文本

    // 控制台层:保留 ANSI 颜色,实时滚动好看
    let console_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(filter)
        .with(console_layer)
        .with(file_layer)
        .init();

    // ---------- 启动参数 ----------
    // camera_index 不在这里读了:改由 camera_manager_loop 每轮循环从 control.current_index
    // 读最新值(支持运行时热切换)。初始值在 main 里从 CAM_INDEX 读。
    let width: u32 = std::env::var("CAM_WIDTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(640);
    let height: u32 = std::env::var("CAM_HEIGHT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(480);
    let fps: u32 = std::env::var("CAM_FPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let bind_addr: String = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    // ---------- 摄像头 ----------
    // 设备列表存进共享 control(cameras 字段),供设置窗口和 /api/cameras 刷新/读取。
    {
        let cams = camera::list_cameras();
        tracing::info!("检测到 {} 个摄像头设备:", cams.len());
        for c in cams.iter() {
            tracing::info!("  [{}] {} - {}", c.index, c.name, c.description);
        }
        *control.cameras.write() = cams;
    }

    // broadcast channel 先于摄像头创建:即使没有订阅者,send 也只是无操作,
    // 这样 HTTP 服务不依赖摄像头是否连上。
    let (tx, _rx) = broadcast::channel::<JpegFrame>(CHANNEL_CAPACITY);

    // 共享的摄像头状态 + 最后一帧时间戳,后台重连任务和 health 端点都要用
    let camera_status = Arc::new(AtomicU8::new(camera::CameraStatus::Disconnected as u8));
    let last_frame_time = Arc::new(AtomicI64::new(0));

    // 启动后台任务:持续重试连接摄像头,连上后监控掉线,掉线自动重连。
    // 用 spawn_blocking 因为 spawn_capture 内部是阻塞调用(nokhwa CallbackCamera)。
    // switch_rx 由 main 创建并通过 run 参数传入;control.switch_tx 供设置窗口发命令。
    tokio::task::spawn_blocking({
        let camera_status = camera_status.clone();
        let last_frame_time = last_frame_time.clone();
        let tx = tx.clone();
        let control = control.clone();
        move || camera_manager_loop(control, switch_rx, width, height, fps, tx, camera_status, last_frame_time)
    });

    let state = AppState {
        tx,
        control,
        camera_status,
    };

    // ---------- 路由 ----------
    // CORS:允许网页由其他服务(Python http.server、Vite、其他端口)托管时跨域访问视频流。
    // cam-stream 本身只提供视频流端点,不托管任何网页文件。
    let cors = CorsLayer::very_permissive();

    let app = Router::new()
        .route("/api/health", get(health_handler))
        .route("/api/cameras", get(list_cameras_handler))
        .route("/ws", get(ws_handler))
        .layer(cors)
        .with_state(state);

    // ---------- 启动 ----------
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    let port = bind_addr.split(':').last().unwrap_or("3000");
    tracing::info!("🚀 视频流服务已启动: http://{}", bind_addr);
    tracing::info!("   后台正在连接摄像头(就绪后会自动开始推流)...");
    tracing::info!("   WebSocket:  http://localhost:{port}/ws");
    tracing::info!("   网页需另行托管(如 python serve.py),指向上面的端点");
    axum::serve(listener, app).await?;
    Ok(())
}

/// 摄像头后台管理循环:重试连接 → 采集 → 掉线检测 → 重连。
/// 在 spawn_blocking 线程里跑,内部用 std::thread::sleep(不能用 tokio 的 sleep)。
///
/// 热切换:`switch_rx` 收到新 index 时,立即 break 当前设备去重连新设备。
/// 「停旧开新」靠 break → info drop(释放设备)→ 外层 loop 重新 spawn_capture 天然完成。
fn camera_manager_loop(
    control: SharedControl,
    switch_rx: std::sync::mpsc::Receiver<u32>,
    width: u32,
    height: u32,
    fps: u32,
    tx: broadcast::Sender<JpegFrame>,
    camera_status: Arc<AtomicU8>,
    last_frame_time: Arc<AtomicI64>,
) {
    const RECONNECT_INTERVAL: Duration = Duration::from_secs(3); // 连接失败重试间隔
    const STALL_TIMEOUT: Duration = Duration::from_secs(5);      // 距上一帧超过此值视为掉线
    // 失败时重新枚举摄像头的最小间隔,避免每 3 秒刷屏(Media Foundation 枚举也略慢)。
    const REENUM_INTERVAL: Duration = Duration::from_secs(15);

    // 上次枚举摄像头的时间。失败/掉线时若距上次超过 REENUM_INTERVAL,就重新列一遍,
    // 这样用户能从日志看出摄像头是不是还在系统里(USB 松动会让它消失)。
    let mut last_enum = None::<std::time::Instant>;

    loop {
        // 每轮循环读最新 index(用户可能从设置窗口切换了设备)
        let camera_index = control
            .current_index
            .load(std::sync::atomic::Ordering::Relaxed);

        // 标记为"连接中"
        camera_status.store(
            camera::CameraStatus::Connecting as u8,
            std::sync::atomic::Ordering::Relaxed,
        );
        tracing::info!("正在连接摄像头 index={} ({}x{}@{}fps)...", camera_index, width, height, fps);

        match camera::spawn_capture(camera_index, width, height, fps, tx.clone(), last_frame_time.clone()) {
            Ok(info) => {
                // 连接成功,标记为"推流中"
                camera_status.store(
                    camera::CameraStatus::Streaming as u8,
                    std::sync::atomic::Ordering::Relaxed,
                );
                tracing::info!("摄像头已连接,实际分辨率 {}x{},开始推流", info.width, info.height);

                // 监控掉线 + 切换命令:用 recv_timeout(1s) 代替 sleep(1s)。
                //   - 超时(Err Timeout) = 正常轮询,继续检查 last_frame_time
                //   - Ok(new_idx) = 用户要切换设备,break 去重连新设备
                // info 在 break 后 block 结束时 drop,触发停止采集(释放设备句柄)。
                loop {
                    // 先检查掉线
                    if let Some(since_last) = millis_since(&last_frame_time) {
                        if Duration::from_millis(since_last as u64) > STALL_TIMEOUT {
                            tracing::warn!("摄像头已 {} 秒无新帧,判定掉线,准备重连", STALL_TIMEOUT.as_secs());
                            break;
                        }
                    }
                    // 用 recv_timeout 代替 sleep,顺便监听切换命令
                    match switch_rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(new_idx) => {
                            tracing::info!("收到切换命令 → index={},切换摄像头", new_idx);
                            // current_index 已由设置窗口更新,这里只需 break 重连
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            // switch_tx 被 drop(程序退出中),直接退出循环
                            tracing::info!("切换通道已关闭,退出摄像头管理循环");
                            return;
                        }
                    }
                }
                // info 在这里 drop,CallbackCamera 停止采集。
                maybe_reenumerate(&mut last_enum, REENUM_INTERVAL);
            }
            Err(e) => {
                camera_status.store(
                    camera::CameraStatus::Disconnected as u8,
                    std::sync::atomic::Ordering::Relaxed,
                );
                tracing::warn!("连接摄像头失败: {e:#},{} 秒后重试", RECONNECT_INTERVAL.as_secs());
                maybe_reenumerate(&mut last_enum, REENUM_INTERVAL);
                // 失败重试也用 recv_timeout,这样切换命令能在等待期间及时响应
                match switch_rx.recv_timeout(RECONNECT_INTERVAL) {
                    Ok(new_idx) => {
                        tracing::info!("(等待重试期间)收到切换命令 → index={}", new_idx);
                        // 直接进入下一轮循环,用新 index 重连
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }
}

/// 重新枚举摄像头并打印当前系统可见的设备列表。
fn log_camera_list() {
    let cams = camera::list_cameras();
    tracing::info!("当前系统检测到 {} 个摄像头设备:", cams.len());
    for c in cams.iter() {
        tracing::info!("  [{}] {} - {}", c.index, c.name, c.description);
    }
}

/// 节流地重新枚举摄像头并打印。距上次枚举不足 min_interval 则跳过,避免日志刷屏。
fn maybe_reenumerate(last_enum: &mut Option<std::time::Instant>, min_interval: Duration) {
    let should_enum = match last_enum {
        None => true,
        Some(t) => t.elapsed() >= min_interval,
    };
    if should_enum {
        log_camera_list();
        *last_enum = Some(std::time::Instant::now());
    }
}

/// 返回"距上一帧过了多少毫秒"。last_frame_time 为 0(从未收到帧)时返回 None,
/// 这种情况下外层不判定掉线(给摄像头启动一点时间出第一帧)。
fn millis_since(last_frame_time: &AtomicI64) -> Option<u64> {
    let last = last_frame_time.load(std::sync::atomic::Ordering::Relaxed);
    if last == 0 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Some((now - last).max(0) as u64)
}

// ============================================================================
// Handlers
// ============================================================================

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let raw = state.camera_status.load(std::sync::atomic::Ordering::Relaxed);
    let camera = camera::CameraStatus::from_u8(raw).as_str();
    axum::Json(serde_json::json!({"status": "ok", "camera": camera}))
}

async fn list_cameras_handler(State(state): State<AppState>) -> impl IntoResponse {
    // 读共享的设备列表(设置窗口可能已经刷新过它)
    let cams = state.control.cameras.read().clone();
    axum::Json(cams)
}

/// WebSocket 流：每条 binary message 是一帧 JPEG，前端用 canvas 渲染
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    let mut rx = state.tx.subscribe();
    ws.on_upgrade(move |socket: WebSocket| async move {
        tracing::info!("WebSocket 客户端已连接");
        let mut last_fps_check = std::time::Instant::now();
        let mut frames_in_window: u32 = 0;

        let mut socket = socket;
        loop {
            tokio::select! {
                frame = rx.recv() => {
                    match frame {
                        Ok(f) => {
                            frames_in_window += 1;
                            // 每秒向客户端发一次 fps
                            if last_fps_check.elapsed() >= Duration::from_secs(1) {
                                let fps = frames_in_window as f32
                                    / last_fps_check.elapsed().as_secs_f32();
                                let msg = serde_json::json!({
                                    "type": "stats",
                                    "fps": fps.round(),
                                    "seq": f.seq,
                                });
                                if socket.send(Message::Text(msg.to_string())).await.is_err() {
                                    break;
                                }
                                frames_in_window = 0;
                                last_fps_check = std::time::Instant::now();
                            }
                            if socket.send(Message::Binary((*f.bytes).clone())).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                msg = socket.recv() => {
                    match msg {
                        Some(Ok(Message::Close(_))) | None => break,
                        _ => {}
                    }
                }
            }
        }
        tracing::info!("WebSocket 客户端已断开");
    })
}