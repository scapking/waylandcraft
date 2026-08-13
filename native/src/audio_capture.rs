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
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pipewire as pw;
use pw::prelude::*;

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

/// 枚举 PipeWire registry 中所有 node / port 的信息
#[derive(Default, Clone)]
struct PwTopology {
    /// node_id -> (media_class, app_pid, node_name, app_process_name)
    pub nodes: HashMap<u32, (String, u32, String, String)>,
    /// port_id -> (parent_node_id, is_output)
    pub ports: HashMap<u32, (u32, bool)>,
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
                pw::types::ObjectType::Port => {
                    if let Some(parent_id) = global
                        .props
                        .and_then(|p| p.get("node.id"))
                        .and_then(|s| s.parse::<u32>().ok())
                    {
                        let is_output = global
                            .props
                            .and_then(|p| p.get("port.direction"))
                            .map(|d| d == "output")
                            .unwrap_or(false);
                        t.ports.insert(global.id, (parent_id, is_output));
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

/// 收集 pid 及其所有后代进程（/proc/<pid>/task/*/children）。
/// 多进程应用（Firefox/Chrome 等）的音频输出在 content/渲染子进程里，
/// 窗口 PID 是主进程 —— 只按主进程 PID 精确匹配会永远抓不到声音。
fn collect_process_tree(pid: u32) -> Vec<u32> {
    use std::collections::HashSet;
    let mut result = vec![pid];
    let mut seen = HashSet::new();
    seen.insert(pid);
    let mut queue = vec![pid];

    while let Some(p) = queue.pop() {
        let task_dir = format!("/proc/{}/task", p);
        let Ok(entries) = std::fs::read_dir(&task_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let tid = entry.file_name().to_string_lossy().to_string();
            let children_path = format!("/proc/{}/task/{}/children", p, tid);
            let Ok(children) = std::fs::read_to_string(children_path) else {
                continue;
            };
            for child in children.split_whitespace() {
                let Ok(cpid) = child.parse::<u32>() else {
                    continue;
                };
                if seen.insert(cpid) {
                    result.push(cpid);
                    queue.push(cpid);
                }
            }
        }
    }
    result
}

/// 窗口进程的可执行文件名（/proc/<pid>/exe basename），用于进程名级匹配。
fn process_exe_name(pid: u32) -> Option<String> {
    let exe = std::fs::read_link(format!("/proc/{}/exe", pid)).ok()?;
    exe.file_name().map(|n| n.to_string_lossy().to_string())
}

/// 找目标进程的音频输出节点 + 它的 output 端口。
///
/// 匹配策略（由松到严，全部走 PipeWire 节点属性）：
/// 1. app.process.id ∈ 窗口进程树（窗口 PID 本身 + 所有后代，覆盖 Firefox 多进程）；
/// 2. app.process.name == 窗口进程 exe basename（pipewire 的 app.process.name 取自
///    进程 exe，content 进程与主进程同名；兜底匹配）。
fn find_process_audio(pid: u32, topo: &PwTopology) -> Result<(u32, u32), String> {
    let tree = collect_process_tree(pid);
    let exe_name = process_exe_name(pid);

    // 优先 Stream/Output/Audio（普通应用输出流）；其次 Audio/Sink（某些应用自建 sink）
    let mut best_node = None;
    let mut best_score = 0i32;

    for (node_id, (media_class, app_pid, _name, proc_name)) in &topo.nodes {
        let pid_ok = tree.iter().any(|p| p == app_pid);
        let name_ok = !pid_ok
            && exe_name.is_some()
            && !proc_name.is_empty()
            && proc_name.eq_ignore_ascii_case(exe_name.as_deref().unwrap_or(""));
        if !pid_ok && !name_ok {
            continue;
        }
        let mut score = if media_class.contains("Stream/Output/Audio") {
            2
        } else if media_class == "Audio/Sink" {
            1
        } else {
            0
        };
        if pid_ok {
            score += 10; // 进程树精确命中优先于进程名兜底
        }
        if score > best_score {
            best_score = score;
            best_node = Some(*node_id);
        }
    }

    let node_id = best_node.ok_or_else(|| {
        // 失败时列出所有候选节点，方便直接看 key 是否又对不上
        let mut candidates = Vec::new();
        for (node_id, (media_class, app_pid, _name, proc_name)) in &topo.nodes {
            candidates.push(format!(
                "  node {}: class={} pid={} proc={}",
                node_id, media_class, app_pid, proc_name
            ));
        }
        format!(
            "no PipeWire audio node found for pid={} (tree={} nodes, exe={:?}) — app may be silent or audio not on PipeWire\ncandidates:\n{}",
            pid,
            tree.len(),
            exe_name,
            candidates.join("\n")
        )
    })?;

    // 找该节点的 output 端口（对 Stream/Output/Audio，输出端口是接 sink 的那个）
    let mut port_id = None;
    for (port_id_, (parent, is_output)) in &topo.ports {
        if *parent == node_id && *is_output {
            port_id = Some(*port_id_);
            break;
        }
    }
    let out_port = port_id.ok_or_else(|| format!("node {} has no output port", node_id))?;

    Ok((node_id, out_port))
}

/// 启动音频捕获：匹配 pid 的音频节点 → capture stream → pw-link 强制连接
pub fn start_audio_capture(pid: u32) -> Result<(), String> {
    // 确保没有正在运行的捕获
    stop_audio_capture();

    let topo = enumerate_topology(500)?;
    eprintln!(
        "[audio] enumerated {} nodes, {} ports",
        topo.nodes.len(),
        topo.ports.len()
    );
    let (target_node, target_port) = find_process_audio(pid, &topo)?;
    eprintln!(
        "[audio] target node={} (output port {}) for pid={}",
        target_node, target_port, pid
    );

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
    }));

    let stream = pw::stream::Stream::new(
        &core,
        "wc-audio-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
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
                                            "[audio] FIRST capture: {} bytes (capture stream LIVE)",
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
                                eprintln!("{line}");
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
            pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| format!("connect: {}", e))?;

    // 等 stream 的 node/port global 出现在 registry 上
    std::thread::sleep(Duration::from_millis(400));
    let topo2 = enumerate_topology(400)?;
    let stream_node = stream.node_id();
    eprintln!("[audio] capture stream node={}", stream_node);

    // 找 stream 的 input 端口
    let mut stream_port = None;
    for (port_id, (parent, is_output)) in &topo2.ports {
        if *parent == stream_node && !*is_output {
            stream_port = Some(*port_id);
            break;
        }
    }
    let stream_port =
        stream_port.ok_or_else(|| format!("capture stream {} has no input port", stream_node))?;

    // 用 pw-link 强制连接：目标节点 output 端口 → capture stream input 端口
    // （目标端口可能已连 sink，pw-link 允许一个 output 端口多路 fan-out）
    let output = Command::new("pw-link")
        .args(&[&target_port.to_string(), &stream_port.to_string()])
        .output()
        .map_err(|e| format!("pw-link: {} (install pipewire-utils)", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("pw-link failed: {}", stderr.trim()));
    }
    eprintln!(
        "[audio] linked port {} -> {} (node {} -> stream {})",
        target_port, stream_port, target_node, stream_node
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
        }
        *guard = Some(state.clone());
    }

    std::thread::Builder::new()
        .name("wc-audio-capture".to_string())
        .spawn(move || {
            mainloop_run(ml_ptr);
        })
        .map_err(|e| format!("spawn: {}", e))?;

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
    eprintln!("[audio] capture stopped");
}
