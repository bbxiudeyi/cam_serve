//! USB 摄像头实时视频流服务
//!
//! 启动：
//!   cargo run --release
//!
//! 浏览器访问 http://localhost:3000 即可看到实时画面。
//!
//! 端点：
//!   GET  /              HTML 页面
//!   GET  /stream        MJPEG multipart 流（<img src="/stream"> 直接显示）
//!   GET  /ws            WebSocket 传输 JPEG 帧（前端可用 <canvas> 渲染 + 显示 FPS）
//!   GET  /api/cameras   列出所有可用摄像头
//!   GET  /api/health    健康检查

mod camera;

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

/// 全局应用状态
#[derive(Clone)]
struct AppState {
    /// 帧广播通道
    tx: broadcast::Sender<JpegFrame>,
    /// 摄像头可用信息
    cameras: Arc<Vec<camera::CameraInfo>>,
    /// 摄像头连接状态(0=Disconnected, 1=Connecting, 2=Streaming)
    camera_status: Arc<AtomicU8>,
}

/// 返回 exe 所在目录。取不到(如非 Windows)则回退到当前工作目录。
/// 用来定位 logs/ 目录,保证无论从哪里双击 exe,日志都落在 exe 旁边。
fn exe_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

#[tokio::main]
async fn main() {
    // 把实际工作放到 run(),main 负责捕获错误后"暂停等按键",
    // 这样双击 exe 启动时窗口不会秒退,用户能看清错误信息。
    if let Err(e) = run().await {
        eprintln!();
        eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        eprintln!("❌ 程序出错退出:");
        eprintln!("{e:#}");
        eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        eprintln!("💡 常见原因:");
        eprintln!("   • 没有可用摄像头(检查 USB 连接 / 设备管理器)");
        eprintln!("   • 摄像头被其他程序占用(浏览器、Zoom、会议软件)");
        eprintln!("   • Windows 隐私设置禁用了摄像头访问");
        eprintln!("   • 换设备号试试:设置环境变量 CAM_INDEX=1");
        eprintln!();
        eprintln!("详细日志见 exe 同级 logs/ 目录下的日志文件。");
        eprintln!();
        eprintln!("按 Enter 键关闭窗口…");
        let mut buf = String::new();
        let _ = std::io::stdin().read_line(&mut buf);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
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
    let camera_index: u32 = std::env::var("CAM_INDEX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
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
    let cameras = Arc::new(camera::list_cameras());
    tracing::info!("检测到 {} 个摄像头设备:", cameras.len());
    for c in cameras.iter() {
        tracing::info!("  [{}] {} - {}", c.index, c.name, c.description);
    }

    // broadcast channel 先于摄像头创建:即使没有订阅者,send 也只是无操作,
    // 这样 HTTP 服务不依赖摄像头是否连上。
    let (tx, _rx) = broadcast::channel::<JpegFrame>(CHANNEL_CAPACITY);

    // 共享的摄像头状态 + 最后一帧时间戳,后台重连任务和 health 端点都要用
    let camera_status = Arc::new(AtomicU8::new(camera::CameraStatus::Disconnected as u8));
    let last_frame_time = Arc::new(AtomicI64::new(0));

    // 启动后台任务:持续重试连接摄像头,连上后监控掉线,掉线自动重连。
    // 用 spawn_blocking 因为 spawn_capture 内部是阻塞调用(nokhwa CallbackCamera)。
    tokio::task::spawn_blocking({
        let camera_status = camera_status.clone();
        let last_frame_time = last_frame_time.clone();
        let tx = tx.clone();
        move || camera_manager_loop(camera_index, width, height, fps, tx, camera_status, last_frame_time)
    });

    let state = AppState {
        tx,
        cameras: cameras.clone(),
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
fn camera_manager_loop(
    camera_index: u32,
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

                // 监控掉线:每秒检查 last_frame_time,超过 STALL_TIMEOUT 没有新帧就重连。
                // 不调用 camera.is_streaming()——nokhwa 0.10 有已知锁问题(issue #111)。
                // info 在循环末尾 drop,触发停止采集。
                while let Some(since_last) = millis_since(&last_frame_time) {
                    if Duration::from_millis(since_last as u64) > STALL_TIMEOUT {
                        tracing::warn!("摄像头已 {} 秒无新帧,判定掉线,准备重连", STALL_TIMEOUT.as_secs());
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                // 这里 info 被 drop,CallbackCamera 停止采集。
                // 掉线重连前也重新枚举一次(节流),方便排查是 USB 掉线还是被占用。
                maybe_reenumerate(&mut last_enum, REENUM_INTERVAL);
            }
            Err(e) => {
                camera_status.store(
                    camera::CameraStatus::Disconnected as u8,
                    std::sync::atomic::Ordering::Relaxed,
                );
                tracing::warn!("连接摄像头失败: {e:#},{} 秒后重试", RECONNECT_INTERVAL.as_secs());
                // 打开失败很可能是被占用或设备已拔出。失败时重新枚举摄像头,
                // 用户能从日志看出摄像头还在不在系统里。节流避免每 3 秒刷屏。
                maybe_reenumerate(&mut last_enum, REENUM_INTERVAL);
                std::thread::sleep(RECONNECT_INTERVAL);
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
    axum::Json(state.cameras.as_ref().clone())
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