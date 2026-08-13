//! H.264 视频编码（方案 A 的视频部分）。
//!
//! 发送端（共享者）每抓一帧 RGBA，调 `encode_rgba` 得到 H.264 Annex-B NAL
//! 字节流，交给 Java 侧封包发给观看端；观看端用 JCodec（纯 Java）解码，
//! 无需 native。POC 已验证 openh264 编码 ↔ JCodec 解码码流互认。
//!
//! 每个窗口（windowHandle）一个独立编码器实例（帧间预测有状态，分辨率
//! 不同不能复用）。编码器懒创建，尺寸变化由 openh264 在 `encode` 时自动
//! reinit（reinit 后自动强制 IDR）。
//!
//! 关键约束：OpenH264 的 YUV420 要求宽高为偶数（`RgbaSliceU8::new` 会
//! assert）。这里把宽高向下取偶并逐行紧凑拷贝，避免奇数窗口尺寸 panic。

use std::collections::HashMap;
use std::sync::Mutex;

use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, UsageType};
use openh264::formats::{RgbaSliceU8, YUVBuffer};
use openh264::OpenH264API;

/// 目标码率（bps）。8 Mbps 对 1080p 屏幕内容（游戏 + UI 文字）是合理起点，
/// 后续可按分辨率/网络条件动态调整。
const DEFAULT_BITRATE_BPS: u32 = 8_000_000;
/// 编码器内部最大帧率（Hz），只影响码率控制，不强制丢帧（丢帧由 Java 侧
/// FrameRateController 控制）。
const DEFAULT_FPS: f32 = 30.0;
/// 周期性 IDR 间隔（帧）。屏幕内容变化少可放宽，但网络丢包/新观众加入
/// 需要尽快拿到关键帧恢复画面，60 帧（2s @30fps）是折中。
const DEFAULT_INTRA_PERIOD: u32 = 60;
/// OpenH264 编码线程数。
const DEFAULT_THREADS: u16 = 2;

/// 全局编码器表：windowHandle → Encoder。
static ENCODERS: Mutex<Option<HashMap<u64, Encoder>>> = Mutex::new(None);

/// 把 RGBA 帧编码为 H.264 Annex-B NAL 字节流。
///
/// * `window_handle` — 窗口句柄（区分不同窗口的编码器状态）
/// * `rgba`           — 逐像素 RGBA（每像素 4 字节，R,G,B,A 顺序）
/// * `width`/`height` — 帧尺寸（可奇数，内部向下取偶）
/// * `force_keyframe` — 强制下一帧为 IDR（新观众加入/关键帧刷新时置 true）
/// * `flip_y`         — true 时垂直翻转（glReadPixels 是 bottom-up，需翻转为
///                      top-down；X11 XGetImage 已是 top-down，传 false）
pub fn encode_rgba(
    window_handle: u64,
    rgba: &[u8],
    width: u32,
    height: u32,
    force_keyframe: bool,
    flip_y: bool,
) -> Result<Vec<u8>, String> {
    // 校验 + 偶数对齐（YUV420 要求宽高为偶数）
    if width == 0 || height == 0 {
        return Err(format!("[h264] invalid dimensions {}x{}", width, height));
    }
    // OpenH264 上限 3840x2160（水平）或 2160x3840（垂直）
    if width > 3840 || height > 2160 {
        return Err(format!("[h264] resolution {}x{} exceeds OpenH264 max", width, height));
    }

    let enc_w = width & !1; // floor to even
    let enc_h = height & !1;
    if enc_w == 0 || enc_h == 0 {
        return Err(format!("[h264] resolution too small {}x{}", width, height));
    }

    let expected = (enc_w as usize) * (enc_h as usize) * 4;
    if rgba.len() < width as usize * height as usize * 4 {
        return Err(format!(
            "[h264] rgba buffer too small: {} < {} ({}x{})",
            rgba.len(),
            width as usize * height as usize * 4,
            width,
            height
        ));
    }

    // 逐行紧凑拷贝（去掉奇数宽度的最后一列、奇数高度的最后一行）
    // flip_y 时按 bottom-up 顺序读（glReadPixels 从底部起），翻转为 top-down。
    let mut compact: Vec<u8> = Vec::with_capacity(expected);
    let row_bytes = enc_w as usize * 4;
    let src_stride = width as usize * 4;
    for y in 0..enc_h as usize {
        let src_y = if flip_y { height as usize - 1 - y } else { y };
        let row_start = src_y * src_stride;
        compact.extend_from_slice(&rgba[row_start..row_start + row_bytes]);
    }

    let mut guard = ENCODERS.lock().map_err(|e| format!("[h264] lock: {}", e))?;
    let map = guard.get_or_insert_with(HashMap::new);

    // 懒创建编码器
    let encoder = match map.get_mut(&window_handle) {
        Some(enc) => enc,
        None => {
            let config = EncoderConfig::new()
                .bitrate(BitRate::from_bps(DEFAULT_BITRATE_BPS))
                .max_frame_rate(FrameRate::from_hz(DEFAULT_FPS))
                .usage_type(UsageType::ScreenContentRealTime)
                .intra_frame_period(IntraFramePeriod::from_num_frames(DEFAULT_INTRA_PERIOD))
                .num_threads(DEFAULT_THREADS);
            let enc = Encoder::with_api_config(OpenH264API::from_source(), config)
                .map_err(|e| format!("[h264] create encoder: {}", e))?;
            eprintln!(
                "[h264] created encoder for window 0x{:x} (bitrate={}kbps, fps={}, intra={})",
                window_handle,
                DEFAULT_BITRATE_BPS / 1000,
                DEFAULT_FPS,
                DEFAULT_INTRA_PERIOD
            );
            map.insert(window_handle, enc);
            map.get_mut(&window_handle).unwrap()
        }
    };

    if force_keyframe {
        encoder.force_intra_frame();
    }

    let yuv = YUVBuffer::from_rgba8_source(RgbaSliceU8::new(&compact, (enc_w as usize, enc_h as usize)));
    let bitstream = encoder
        .encode(&yuv)
        .map_err(|e| format!("[h264] encode: {}", e))?;

    Ok(bitstream.to_vec())
}

/// 销毁某个窗口的编码器（停止共享时调用）。
pub fn destroy(window_handle: u64) {
    if let Ok(mut guard) = ENCODERS.lock() {
        if let Some(map) = guard.as_mut() {
            if map.remove(&window_handle).is_some() {
                eprintln!("[h264] destroyed encoder for window 0x{:x}", window_handle);
            }
        }
    }
}

/// 销毁全部编码器（进程退出 / 全部停止共享时兜底）。
pub fn destroy_all() {
    if let Ok(mut guard) = ENCODERS.lock() {
        if let Some(map) = guard.as_mut() {
            let count = map.len();
            map.clear();
            if count > 0 {
                eprintln!("[h264] destroyed all {} encoder(s)", count);
            }
        }
    }
}
