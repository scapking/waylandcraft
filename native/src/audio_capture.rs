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
}

/// 枚举 PipeWire registry 中所有 node / port 的信息
#[derive(Default, Clone)]
struct PwTopology {
    /// node_id -> (media_class, app_pid, name)
    pub nodes: HashMap<u32, (String, u32, String)>,
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
        .global(|global| {
            let mut t = topo_ref.lock().unwrap();
            match global.type_name() {
                "PipeWire:Interface:Node" => {
                    if let Some(media_class) = global.props().get("media.class") {
                        let app_pid = global
                            .props()
                            .get("app.process.id")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        let name = global
                            .props()
                            .get("node.name")
                            .unwrap_or("")
                            .to_string();
                        t.nodes
                            .insert(global.id(), (media_class.to_string(), app_pid, name));
                    }
                }
                "PipeWire:Interface:Port" => {
                    if let Some(parent_id) = global
                        .props()
                        .get("node.id")
                        .and_then(|s| s.parse::<u32>().ok())
                    {
                        let is_output = global
                            .props()
                            .get("port.direction")
                            .map(|d| d == "output")
                            .unwrap_or(false);
                        t.ports.insert(global.id(), (parent_id, is_output));
                    }
                }
                _ => {}
            }
        })
        .register()
        .map_err(|e| format!("registry listener: {}", e))?;

    let ml = mainloop.clone();
    std::thread::spawn(move || {
        let _ = ml.run();
    });

    std::thread::sleep(Duration::from_millis(collect_ms));
    mainloop.quit();

    let t = topology.lock().unwrap();
    Ok(t.clone())
}

/// 找目标进程的音频输出节点 + 它的 output 端口
fn find_process_audio(pid: u32, topo: &PwTopology) -> Result<(u32, u32), String> {
    // 优先 Stream/Output/Audio（普通应用输出流）；其次 Audio/Sink（某些应用自建 sink）
    let mut best_node = None;
    let mut best_score = 0i32;

    for (node_id, (media_class, app_pid, _name)) in &topo.nodes {
        if *app_pid != pid {
            continue;
        }
        let score = if media_class.contains("Stream/Output/Audio") {
            2
        } else if media_class == "Audio/Sink" {
            1
        } else {
            0
        };
        if score > best_score {
            best_score = score;
            best_node = Some(*node_id);
        }
    }

    let node_id = best_node.ok_or_else(|| {
        format!(
            "no PipeWire audio node found for pid={} (app may be silent or audio not on PipeWire)",
            pid
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
    let mut unit = ();
    let _listener = stream
        .add_local_listener_with_user_data(&mut unit)
        .process(move |stream, _| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if !datas.is_empty() {
                    let data = &mut datas[0];
                    let size = data.chunk().size() as usize;
                    if size > 0 {
                        if let Some(slice) = data.data() {
                            let src = &slice[..size.min(slice.len())];
                            let mut s = state_ref.lock().unwrap();
                            if s.active {
                                s.pcm.extend_from_slice(src);
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
            Choice,
            Range,
            Int,
            48000,
            8000,
            192000
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::AudioChannels,
            Choice,
            Range,
            Int,
            2,
            1,
            8
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

    // 保存全局状态（含保活 stream）；mainloop 在后台线程跑
    {
        let mut guard = AUDIO_CAPTURE.lock().map_err(|e| format!("lock: {}", e))?;
        {
            let mut s = state.lock().map_err(|e| format!("state lock: {}", e))?;
            s._stream = Some(stream.clone());
        }
        *guard = Some(state.clone());
    }

    let ml = mainloop.clone();
    std::thread::Builder::new()
        .name("wc-audio-capture".to_string())
        .spawn(move || {
            if let Err(e) = ml.run() {
                eprintln!("[audio] mainloop error: {}", e);
            }
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
        s._stream = None; // drop stream → PipeWire 节点消失 → link 断开
    }
    eprintln!("[audio] capture stopped");
}
