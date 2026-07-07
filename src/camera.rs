//! 摄像头捕获模块（Windows Media Foundation 原生实现）
//!
//! 直接用 Media Foundation 的 IMFSourceReader 读帧，绕开 nokhwa
//! 对采集卡格式协商的 bug。优先选择 MJPG（直接透传给浏览器，零编码成本），
//! 其次 RGB32（转成 JPEG）。对外接口与原 nokhwa 版本一致。
use anyhow::{anyhow, Result};
use bytes::Bytes;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use tokio::sync::broadcast;
use windows::core::PCWSTR;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFMediaSource, IMFSample, IMFSourceReader,
    MFCreateAttributes, MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFStartup,
    MF_MT_FRAME_SIZE, MF_MT_SUBTYPE, MF_VERSION, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MFVideoFormat_MJPG, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

/// COM 接口的 Send wrapper。Media Foundation 在 MTA（多线程单元）下接口是
/// 线程安全的，这里 unsafe impl Send 允许把接口 move 到采集线程。
struct SendCom<T>(T);
unsafe impl<T> Send for SendCom<T> {}

#[derive(Clone)]
pub struct JpegFrame {
    pub bytes: Bytes,
    pub seq: u64,
}

#[derive(Clone, Copy, Debug)]
pub enum CameraStatus {
    Disconnected = 0,
    Connecting = 1,
    Streaming = 2,
}

impl CameraStatus {
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

pub struct CaptureInfo {
    pub width: u32,
    pub height: u32,
    stop: Arc<AtomicBool>,
}

impl Drop for CaptureInfo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct ChosenFormat {
    width: u32,
    height: u32,
    is_mjpg: bool,
}

/// 枚举视频输入设备，返回 IMFActivate 数组 + 数量。
/// 调用方负责 CoTaskMemFree 顶层数组指针。
unsafe fn enum_devices() -> Result<(Vec<IMFActivate>, u32)> {
    let mut attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attrs, 1)?;
    let attrs = attrs.ok_or_else(|| anyhow!("MFCreateAttributes 返回空"))?;
    attrs.SetGUID(
        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    )?;

    let mut raw_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count: u32 = 0;
    MFEnumDeviceSources(&attrs, &mut raw_ptr, &mut count)?;

    // raw_ptr 指向 count 个 Option<IMFActivate>。把它们克隆出来，
    // 然后释放顶层数组（IMFActivate 自身的引用计数由 clone 维持）。
    let slice = std::slice::from_raw_parts(raw_ptr, count as usize);
    let out: Vec<IMFActivate> = slice
        .iter()
        .filter_map(|x| x.clone())
        .collect();
    CoTaskMemFree(Some(raw_ptr as *const _));
    Ok((out, count))
}

pub fn spawn_capture(
    camera_index: u32,
    req_width: u32,
    req_height: u32,
    req_fps: u32,
    tx: broadcast::Sender<JpegFrame>,
    last_frame_time: Arc<AtomicI64>,
) -> Result<CaptureInfo> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, 0).ok().ok_or_else(|| anyhow!("MFStartup 失败"))?;

        let (activates, count) = enum_devices()?;
        if count == 0 {
            return Err(anyhow!("系统未检测到任何视频输入设备"));
        }
        if camera_index >= count {
            return Err(anyhow!(
                "设备 index={} 不存在（共 {} 个设备）",
                camera_index,
                count
            ));
        }
        let activate = &activates[camera_index as usize];

        let source: IMFMediaSource = activate
            .ActivateObject::<IMFMediaSource>()
            .map_err(|e| anyhow!("ActivateObject 失败(index={}): {e}", camera_index))?;
        let reader: IMFSourceReader =
            MFCreateSourceReaderFromMediaSource(&source, None)
                .map_err(|e| anyhow!("MFCreateSourceReaderFromMediaSource 失败: {e}"))?;

        reader
            .SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)
            .ok();

        let chosen = choose_native_format(&reader, req_width, req_height)?;
        let cap_width = chosen.width;
        let cap_height = chosen.height;
        tracing::info!(
            "摄像头已打开: index={} 实际 {}x{} 格式={}{}(请求 {}x{}@{}fps)",
            camera_index,
            cap_width,
            cap_height,
            if chosen.is_mjpg { "MJPG" } else { "RGB32" },
            if chosen.is_mjpg { "(JPEG 透传) " } else { "(转JPEG) " },
            req_width,
            req_height,
            req_fps
        );

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let reader = SendCom(reader);
        let source = SendCom(source);
        let _ = thread::Builder::new()
            .name(format!("mf-capture-{}", camera_index))
            .spawn({
                let tx = tx.clone();
                let last_frame_time = last_frame_time.clone();
                move || capture_loop(reader, source, chosen, tx, last_frame_time, stop_clone)
            })
            .map_err(|e| anyhow!("创建采集线程失败: {e}"))?;

        Ok(CaptureInfo {
            width: cap_width,
            height: cap_height,
            stop,
        })
    }
}

fn capture_loop(
    reader: SendCom<IMFSourceReader>,
    _source: SendCom<IMFMediaSource>,
    fmt: ChosenFormat,
    tx: broadcast::Sender<JpegFrame>,
    last_frame_time: Arc<AtomicI64>,
    stop: Arc<AtomicBool>,
) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let seq = AtomicU64::new(0);
        let reader = reader.0;

        while !stop.load(Ordering::Relaxed) {
            let mut actual_stream: u32 = 0;
            let mut actual_flags: u32 = 0;
            let mut sample_ptr: Option<IMFSample> = None;

            if let Err(e) = reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                Some(&mut actual_stream),
                Some(&mut actual_flags),
                None,
                Some(&mut sample_ptr),
            ) {
                tracing::warn!("ReadSample 失败: {e}");
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }

            // MF_SOURCE_READERF_ENDOFSTREAM = 0x2
            if actual_flags & 0x2 != 0 {
                tracing::warn!("采集流结束(EOF)");
                break;
            }
            let Some(sample) = sample_ptr else { continue };

            let buffer = match sample.ConvertToContiguousBuffer() {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("ConvertToContiguousBuffer 失败: {e}");
                    continue;
                }
            };
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut max_len: u32 = 0;
            let mut cur_len: u32 = 0;
            if let Err(e) = buffer.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len)) {
                tracing::warn!("IMFMediaBuffer::Lock 失败: {e}");
                continue;
            }
            let data_slice = std::slice::from_raw_parts(ptr, cur_len as usize);
            let jpeg_result: Result<Vec<u8>> = if fmt.is_mjpg {
                Ok(data_slice.to_vec())
            } else {
                encode_rgb32_to_jpeg(data_slice, fmt.width, fmt.height, 80)
            };
            let _ = buffer.Unlock().ok();

            match jpeg_result {
                Ok(jpeg) => {
                    let s = seq.fetch_add(1, Ordering::Relaxed);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    last_frame_time.store(now, Ordering::Relaxed);
                    let _ = tx.send(JpegFrame {
                        bytes: Bytes::from(jpeg),
                        seq: s,
                    });
                }
                Err(e) => tracing::warn!("帧编码失败: {e}"),
            }
        }
    }
}

fn choose_native_format(
    reader: &IMFSourceReader,
    req_width: u32,
    req_height: u32,
) -> Result<ChosenFormat> {
    unsafe {
        let mut best_mjpg: Option<(u32, u32)> = None;
        let mut best_rgb: Option<(u32, u32)> = None;
        // 记下选中的 IMFMediaType 以便 SetCurrentMediaType（类型是 IMFAttributes 的子接口）
        let mut chosen_mt: Option<windows::Win32::Media::MediaFoundation::IMFMediaType> = None;

        let mut index: u32 = 0;
        loop {
            let mt = match reader.GetNativeMediaType(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                index,
            ) {
                Ok(t) => t,
                Err(_) => break,
            };
            index += 1;

            let subtype = mt.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default();
            let packed = mt.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            let w = (packed >> 32) as u32;
            let h = (packed & 0xFFFF_FFFF) as u32;

            if subtype == MFVideoFormat_MJPG {
                if better_fit(&best_mjpg, w, h, req_width, req_height) {
                    best_mjpg = Some((w, h));
                    chosen_mt = Some(mt);
                }
            } else if subtype == MFVideoFormat_RGB32 {
                if better_fit(&best_rgb, w, h, req_width, req_height) {
                    best_rgb = Some((w, h));
                    chosen_mt = Some(mt);
                }
            }
        }

        let (w, h, is_mjpg) = if let Some((w, h)) = best_mjpg {
            (w, h, true)
        } else if let Some((w, h)) = best_rgb {
            (w, h, false)
        } else {
            return Err(anyhow!("设备未提供 MJPG 或 RGB32 格式（可能是采集卡兼容问题）"));
        };
        let mt = chosen_mt.ok_or_else(|| anyhow!("内部错误：chosen_mt 为空"))?;

        reader
            .SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &mt)
            .map_err(|e| anyhow!("SetCurrentMediaType 失败: {e}"))?;

        Ok(ChosenFormat {
            width: w,
            height: h,
            is_mjpg,
        })
    }
}

fn better_fit(best: &Option<(u32, u32)>, w: u32, h: u32, rw: u32, rh: u32) -> bool {
    match best {
        None => true,
        Some((bw, bh)) => {
            let dist = (w as i64 - rw as i64).abs() + (h as i64 - rh as i64).abs();
            let bdist = (*bw as i64 - rw as i64).abs() + (*bh as i64 - rh as i64).abs();
            dist < bdist
        }
    }
}

fn encode_rgb32_to_jpeg(bgra: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>> {
    use image::{codecs::jpeg::JpegEncoder, ColorType, ImageEncoder};
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for px in bgra.chunks_exact(4) {
        rgb.push(px[2]); // R
        rgb.push(px[1]); // G
        rgb.push(px[0]); // B
    }
    let mut buf = Vec::with_capacity((width * height / 4) as usize);
    let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    encoder.write_image(&rgb, width, height, ColorType::Rgb8.into())?;
    Ok(buf)
}

pub fn list_cameras() -> Vec<CameraInfo> {
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            // 可能已初始化，继续
        }
        if MFStartup(MF_VERSION, 0).is_err() {
            return vec![];
        }
        let (activates, _count) = match enum_devices() {
            Ok(x) => x,
            Err(_) => return vec![],
        };

        let mut out = Vec::new();
        for (i, act) in activates.iter().enumerate() {
            let mut pwstr = windows::core::PWSTR::null();
            let mut cch: u32 = 0;
            let name = if act
                .GetAllocatedString(
                    &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
                    &mut pwstr,
                    &mut cch,
                )
                .is_ok()
            {
                let s = pwstr.to_string().unwrap_or_default();
                CoTaskMemFree(Some(pwstr.as_ptr() as *const _));
                s
            } else {
                String::new()
            };
            out.push(CameraInfo {
                index: i as u32,
                name,
                description: "Media Foundation".to_string(),
            });
        }
        // 抑制未用警告（PCWSTR 在后续扩展会用上）
        let _ = PCWSTR::null;
        out
    }
}

#[derive(serde::Serialize, Clone)]
pub struct CameraInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
}
