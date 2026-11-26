//! portal_capture 的非 Linux/Android 平台替身。
//!
//! 背景：XDG Desktop Portal + PipeWire 只在 Linux/Android 存在；为让
//! `cargo check --target *-apple-darwin`（以及后续任何 mac native 构建）
//! 不把 pipewire 相关模块编进 macOS 产物，lib.rs 按 target_os 在
//! `portal_capture.rs` 与本文件之间二选一（bridge.rs 调用点因此零改动）。
//!
//! 语义：本平台上没有任何 portal 会话可启动——start 一律返回 Err，
//! get_frame 一律 None。绝不假成功。

pub struct FrameData {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn start_portal_capture() -> Result<u32, String> {
    Err("portal capture is only supported on Linux/Android".to_string())
}

pub fn start_pw_stream(_node_id: u32) -> Result<(), String> {
    Err("portal capture is only supported on Linux/Android".to_string())
}

pub fn get_frame() -> Option<FrameData> {
    None
}
