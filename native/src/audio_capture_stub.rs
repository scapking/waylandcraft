//! audio_capture 的非 Linux/Android 平台替身（同 portal_capture_stub 的门控理由）。
//!
//! 全部入口早退：start 返回 Err、poll 返回 None、status/brief 如实报"本平台不支持"。
//! 绝不假成功，也绝不启动任何线程。

pub fn start_audio_capture(_pid: u32) -> Result<(), String> {
    Err("audio capture is only supported on Linux/Android".to_string())
}

pub fn stop_audio_capture() {}

pub fn poll_audio_capture() -> Option<Vec<u8>> {
    None
}

pub fn get_audio_capture_status() -> String {
    "unsupported platform".to_string()
}

pub enum CaptureBrief {
    Idle(String),
    PerApp(String),
    MonitorFallback(String),
}

pub fn audio_capture_brief() -> CaptureBrief {
    CaptureBrief::Idle("audio capture unsupported on this platform".to_string())
}
