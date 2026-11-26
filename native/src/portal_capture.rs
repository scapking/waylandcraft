use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pipewire as pw;
use pw::prelude::*;

static CAPTURE_FRAMES: std::sync::Mutex<Option<Arc<Mutex<Option<FrameData>>>>> =
    std::sync::Mutex::new(None);

/// P1-4：捕获生命周期状态。
///
/// 旧实现 start_pw_stream spawn 的 "pw-stream" 线程在 `mainloop.run()` 上永不返回，
/// portal_capture_stop 是空函数——每次 start/stop 泄漏 1 个 30fps 收帧的僵尸线程
/// （每帧新建 8MB+ Vec，约 250MB/s 分配，2-5% CPU，8-16MB 常驻），且 portal
/// Session 从不 Close（桌面持续显示"正在录屏"）。
/// 现在 quit 标志 + join + Session.Close + 唯一 token，stop 后资源全部归零。
struct CaptureEntry {
    quit: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    session: Option<String>,
}

static CAPTURE: Mutex<Option<CaptureEntry>> = Mutex::new(None);

/// portal handle token 序列号（同进程内单调，配合 pid 保证跨重启唯一）
static TOKEN_SEQ: AtomicU64 = AtomicU64::new(0);

fn portal_token(prefix: &str) -> String {
    let seq = TOKEN_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}{}-{}", prefix, std::process::id(), seq)
}

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
/// Response 行形如（gdbus call 与 gdbus monitor 的引号/标注风格不同）:
///   ... Response (uint32 0, {'session_handle': <'/org/freedesktop/portal/desktop/session/1_xxx/wcs1'>, ...})
///   "session_handle": objectpath "/org/freedesktop/portal/desktop/session/1_xxx/wcs1"
/// P1-4：旧实现只认"session_handle 之后第一对单引号"，在带类型标注
/// （objectpath '/org/...'）或双引号打印时拿到的是 `: <objectpath ` 之类的
/// 垃圾段而返回 None。改为扫描全行的引号片段（' 与 " 都认），取第一个
/// 以 session 路径前缀开头的——对旧格式仍然成立（超集）。
fn extract_session_handle(response: &str) -> Option<String> {
    const PREFIX: &str = "/org/freedesktop/portal/desktop/session/";
    let b = response.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        let q = b[i];
        if q == b'\'' || q == b'"' {
            // 配对同类引号（对象路径本身不含引号）
            if let Some(l) = response[i + 1..].find(q as char) {
                let token = &response[i + 1..i + 1 + l];
                if token.starts_with(PREFIX) {
                    return Some(token.to_string());
                }
                i = i + 1 + l + 1; // 跳到闭引号之后
                continue;
            }
        }
        i += 1;
    }
    None
}

/// 通过 XDG Desktop Portal ScreenCast 启动捕获
/// 返回 PipeWire 节点 ID
pub fn start_portal_capture() -> Result<u32, String> {
    // P1-4：先回收上一轮残留（Java 侧漏 stop / start 失败半途），
    // 保证"一次 start 对应一次会话+一个线程"，不再叠加泄漏。
    stop_portal_capture();

    // 1. CreateSession
    // P1-4：token 固定 wcs1/wcr1 → 第二次 start 被 portal 以重复 token 拒绝。
    // 改为 pid+自增序列，进程内单调、重启后仍唯一。
    let tok_session = portal_token("wcs");
    let tok_request = portal_token("wcr");
    eprintln!("[portal] CreateSession (token {tok_session}/{tok_request})...");
    let session_opts = format!(
        "{{'session_handle_token': <'{}'>, 'handle_token': <'{}'>}}",
        tok_session, tok_request
    );
    let out = gdbus_call(&[
        "call", "--session",
        "--dest", "org.freedesktop.portal.Desktop",
        "--object-path", "/org/freedesktop/portal/desktop",
        "--method", "org.freedesktop.portal.ScreenCast.CreateSession",
        &session_opts,
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

    // 会话已建立——立即登记，此后任何失败路径都能被 stop_portal_capture 关闭
    *CAPTURE.lock().map_err(|e| format!("lock: {}", e))? = Some(CaptureEntry {
        quit: Arc::new(AtomicBool::new(false)),
        join: None,
        session: Some(session.clone()),
    });

    // 2. SelectSources
    eprintln!("[portal] SelectSources...");
    let select_opts = format!("{{'handle_token': <'{}'>, 'types': <uint32 2>, 'multiple': <false>}}", portal_token("wcr"));
    let out = gdbus_call(&[
        "call", "--session",
        "--dest", "org.freedesktop.portal.Desktop",
        "--object-path", "/org/freedesktop/portal/desktop",
        "--method", "org.freedesktop.portal.ScreenCast.SelectSources",
        &session,
        &select_opts,
    ])?;
    let req2 = match extract_object_path(&out) {
        Ok(p) => p,
        Err(e) => { stop_portal_capture(); return Err(e); }
    };
    let (code, _) = match wait_portal_response(&req2, 10) {
        Ok(v) => v,
        Err(e) => { stop_portal_capture(); return Err(e); }
    };
    if code != 0 {
        stop_portal_capture();
        return Err(format!("SelectSources failed: {}", code));
    }

    // 3. Start (用户需要在弹窗中确认)
    eprintln!("[portal] Start (请在弹窗中选择窗口并点击分享)...");
    let start_opts = format!("{{'handle_token': <'{}'>}}", portal_token("wcr"));
    let out = gdbus_call(&[
        "call", "--session",
        "--dest", "org.freedesktop.portal.Desktop",
        "--object-path", "/org/freedesktop/portal/desktop",
        "--method", "org.freedesktop.portal.ScreenCast.Start",
        &session,
        "''",
        &start_opts,
    ])?;
    let req3 = match extract_object_path(&out) {
        Ok(p) => p,
        Err(e) => { stop_portal_capture(); return Err(e); }
    };
    eprintln!("[portal] Start req: {}", req3);

    let (code, response) = match wait_portal_response(&req3, 60) {
        Ok(v) => v,
        Err(e) => { stop_portal_capture(); return Err(e); }
    };
    if code != 0 {
        // 用户在弹窗里点了取消/超时：会话同样必须关闭，否则"正在录屏"指示灯长亮
        stop_portal_capture();
        return Err(format!("Start failed: {}", code));
    }

    // 4. 从 Response 中提取 PipeWire 节点 ID
    let node_id = match extract_node_id_from_response(&response) {
        Ok(id) => id,
        Err(e) => { stop_portal_capture(); return Err(e); }
    };
    eprintln!("[portal] PipeWire node: {}", node_id);

    Ok(node_id)
}

/// P1-4：关闭捕获——退线程、清帧槽、Close portal Session。幂等：未启动/已停止时空操作。
///
/// 由 portal_capture_stop（Java stop 路径）与 start 的失败/重入路径调用。
/// join 有界（pw-stream 线程每 ≤50ms 检查一次 quit 标志），在 Java 调用线程上等待可接受。
pub fn stop_portal_capture() {
    let entry = match CAPTURE.lock() {
        Ok(mut g) => g.take(),
        Err(_) => None,
    };
    let Some(mut entry) = entry else { return };

    // 1. 让 pw-stream 线程退出 iterate 循环
    entry.quit.store(true, Ordering::Relaxed);
    // 2. join：stream/context/core/mainloop 都在该线程上释放（pipewire 对象
    //    必须在自己的 loop 线程上销毁；join 保证 stop 返回时资源已归还）
    if let Some(join) = entry.join.take() {
        if join.join().is_err() {
            eprintln!("[portal] pw-stream thread panicked while stopping");
        }
    }
    // 3. 帧槽清零：get_frame() 之后立即返回 None，不会再拷旧帧
    if let Ok(mut g) = CAPTURE_FRAMES.lock() {
        *g = None;
    }
    // 4. Session.Close：撤掉桌面的"正在录屏"指示并释放 portal 侧资源
    if let Some(session) = entry.session.take() {
        close_session(&session);
    }
}

fn close_session(session_path: &str) {
    match gdbus_call(&[
        "call", "--session",
        "--dest", "org.freedesktop.portal.Desktop",
        "--object-path", session_path,
        "--method", "org.freedesktop.portal.Session.Close",
    ]) {
        Ok(_) => eprintln!("[portal] Session closed: {}", session_path),
        Err(e) => eprintln!("[portal] Session.Close {} failed: {}", session_path, e),
    }
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
    // 取 "streams" 之后的第一个独立数字作为 node_id。
    // P1-4 加固：旧实现按"非数字分段"取第一个数字串——带类型标注的打印
    // （[(uint32 42, ...)]）会先撞上 "uint32" 里的 32，连到错误节点。
    // 现在跳过紧跟在字母后面的数字段（uintNN/intNN 等类型名的一部分），
    // 仍然取后面第一个 >0 的数；对无标注格式 [(42, ...)] 行为不变。
    if let Some(pos) = response.find("streams") {
        let rest = &response[pos..];
        let b = rest.as_bytes();
        let mut i = 0usize;
        while i < b.len() {
            if b[i].is_ascii_digit() {
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                // 前一个字符是字母 → 是类型名（uint32 等）的一部分，跳过
                if start > 0 && (b[start - 1].is_ascii_alphabetic() || b[start - 1] == b'_') {
                    continue;
                }
                if let Ok(id) = rest[start..i].parse::<u32>() {
                    if id > 0 {
                        return Ok(id);
                    }
                }
            } else {
                i += 1;
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

    // P1-4：线程句柄登记进 CAPTURE，stop 时 quit 标志 + join，线程可回收
    let quit = Arc::new(AtomicBool::new(false));
    let quit_thread = quit.clone();
    let join = std::thread::Builder::new()
        .name("pw-stream".to_string())
        .spawn(move || {
            if let Err(e) = pw_stream_loop(node_id, frame_data, quit_thread) {
                eprintln!("[portal] PW error: {}", e);
            }
        })
        .map_err(|e| format!("spawn: {}", e))?;

    let mut guard = CAPTURE.lock().map_err(|e| format!("lock: {}", e))?;
    match guard.as_mut() {
        // 正常路径：start_portal_capture 已建条目（含 session），补线程句柄
        Some(entry) => {
            entry.quit = quit;
            entry.join = Some(join);
        }
        // 乱序调用防御（只有 start_pw_stream 没有 start_portal_capture）：
        // 也登记进 CAPTURE，保证 stop 能收线程
        None => {
            *guard = Some(CaptureEntry { quit, join: Some(join), session: None });
        }
    }

    Ok(())
}

fn pw_stream_loop(
    node_id: u32,
    frame_data: Arc<Mutex<Option<FrameData>>>,
    quit: Arc<AtomicBool>,
) -> Result<(), String> {
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
    // P1-4：mainloop.run() 永不返回（线程只能随进程退出而死）——换成带 50ms
    // 超时的 iterate 轮询 quit 标志。视频流以 30fps 产生事件，iterate 立即返回，
    // 无事件时最多睡 50ms；stop 置标志后线程 ≤50ms 内退出并在本线程释放
    // stream/context/core/mainloop（pipewire 对象须在自身 loop 线程销毁）。
    while !quit.load(Ordering::Relaxed) {
        mainloop.loop_().iterate(std::time::Duration::from_millis(50));
    }
    eprintln!("[portal] pw-stream loop exit (node {})", node_id);

    Ok(())
}

pub fn get_frame() -> Option<FrameData> {
    let guard = CAPTURE_FRAMES.lock().ok()?;
    guard.as_ref().and_then(|arc| arc.lock().ok()?.clone())
}

#[cfg(test)]
mod portal_tests {
    use super::*;

    #[test]
    fn portal_tokens_are_unique_and_valid_dbus_tokens() {
        // P1-4：固定 wcs1/wcr1 会让第二次 start 被 portal 以重复 token 拒绝。
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let a = portal_token("wcs");
            let b = portal_token("wcr");
            assert!(seen.insert(a.clone()) && seen.insert(b.clone()), "token 重复: {a}/{b}");
            // D-Bus object-path element 合法字符集（[A-Za-z0-9_] 为安全子集，'-' 也允许）
            assert!(
                a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "非法 token 字符: {a}"
            );
        }
    }

    #[test]
    fn extract_session_handles_quoted_and_annotated_forms() {
        // gdbus call 风格：'...' 直接包裹
        let line = "Response (uint32 0, {'session_handle': <'/org/freedesktop/portal/desktop/session/1_42/wcs12345-7'>, })";
        assert_eq!(
            extract_session_handle(line).as_deref(),
            Some("/org/freedesktop/portal/desktop/session/1_42/wcs12345-7")
        );
        // gdbus monitor 风格："..." + objectpath 类型标注
        let line2 = "  \"session_handle\": objectpath \"/org/freedesktop/portal/desktop/session/1_7/wcs99-3\"\n";
        assert_eq!(
            extract_session_handle(line2).as_deref(),
            Some("/org/freedesktop/portal/desktop/session/1_7/wcs99-3")
        );
        // 混入 request token 干扰也不误取
        let line3 = "Response (uint32 0, {'x': <'wcs5-1'>, 'session_handle': <'/org/freedesktop/portal/desktop/session/1_9/wcs5-2'>})";
        assert_eq!(
            extract_session_handle(line3).as_deref(),
            Some("/org/freedesktop/portal/desktop/session/1_9/wcs5-2")
        );
        assert_eq!(extract_session_handle("Response (uint32 1, {})"), None);
    }

    #[test]
    fn extract_node_id_skips_type_annotation_digits() {
        // 带类型标注：uint32 的 "32" 是类型名的一部分，不能当 node id
        let annotated = "Response (uint32 0, {'streams': [(uint32 1899, {'node.id': <1899>})], })";
        assert_eq!(extract_node_id_from_response(annotated), Ok(1899));
        // 无标注格式行为不变
        let plain = "Response (0, {'streams': [(42, {})], })";
        assert_eq!(extract_node_id_from_response(plain), Ok(42));
        assert!(extract_node_id_from_response("nothing here").is_err());
    }

    #[test]
    fn stop_without_start_is_safe_noop() {
        // P1-4：stop 幂等——从未 start（CAPTURE 空）时不得 panic、不得发起 D-Bus 调用
        stop_portal_capture();
        stop_portal_capture();
        assert!(CAPTURE.lock().unwrap().is_none());
    }
}
