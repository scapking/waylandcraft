// 按进程捕获 PipeWire 音频（"只要共享窗口的声音"）
//
// 思路：PipeWire 音频节点（Stream/Output/Audio）带 app.process.id（进程 PID）。
// 窗口 → 进程 PID（X11 _NET_WM_PID / app_id 匹配）→ 找到该进程的音频输出节点 →
// 创建我们自己的 capture stream（Direction::Input，不 AUTOCONNECT → 产生裸 input 端口），
// 再用 pw-link 把目标节点的 output 端口强制 link 到我们的 input 端口，
// 从而只捕获该进程的声音（不影响它到系统 sink 的原有连接）。
//
// 已知局限：单进程多窗口应用（如 Firefox 多标签）的声音按进程合并 ——
// PipeWire 层面无法细分到窗口，这是粒度极限。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pipewire as pw;
use pw::prelude::*;

/// [audio] 日志宏：同时写 stderr 和 audio 日志文件（bridge::audio_log_write）。
macro_rules! audio_log {
    ($($arg:tt)*) => {
        crate::bridge::audio_log_write(&format!($($arg)*))
    };
}

/// 全局捕获状态（native 侧单例，一次只允许一个音频捕获会话）
static AUDIO_CAPTURE: Mutex<Option<Arc<Mutex<AudioCaptureState>>>> = Mutex::new(None);

/// 捕获状态：累积 PCM 缓冲
pub struct AudioCaptureState {
    pub pcm: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
    pub active: bool,
    /// 保活 capture stream（drop 会导致 PipeWire 节点消失、link 断开）
    pub _stream: Option<pw::stream::Stream>,
    /// 保活 mainloop：后台线程跑 run()，stop 时先 quit 再随 state drop
    pub _mainloop: Option<pw::main_loop::MainLoop>,
    /// 保活 process 回调注册句柄：drop 会 unregister，process 不再被调用。
    /// 这是音频"实际不可用"的根因——之前是函数局部变量，返回即失效。
    pub _listener: Option<pw::stream::StreamListener<()>>,
    /// 诊断计数：process 回调触发次数 / 累计捕获字节
    pub capture_events: u64,
    pub total_bytes: u64,
    /// 全链路状态（JSON 字符串，供 Java audioCaptureStatus 查询）
    pub pid: u32,
    /// 方案A：捕获目标 = 系统默认 sink（monitor 全捕获）
    pub sink_node: u32,
    pub sink_name: String,
    pub stream_node: u32,
    pub linked: bool,
    pub last_error: Option<String>,
}

// libpipewire 的 pw_stream / pw_core 内部自带线程安全（有锁），Rust 绑定
// 因持有 NonNull + Rc 引用计数而保守地标记 !Send。实际使用模式：process
// 回调在 mainloop 线程访问 stream，stop 时先置 active=false、再由持有方 drop，
// 与 libpipewire 的线程安全模型一致，因此这里显式声明 Send 是安全的。
unsafe impl Send for AudioCaptureState {}

/// pw_main_loop 指针的 Send 包装：libpipewire 允许跨线程 quit/run
/// mainloop（标准用法），Rust 绑定因 NonNull 保守标记 !Send。
struct MainLoopPtr(*mut pipewire::sys::pw_main_loop);
unsafe impl Send for MainLoopPtr {}

/// 把 MainLoopPtr 整个 move 进后台线程再 run（跨线程调 libpipewire 标准用法）
fn mainloop_run(ml: MainLoopPtr) {
    unsafe {
        pipewire::sys::pw_main_loop_run(ml.0);
    }
}

/// 把 MainLoopPtr 整个 move 进定时线程再 quit
fn mainloop_quit(ml: MainLoopPtr) {
    unsafe {
        pipewire::sys::pw_main_loop_quit(ml.0);
    }
}

/// 枚举 PipeWire registry 中所有 node 的信息
#[derive(Default, Clone)]
struct PwTopology {
    /// node_id -> (media_class, app_pid, node_name, app_process_name)
    pub nodes: HashMap<u32, (String, u32, String, String)>,
}

/// 通过 registry 枚举一次 PipeWire 拓扑（起线程跑 mainloop，主线程等 collect_ms 后 quit）
fn enumerate_topology(collect_ms: u64) -> Result<PwTopology, String> {
    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None).map_err(|e| format!("mainloop: {}", e))?;
    let context = pw::context::Context::new(&mainloop).map_err(|e| format!("context: {}", e))?;
    let core = context.connect(None).map_err(|e| format!("connect: {}", e))?;

    let topology = Arc::new(Mutex::new(PwTopology::default()));

    let topo_ref = topology.clone();
    let _registry = core
        .get_registry()
        .map_err(|e| format!("registry: {}", e))?
        .add_listener_local()
        .global(move |global| {
            let mut t = topo_ref.lock().unwrap();
            match global.type_ {
                pw::types::ObjectType::Node => {
                    if let Some(media_class) = global.props.and_then(|p| p.get("media.class")) {
                        // 注意：PipeWire 的属性 key 是 "application.process.id" /
                        // "application.process.binary"（见 pipewire 源码 keys.h / context.c：
                        // PW_KEY_APP_PROCESS_ID = "application.process.id"）。
                        // 曾误用 "app.process.id"/"app.process.name" → PID 恒为 0，
                        // 匹配永远失败 → 这就是 v0.7 起音频一直无声的最深根因。
                        let app_pid = global
                            .props
                            .and_then(|p| p.get("application.process.id"))
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        let name = global
                            .props
                            .and_then(|p| p.get("node.name"))
                            .unwrap_or("")
                            .to_string();
                        let proc_name = global
                            .props
                            .and_then(|p| p.get("application.process.binary"))
                            .or_else(|| global.props.and_then(|p| p.get("application.name")))
                            .unwrap_or("")
                            .to_string();
                        t.nodes.insert(
                            global.id,
                            (media_class.to_string(), app_pid, name, proc_name),
                        );
                    }
                }
                _ => {}
            }
        })
        .register();

    // MainLoop 含 Rc 不可跨线程 move；libpipewire 允许从其他线程调用
    // pw_main_loop_quit（标准用法）。mainloop 在本线程存活到 run() 返回，
    // 定时线程只 sleep 后 quit 一次，指针生命周期安全。
    let ml_ptr = MainLoopPtr(mainloop.as_raw_ptr());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(collect_ms));
        // 整个 MainLoopPtr 传参 → 闭包整体捕获 Send 包装（避免按字段捕获裸指针）
        mainloop_quit(ml_ptr);
    });

    mainloop.run();

    let t = topology.lock().unwrap();
    Ok(t.clone())
}

/// 找系统默认输出 sink（方案A：monitor 全捕获，不再按 PID 匹配应用节点）。
///
/// 捕获目标 = sink 的 monitor 端口：PipeWire 每个输出设备（Audio/Sink）自带一个
/// monitor，能录到"所有正在往该设备播放的声音"。设置 target.object 指向 sink 后，
/// capture stream 用 AUTOCONNECT 自动连 monitor，无需 pw-link 手动连接。
///
/// 多 sink 时（HDMI + 耳机）选第一个；全部 dump 到日志便于核对。
fn find_default_sink(topo: &PwTopology) -> Result<(u32, String), String> {
    let mut sinks: Vec<(u32, String)> = topo
        .nodes
        .iter()
        .filter(|(_, (media_class, _, _, _))| media_class == "Audio/Sink")
        .map(|(id, (_, _, name, _))| (*id, name.clone()))
        .collect();

    // 精确 "Audio/Sink" 匹配不到时，兜底用 contains（某些版本可能带后缀）
    if sinks.is_empty() {
        sinks = topo
            .nodes
            .iter()
            .filter(|(_, (media_class, _, _, _))| media_class.contains("Audio/Sink"))
            .map(|(id, (_, _, name, _))| (*id, name.clone()))
            .collect();
    }

    for (id, name) in &sinks {
        audio_log!("[audio] candidate sink: id={} name={}", id, name);
    }

    if sinks.is_empty() {
        return Err("no Audio/Sink node found — PipeWire 可能没在跑，或没有音频输出设备".to_string());
    }

    // 选第一个（通常即默认输出）；如需精确默认 sink 可后续读 metadata default.audio.sink
    let (id, name) = sinks.remove(0);
    Ok((id, name))
}

/// 启动音频捕获（方案A）：捕获系统默认 sink 的 monitor（全系统音频），
/// 不再按 PID 匹配应用节点。窗口 pid 仅作日志记录。
pub fn start_audio_capture(pid: u32) -> Result<(), String> {
    // 确保没有正在运行的捕获
    stop_audio_capture();

    audio_log!("[audio] ===== start_audio_capture (方案A: monitor 全捕获, 窗口 pid={}) =====", pid);
    audio_log!("[audio] stage 1/4: 枚举 PipeWire 拓扑（节点）...");
    let topo = enumerate_topology(500).map_err(|e| {
        let msg = format!("[audio] 拓扑枚举失败: {}", e);
        audio_log!("{}", msg);
        msg
    })?;
    audio_log!(
        "[audio] stage 1/4: OK — {} nodes",
        topo.nodes.len()
    );
    audio_log!("[audio] stage 2/4: 选默认 sink（monitor 捕获目标）...");
    let (sink_node, sink_name) = find_default_sink(&topo).map_err(|e| {
        let msg = format!("[audio] 找不到默认 sink: {}", e);
        audio_log!("{}", msg);
        msg
    })?;
    audio_log!(
        "[audio] stage 2/4: OK — sink node={} name={}",
        sink_node, sink_name
    );

    audio_log!("[audio] stage 3/4: 创建 capture stream (target={})...", sink_name);
    pw::init();
    let mainloop = pw::main_loop::MainLoop::new(None).map_err(|e| format!("mainloop: {}", e))?;
    let context = pw::context::Context::new(&mainloop).map_err(|e| format!("context: {}", e))?;
    let core = context.connect(None).map_err(|e| format!("connect: {}", e))?;

    let state = Arc::new(Mutex::new(AudioCaptureState {
        pcm: Vec::new(),
        sample_rate: 48000,
        channels: 2,
        active: true,
        _stream: None,
        _mainloop: None,
        _listener: None,
        capture_events: 0,
        total_bytes: 0,
        pid,
        sink_node,
        sink_name: sink_name.clone(),
        stream_node: 0,
        linked: false,
        last_error: None,
    }));

    let stream = pw::stream::Stream::new(
        &core,
        "wc-audio-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
            // 方案A：target.object 指向默认 sink → PipeWire 自动连它的 monitor
            // （配合 AUTOCONNECT，无需 pw-link 手动连接）
            "target.object" => sink_name,
        },
    )
    .map_err(|e| format!("stream: {}", e))?;

    let state_ref = state.clone();
    let listener = stream
        .add_local_listener::<()>()
        .process(move |stream, _| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if !datas.is_empty() {
                    let data = &mut datas[0];
                    let size = data.chunk().size() as usize;
                    if size > 0 {
                        if let Some(slice) = data.data() {
                            let src = &slice[..size.min(slice.len())];
                            let mut log_line = None;
                            {
                                let mut s = state_ref.lock().unwrap();
                                if s.active {
                                    s.pcm.extend_from_slice(src);
                                    s.total_bytes += src.len() as u64;
                                    s.capture_events += 1;
                                    if s.capture_events == 1 {
                                        log_line = Some(format!(
                                            "[audio] FIRST capture — {} bytes (process 回调 LIVE)",
                                            src.len()
                                        ));
                                    } else if s.capture_events % 2000 == 0 {
                                        log_line = Some(format!(
                                            "[audio] capture ongoing: {} events, {} bytes",
                                            s.capture_events, s.total_bytes
                                        ));
                                    }
                                }
                            }
                            if let Some(line) = log_line {
                                audio_log!("{}", line);
                            }
                        }
                    }
                }
            }
        })
        .register()
        .map_err(|e| format!("register: {}", e))?;

    // 连接 stream（不带 AUTOCONNECT → 只建端口，不自动连任何目标）
    let format_obj = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Audio
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::AudioFormat,
            Id,
            pw::spa::param::audio::AudioFormat::S16LE
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::AudioRate,
            Int,
            48000
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::AudioChannels,
            Int,
            2
        ),
    );

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(format_obj),
    )
    .map_err(|e| format!("serialize: {}", e))?
    .0
    .into_inner();

    let mut params = [pw::spa::pod::Pod::from_bytes(&values).ok_or("pod from bytes")?];

    stream
        .connect(
            pw::spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| format!("connect: {}", e))?;

    // 等 stream 的 node global 出现在 registry 上，拿 node_id 用于诊断
    std::thread::sleep(Duration::from_millis(400));
    let stream_node = stream.node_id();
    audio_log!(
        "[audio] stage 4/4: capture stream node={}（AUTOCONNECT 已连 sink monitor）",
        stream_node
    );

    // 保存全局状态（含保活 stream + mainloop）；后台线程跑 mainloop
    // （裸指针：mainloop 本体由 state 持有，stop 时先 quit 再 drop）
    let ml_ptr = MainLoopPtr(mainloop.as_raw_ptr());
    {
        let mut guard = AUDIO_CAPTURE.lock().map_err(|e| format!("lock: {}", e))?;
        {
            let mut s = state.lock().map_err(|e| format!("state lock: {}", e))?;
            s._stream = Some(stream);
            s._mainloop = Some(mainloop);
            s._listener = Some(listener);
            s.stream_node = stream_node;
            s.linked = true;
        }
        *guard = Some(state.clone());
    }

    std::thread::Builder::new()
        .name("wc-audio-capture".to_string())
        .spawn(move || {
            mainloop_run(ml_ptr);
        })
        .map_err(|e| format!("spawn: {}", e))?;

    audio_log!("[audio] stage 4/4: OK — 捕获会话已启动（monitor 全捕获），等待 process 回调产出 PCM");
    Ok(())
}

/// 取走累积的 PCM（返回 [sampleRate(4), channels(4), pcm...]）
pub fn poll_audio_capture() -> Option<Vec<u8>> {
    let guard = AUDIO_CAPTURE.lock().ok()?;
    let state = guard.as_ref()?;
    let mut s = state.lock().ok()?;
    if s.pcm.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(s.pcm.len() + 8);
    out.extend_from_slice(&(s.sample_rate as u32).to_le_bytes());
    out.extend_from_slice(&(s.channels as u32).to_le_bytes());
    out.extend_from_slice(&s.pcm);
    s.pcm.clear();
    Some(out)
}

/// 停止音频捕获
pub fn stop_audio_capture() {
    let mut guard = AUDIO_CAPTURE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.take() {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.active = false;
        s.pcm.clear();
        // 先注销 process 回调，再 quit mainloop（后台 pw_main_loop_run 返回后线程退出），
        // 最后 drop stream → PipeWire 节点消失 → link 断开
        s._listener = None;
        if let Some(ml) = s._mainloop.take() {
            ml.quit();
        }
        std::thread::sleep(Duration::from_millis(100));
        s._stream = None;
    }
    audio_log!("[audio] capture stopped");
}

/// 返回当前音频捕获链路状态（JSON 字符串），供 Java /wl audio status 查询。
/// 覆盖全链路：是否有会话 → PID → 拓扑节点/端口 → 目标节点 → 是否已 link → 回调/字节统计。
pub fn get_audio_capture_status() -> String {
    let guard = match AUDIO_CAPTURE.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let Some(state) = guard.as_ref() else {
        return r#"{"active":false,"stage":"idle","note":"no capture session"}"#.to_string();
    };
    let s = match state.lock() {
        Ok(s) => s,
        Err(e) => e.into_inner(),
    };

    let stage = if !s.active {
        "stopped"
    } else if s.capture_events == 0 {
        "linked_waiting_for_callback"
    } else {
        "streaming"
    };

    format!(
        concat!(
            r#"{{"active":{},"stage":"{}","mode":"monitor","pid":{},"sink_node":{},"#,
            r#""sink_name":"{}","stream_node":{},"linked":{},"capture_events":{},"#,
            r#""total_bytes":{},"sample_rate":{},"channels":{},"last_error":{}}}"#
        ),
        s.active,
        stage,
        s.pid,
        s.sink_node,
        s.sink_name.replace('"', "'"),
        s.stream_node,
        s.linked,
        s.capture_events,
        s.total_bytes,
        s.sample_rate,
        s.channels,
        match &s.last_error {
            Some(e) => format!("\"{}\"", e.replace('"', "'")),
            None => "null".to_string(),
        }
    )
}
