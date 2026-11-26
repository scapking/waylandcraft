use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pipewire as pw;
use pw::prelude::*;

static CAPTURE_FRAMES: std::sync::Mutex<Option<Arc<Mutex<Option<FrameData>>>>> =
    std::sync::Mutex::new(None);

#[derive(Clone)]
pub struct FrameData {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// 调用 gdbus call 并返回 stdout
fn gdbus_call(args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("gdbus")
        .args(args)
        .env("DBUS_SESSION_BUS_ADDRESS",
             std::env::var("DBUS_SESSION_BUS_ADDRESS")
                 .unwrap_or_else(|_| format!("unix:path=/run/user/{}/bus", unsafe { libc::getuid() })))
        .output()
        .map_err(|e| format!("gdbus: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gdbus failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// 等待 D-Bus Response 信号（通过 gdbus monitor）
///
/// 注意：`gdbus monitor` 是持续监听的，收到目标信号后不会自行退出。
/// 早期实现用 `timeout` 包住它，导致每次调用都要等满超时（CreateSession 10s
/// + SelectSources 10s + Start 60s = 至少 80 秒）才能返回，表现为"捕获不可用"。
/// 这里改为 `gdbus monitor | grep -m1 Response`：grep 匹配到第一个 Response 就
/// 退出，管道关闭后 gdbus monitor 因 SIGPIPE 退出，timeout 只是兜底。
fn wait_portal_response(request_path: &str, timeout_secs: u64) -> Result<(u32, String), String> {
    let shell_cmd = format!(
        "timeout {} gdbus monitor --session --dest org.freedesktop.portal.Desktop --object-path '{}' | grep -m1 -E 'Response'",
        timeout_secs,
        request_path
    );

    let output = std::process::Command::new("sh")
        .args(&["-c", &shell_cmd])
        .env("DBUS_SESSION_BUS_ADDRESS",
             std::env::var("DBUS_SESSION_BUS_ADDRESS")
                 .unwrap_or_else(|_| format!("unix:path=/run/user/{}/bus", unsafe { libc::getuid() })))
        .output()
        .map_err(|e| format!("monitor: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap_or("").trim();

    if line.contains("Response") {
        // 解析: Response (uint32 0, @a{sv} {...})
        // code 是行内第一个数字（uint32 后的值）
        let code = line
            .split(|c: char| !c.is_ascii_digit())
            .find(|s| !s.is_empty())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);

        return Ok((code, line.to_string()));
    }

    Err(format!("no Response signal received (timeout={}s)", timeout_secs))
}

/// 从 CreateSession 的 Response 信号中提取 session_handle
/// Response 行形如:
///   ... org.freedesktop.portal.Request.Response (uint32 0, {'session_handle': <'/org/freedesktop/portal/desktop/session/1_xxx/wcs1'>, ...})
fn extract_session_handle(response: &str) -> Option<String> {
    if let Some(pos) = response.find("session_handle") {
        let rest = &response[pos..];
        if let Some(q1) = rest.find('\'') {
            let after = &rest[q1 + 1..];
            if let Some(q2) = after.find('\'') {
                let path = &after[..q2];
                if path.starts_with("/org/freedesktop/portal/desktop/session/") {
                    return Some(path.to_string());
                }
            }
        }
    }
    None
}

/// 通过 XDG Desktop Portal ScreenCast 启动捕获
/// 返回 PipeWire 节点 ID
pub fn start_portal_capture() -> Result<u32, String> {
    // 1. CreateSession
    eprintln!("[portal] CreateSession...");
    let out = gdbus_call(&[
        "call", "--session",
        "--dest", "org.freedesktop.portal.Desktop",
        "--object-path", "/org/freedesktop/portal/desktop",
        "--method", "org.freedesktop.portal.ScreenCast.CreateSession",
        "{'session_handle_token': <'wcs1'>, 'handle_token': <'wcr1'>}",
    ])?;
    // 返回: (objectpath '/org/.../wcr1',)
    let req1 = extract_object_path(&out)?;
    eprintln!("[portal] CreateSession req: {}", req1);

    let (code, create_response) = wait_portal_response(&req1, 10)?;
    if code != 0 { return Err(format!("CreateSession failed: {}", code)); }
    // session_handle 从 Response 中提取，而不是靠固定 token 硬编码推导
    // （硬编码 replace("wcr1","wcs1") 依赖 handle_token 恰好是 wcr1/wcs1，
    //   一旦 portal 返回不同 token 就会得到错误路径）
    let session = extract_session_handle(&create_response)
        .ok_or("CreateSession: cannot extract session_handle")?;
    eprintln!("[portal] Session: {}", session);

    // 2. SelectSources
    eprintln!("[portal] SelectSources...");
    let out = gdbus_call(&[
        "call", "--session",
        "--dest", "org.freedesktop.portal.Desktop",
        "--object-path", "/org/freedesktop/portal/desktop",
        "--method", "org.freedesktop.portal.ScreenCast.SelectSources",
        &session,
        "{'handle_token': <'wcr2'>, 'types': <uint32 2>, 'multiple': <false>}",
    ])?;
    let req2 = extract_object_path(&out)?;
    let (code, _) = wait_portal_response(&req2, 10)?;
    if code != 0 { return Err(format!("SelectSources failed: {}", code)); }

    // 3. Start (用户需要在弹窗中确认)
    eprintln!("[portal] Start (请在弹窗中选择窗口并点击分享)...");
    let out = gdbus_call(&[
        "call", "--session",
        "--dest", "org.freedesktop.portal.Desktop",
        "--object-path", "/org/freedesktop/portal/desktop",
        "--method", "org.freedesktop.portal.ScreenCast.Start",
        &session,
        "''",
        "{'handle_token': <'wcr3'>}",
    ])?;
    let req3 = extract_object_path(&out)?;
    eprintln!("[portal] Start req: {}", req3);

    let (code, response) = wait_portal_response(&req3, 60)?;
    if code != 0 { return Err(format!("Start failed: {}", code)); }

    // 4. 从 Response 中提取 PipeWire 节点 ID
    let node_id = extract_node_id_from_response(&response)?;
    eprintln!("[portal] PipeWire node: {}", node_id);

    Ok(node_id)
}

fn extract_object_path(s: &str) -> Result<String, String> {
    // 格式: (objectpath '/org/freedesktop/portal/desktop/request/1_109/wr1',)
    let start = s.find('\'').ok_or("no quote")?;
    let rest = &s[start + 1..];
    let end = rest.find('\'').ok_or("no closing quote")?;
    Ok(rest[..end].to_string())
}

fn extract_node_id_from_response(response: &str) -> Result<u32, String> {
    // Response 格式: Response(uint32 0, @a{sv} {...streams: [(uint32 NODE_ID, {...})]...})
    // 查找 "streams" 后面的第一个数字——那就是 node_id。
    // 之前用 "id > 100" 过滤，会漏掉小于 100 的节点 ID（例如 42），改为取第一个数字。
    if let Some(pos) = response.find("streams") {
        let rest = &response[pos..];
        for part in rest.split(|c: char| !c.is_ascii_digit()) {
            if !part.is_empty() {
                if let Ok(id) = part.parse::<u32>() {
                    // 节点 ID 通常 > 0；排除掉 0 避免误抓其他 0
                    if id > 0 {
                        return Ok(id);
                    }
                }
            }
        }
    }

    Err(format!("cannot extract node ID from: {}", &response[..response.len().min(500)]))
}

/// 把 PipeWire 常见的 BGRx/BGRA/RGBA 帧统一转换为 RGBA（每像素4字节，R,G,B,A）
fn convert_to_rgba(src: &[u8], fmt: pw::spa::param::video::VideoFormat) -> Vec<u8> {
    // 宽松的 BGR 判断：VideoFormat 枚举里 BGRx/BGRA 都是 BGR 序
    let bgr = matches!(
        fmt,
        pw::spa::param::video::VideoFormat::BGRx
            | pw::spa::param::video::VideoFormat::BGRA
    );

    let mut out = Vec::with_capacity(src.len());
    // 按 4 字节一组处理；末尾不足 4 字节的直接原样拷贝
    let mut i = 0;
    while i + 4 <= src.len() {
        let b = src[i];
        let g = src[i + 1];
        let r = src[i + 2];
        let a = src[i + 3];
        if bgr {
            out.extend_from_slice(&[r, g, b, a]);
        } else {
            out.extend_from_slice(&[b, g, r, a]);
        }
        i += 4;
    }
    out.extend_from_slice(&src[i..]);
    out
}

/// 连接 PipeWire 节点并读取帧
pub fn start_pw_stream(node_id: u32) -> Result<(), String> {
    let frame_data = Arc::new(Mutex::new(None));
    {
        let mut guard = CAPTURE_FRAMES.lock().map_err(|e| format!("lock: {}", e))?;
        *guard = Some(frame_data.clone());
    }

    std::thread::Builder::new()
        .name("pw-stream".to_string())
        .spawn(move || {
            if let Err(e) = pw_stream_loop(node_id, frame_data) {
                eprintln!("[portal] PW error: {}", e);
            }
        })
        .map_err(|e| format!("spawn: {}", e))?;

    Ok(())
}

fn pw_stream_loop(node_id: u32, frame_data: Arc<Mutex<Option<FrameData>>>) -> Result<(), String> {
    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None).map_err(|e| format!("mainloop: {}", e))?;
    let context = pw::context::Context::new(&mainloop).map_err(|e| format!("context: {}", e))?;
    let core = context.connect(None).map_err(|e| format!("connect: {}", e))?;

    let stream = pw::stream::Stream::new(
        &core,
        "wc-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    ).map_err(|e| format!("stream: {}", e))?;

    let frame_ref = frame_data;
    let mut video_info: pw::spa::param::video::VideoInfoRaw = Default::default();

    let _listener = stream
        .add_local_listener_with_user_data(&mut video_info)
        .state_changed(|_, _, old, new| {
            eprintln!("[portal] PW state: {:?} -> {:?}", old, new);
        })
        .param_changed(|_stream, user_data, _id, param| {
            let Some(param) = param else { return; };
            if let Ok((media_type, media_subtype)) = pw::spa::param::format_utils::parse_format(param) {
                if media_type == pw::spa::param::format::MediaType::Video
                    && media_subtype == pw::spa::param::format::MediaSubtype::Raw
                {
                    // P1-3：pw-stream 线程上的 panic 会让捕获线程静默死亡
                    //（Java 侧以为还在录屏）——格式解析失败只记日志跳过。
                    if let Err(e) = user_data.parse(param) {
                        eprintln!("[portal] parse video format failed: {}", e);
                    } else {
                        eprintln!("[portal] Video: {}x{}", user_data.size().width, user_data.size().height);
                    }
                }
            }
        })
        .process(move |stream, user_data| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if !datas.is_empty() {
                    let data = &mut datas[0];
                    let size = data.chunk().size() as usize;
                    if size > 0 {
                        if let Some(slice) = data.data() {
                            let w = user_data.size().width;
                            let h = user_data.size().height;
                            // 统一转换为 RGBA（PipeWire 可能给 BGRx/BGRA/RGBA）
                            let fmt = user_data.format();
                            let src = &slice[..size.min(slice.len())];
                            let data = convert_to_rgba(src, fmt);
                            // P1-3：锁中毒（其他持有者 panic）时跳过本帧而不是连锁 panic
                            if let Ok(mut frame) = frame_ref.lock() {
                                *frame = Some(FrameData {
                                    data,
                                    width: w,
                                    height: h,
                                });
                            }
                        }
                    }
                }
            }
        })
        .register()
        .map_err(|e| format!("register: {}", e))?;

    let obj = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::BGRA,
            pw::spa::param::video::VideoFormat::RGBA,
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle { width: 1920, height: 1080 },
            pw::spa::utils::Rectangle { width: 1, height: 1 },
            pw::spa::utils::Rectangle { width: 7680, height: 4320 }
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            pw::spa::utils::Fraction { num: 30, denom: 1 },
            pw::spa::utils::Fraction { num: 0, denom: 1 },
            pw::spa::utils::Fraction { num: 120, denom: 1 }
        ),
    );

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .map_err(|e| format!("serialize: {}", e))?
    .0
    .into_inner();

    let mut params = [pw::spa::pod::Pod::from_bytes(&values).ok_or("pod from bytes")?];

    stream.connect(
        pw::spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    ).map_err(|e| format!("connect: {}", e))?;

    eprintln!("[portal] PW stream connected to node {}", node_id);
    mainloop.run();

    Ok(())
}

pub fn get_frame() -> Option<FrameData> {
    let guard = CAPTURE_FRAMES.lock().ok()?;
    guard.as_ref().and_then(|arc| arc.lock().ok()?.clone())
}
