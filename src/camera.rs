//! 摄像头捕获模块
//!
//! 用 nokhwa 的 `CallbackCamera`（内部用专属线程抓帧，回调发送给我们），
//! 每帧编码成 JPEG 字节后通过 broadcast 通道推给所有 HTTP 客户端。

use anyhow::Result;
use nokhwa::{
    pixel_format::RgbFormat,
    query,
    threaded::CallbackCamera,
    utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType},
};
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use tokio::sync::broadcast;

/// 一帧 JPEG 图像（已编码好的字节流，可直接发给浏览器）
#[derive(Clone)]
pub struct JpegFrame {
    pub bytes: bytes::Bytes,
    pub seq: u64,
}

/// 摄像头连接状态(供 AppState 共享,暴露给 /api/health)
/// 用 AtomicU8 存储,值对应下面的枚举判别数。
#[derive(Clone, Copy, Debug)]
pub enum CameraStatus {
    Disconnected = 0,
    Connecting = 1,
    Streaming = 2,
}

impl CameraStatus {
    /// 从 AtomicU8 读出的值还原成枚举
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => CameraStatus::Connecting,
            2 => CameraStatus::Streaming,
            _ => CameraStatus::Disconnected,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            CameraStatus::Disconnected => "disconnected",
            CameraStatus::Connecting => "connecting",
            CameraStatus::Streaming => "streaming",
        }
    }
}

/// 实际分辨率（摄像头最终应用的格式）
pub struct CaptureInfo {
    pub width: u32,
    pub height: u32,
    /// 持有它，drop 时自动停止采集
    _camera: CallbackCamera,
}

/// 启动摄像头采集。返回 `CaptureInfo`，需要一直持有它，
/// 一旦 drop 摄像头采集循环就会停止。
///
/// `last_frame_time`:共享的"最后一帧时间戳"(Unix 毫秒)。callback 每成功
/// 编码一帧就更新它,供外部的掉线监控线程检查"距上一帧多久了"。
pub fn spawn_capture(
    camera_index: u32,
    width: u32,
    height: u32,
    fps: u32,
    tx: broadcast::Sender<JpegFrame>,
    last_frame_time: Arc<AtomicI64>,
) -> Result<CaptureInfo> {
    // 请求格式:用 None,让 nokhwa/MediaFoundation 自己挑摄像头支持的格式(MJPG/NV12 都行)。
    //
    // 为什么不能用 Closest(RAWRGB):C270 等大多数 USB 摄像头在 640x480 等分辨率下
    // 只输出 MJPG/NV12,根本不给 RAWRGB。nokhwa 的 Closest 只在同格式内找最接近分辨率,
    // 不会跨格式(不会 MJPG→RGB),所以会报 "Failed to fulfill requested format"。
    //
    // None 的语义(见 nokhwa_core fulfill()):遍历摄像头支持的所有格式,挑第一个我们
    // 解码器(RgbFormat)能处理的。抓到 MJPG/NV12 后,回调里 decode_image::<RgbFormat>()
    // 会自动转成 RGB,再编码成 JPEG 推给浏览器。width/height/fps 参数这里不再参与选格式,
    // 但保留在 CaptureInfo 里供日志参考。
    //
    // 注:如果以后想精确控制分辨率,可以用 HighestResolution(Resolution::new(width,height))
    // 它不限定像素格式,只按分辨率筛选。
    let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::None);

    // 共享计数器（callback 在 nokhwa 内部线程里跑，需要 Arc<AtomicU64>）
    let seq_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let callback = {
        let seq_counter = seq_counter.clone();
        let tx = tx.clone();
        let last_frame_time = last_frame_time.clone();
        move |buffer: nokhwa::Buffer| {
            let seq = seq_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match buffer.decode_image::<RgbFormat>() {
                Ok(img) => {
                    // 用解码后图像自带的尺寸，而非请求分辨率：摄像头实际生效的
                    // 分辨率可能与请求值不同（nokhwa 会选最接近的支持值），用错
                    // 会导致 RGB buffer 长度对不上，编码失败或画面错位。
                    let (w, h) = img.dimensions();
                    let rgb = img.into_raw();
                    match encode_rgb_to_jpeg(&rgb, w, h, 80) {
                        Ok(jpeg) => {
                            // 记录这一帧的时间戳(自 epoch 的毫秒),供掉线检测用
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0);
                            last_frame_time.store(now, std::sync::atomic::Ordering::Relaxed);
                            let _ = tx.send(JpegFrame {
                                bytes: bytes::Bytes::from(jpeg),
                                seq,
                            });
                        }
                        Err(e) => tracing::warn!("JPEG 编码失败: {e}"),
                    }
                }
                Err(e) => tracing::warn!("帧解码失败: {e}"),
            }
        }
    };

    // 用 Debug 格式 {:#?} 保留 nokhwa 的完整错误链。原来 .with_context() 只盖了一层
    // "无法打开摄像头",真实原因(Media Foundation 协商失败、格式不支持等)被吞掉了。
    let mut camera = match CallbackCamera::new(
        CameraIndex::Index(camera_index),
        requested,
        callback,
    ) {
        Ok(c) => c,
        Err(e) => {
            // e 是 nokhwa::NokhwaError,打印它的 Debug 能看到内部细节
            return Err(anyhow::anyhow!(
                "CallbackCamera::new 失败(index={}): {e:#?}",
                camera_index
            ));
        }
    };

    let actual_format = match camera.camera_format() {
        Ok(f) => f,
        Err(e) => {
            return Err(anyhow::anyhow!("读取摄像头格式失败: {e:#?}"));
        }
    };
    let actual_w = actual_format.width();
    let actual_h = actual_format.height();

    tracing::info!(
        "摄像头已打开: index={} 实际 {}x{} @{}fps 格式={:?}(请求 {}x{}@{}fps,格式由摄像头自选)",
        camera_index,
        actual_w,
        actual_h,
        actual_format.frame_rate(),
        actual_format.format(),
        width,
        height,
        fps
    );

    // 启动采集流
    if let Err(e) = camera.open_stream() {
        return Err(anyhow::anyhow!("启动摄像头采集流失败: {e:#?}"));
    }

    Ok(CaptureInfo {
        width: actual_w,
        height: actual_h,
        _camera: camera,
    })
}

/// RGB888 -> JPEG 编码
fn encode_rgb_to_jpeg(rgb: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>> {
    use image::{codecs::jpeg::JpegEncoder, ColorType, ImageEncoder};
    let mut buf = Vec::with_capacity((width * height / 4) as usize);
    let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    encoder.write_image(rgb, width, height, ColorType::Rgb8.into())?;
    Ok(buf)
}

/// 列出当前系统所有可用摄像头
pub fn list_cameras() -> Vec<CameraInfo> {
    match query(ApiBackend::Auto) {
        Ok(cams) => cams
            .into_iter()
            .map(|info| CameraInfo {
                index: info.index().as_index().unwrap_or(0),
                name: info.human_name(),
                description: info.description().to_string(),
            })
            .collect(),
        Err(e) => {
            tracing::warn!("枚举摄像头失败: {e}");
            vec![]
        }
    }
}

#[derive(serde::Serialize, Clone)]
pub struct CameraInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
}
