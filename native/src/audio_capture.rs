// 多人共享音频捕获 —— "只要共享窗口的声音，不是录音的声音"
//
// 主路径（per-app）：窗口 → 进程 PID（Java 侧 SO_PEERCRED / X11 _NET_WM_PID 解析）
// → 在 PipeWire 拓扑中找该 PID 的**播放流节点**（media.class 以 "Stream/Input"
// 开头；兼容旧版 "Audio/Stream" 命名）→ 我们的 capture stream 用
// target.object=<该节点 id> + AUTOCONNECT 直连该应用输出端口。
// 这条路不经过设备混音/sink monitor，捕获内容 = 该应用自己的播放流。
//
// 回退路径（monitor-fallback）：应用没出声（无 Stream/Input 节点）、PID 未知、
// 或直连失败时 → 沿用旧方案：连系统默认 Audio/Sink 的 monitor（整机捕获），
// 并在 audio.log / 状态 JSON / status.rs 标注 mode=monitor-fallback 及原因。
//
// 断流自愈：per-app 直连的节点会随应用重开流（暂停/恢复/重连设备）消失，
// 表现为 process 回调持续 ~2s 无字节。watchdog 线程重新枚举 + 重连同一 pid
// 的最新节点；连续失败降级 monitor-fallback；重试有节流 + 指数退避。
//
// 已知局限：单进程多窗口应用（如 Firefox 多标签）的声音按进程合并 ——
// PipeWire 层面无法细分到窗口，这是粒度极限。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// 捕获模式（对外字符串见 `CaptureMode::as_str`，Java/UI 按此透出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// 直连共享窗口应用自身的播放流节点（"窗口的声音"）
    PerApp,
    /// 整机默认 sink monitor 捕获（降级方案）
    MonitorFallback,
}

impl CaptureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureMode::PerApp => "per-app",
            CaptureMode::MonitorFallback => "monitor-fallback",
        }
    }
}

// ── watchdog 时序常量 ─────────────────────────────────────────────
/// process 回调持续无数据超过该时长 → 判定断流，触发重枚举
const STALL_MS: u64 = 2000;
/// 相邻两次断流检查（含重新枚举 PipeWire 拓扑）的最小间隔（节流）
const CHECK_THROTTLE_MS: u64 = 1500;

/// 捕获状态：累积 PCM 缓冲 + 全链路诊断字段
pub struct AudioCaptureState {
    pub pcm: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
    pub active: bool,
    /// 保活 capture stream（drop 会导致 PipeWire 节点消失、link 断开）
    pub _stream: Option<pw::stream::Stream>,
    /// 保活 mainloop：后台线程跑 run()，stop/重连时先 quit 再随 state drop
    pub _mainloop: Option<pw::main_loop::MainLoop>,
    /// 保活 process 回调注册句柄：drop 会 unregister，process 不再被调用。
    /// 这是音频"实际不可用"的根因——之前是函数局部变量，返回即失效。
    pub _listener: Option<pw::stream::StreamListener<()>>,
    /// 诊断计数：process 回调触发次数 / 累计捕获字节
    pub capture_events: u64,
    pub total_bytes: u64,
    /// 全链路状态（JSON 字符串，供 Java audioCaptureStatus 查询）
    pub pid: u32,
    /// 当前捕获模式与目标（per-app: 应用播放流节点；monitor: sink 节点）
    pub mode: CaptureMode,
    pub target_node: u32,
    pub target_name: String,
    /// mode=monitor-fallback 时记录降级原因（供状态/UI 展示）
    pub fallback_reason: Option<String>,
    /// watchdog 依据：最后一次 process 回调拿到 >0 字节的时刻 / 下次检查时刻
    pub last_data_at: Instant,
    pub next_check_at: Instant,
    pub check_backoff: Duration,
    /// per-app 重连连续失败计数 / 历史重连次数 / 是否禁止再升级回 per-app
    pub reconnect_failures: u32,
    pub total_reconnects: u64,
    pub promotion_disabled: bool,
    pub last_promotion_at: Instant,
    /// "节点还在但无数据（应用静音）"只播报一次，防刷屏
    pub silence_logged: bool,
    /// 兼容旧字段：最近一次 monitor 捕获使用的 sink（per-app 时为 0/""）
    pub sink_node: u32,
    pub sink_name: String,
    pub stream_node: u32,
    pub linked: bool,
    pub last_error: Option<String>,
}

impl AudioCaptureState {
    fn new(pid: u32) -> Self {
        let now = Instant::now();
        Self {
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
            mode: CaptureMode::MonitorFallback,
            target_node: 0,
            target_name: String::new(),
            fallback_reason: None,
            last_data_at: now,
            next_check_at: now + Duration::from_millis(STALL_MS),
            check_backoff: Duration::from_millis(CHECK_THROTTLE_MS),
            reconnect_failures: 0,
            total_reconnects: 0,
            promotion_disabled: false,
            last_promotion_at: now,
            silence_logged: false,
            sink_node: 0,
            sink_name: String::new(),
            stream_node: 0,
            linked: false,
            last_error: None,
        }
    }
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
    // ⑤ 关键修复：Registry 的 Drop 会 pw_proxy_destroy——旧写法把
    // get_registry() 的临时 Registry 在链式表达式结束时立即销毁，
    // 监听器挂在一个已销毁的 proxy 上，global 事件永远 0 个
    // （= 音频"找不到默认 sink"的真根因，与 PipeWire 是否运行无关）。
    // 必须用具名变量持有到 mainloop 结束。
    let registry = core
        .get_registry()
        .map_err(|e| format!("registry: {}", e))?;
    let _listener = registry
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

/// 判断一个 media.class 是否是**应用音频播放流**（要捕获的目标）。
///
/// - 现代命名：`Stream/Input/Audio`（PipeWire 视角：应用把音频"输入"给 daemon）。
///   用 starts_with("Stream/Input") 命中方向，但必须再要求含 "Audio"，
///   以排除同前缀的 `Stream/Input/Video`、`Stream/Input/Midi`（它们不是音频，
///   直连会拿到非 PCM 负载）。
/// - 旧版命名：`Audio/Stream`（无方向前缀）——按任务要求用 contains 兼容，
///   但要排除 `Stream/Output*`（那是 capture 流/设备侧输出，不是播放流）。
/// 设备节点（Audio/Sink、Audio/Source）不算。
fn is_playback_stream_class(media_class: &str) -> bool {
    let is_audio = media_class.contains("Audio");
    if !is_audio {
        return false;
    }
    media_class.starts_with("Stream/Input")
        || (media_class.contains("Audio/Stream") && !media_class.starts_with("Stream/Output"))
}

/// 按 PID 匹配该应用的播放流节点，返回 (node_id, node_name, media_class)。
///
/// 一个进程可能先后开过多条播放流（暂停/重开），节点 id 是 PipeWire 会话内
/// 单调递增的 serial —— 取 **id 最大** 的一个即"最新一条"。
/// 纯函数（不打日志），便于静态单测。
fn find_app_stream_node(topo: &PwTopology, pid: u32) -> Option<(u32, String, String)> {
    if pid == 0 {
        return None; // PID 未知 → 无从按应用匹配
    }
    let mut best: Option<(u32, String, String)> = None;
    for (id, (media_class, node_pid, name, _)) in &topo.nodes {
        if *node_pid != pid || !is_playback_stream_class(media_class) {
            continue;
        }
        match &best {
            Some((best_id, _, _)) if *best_id >= *id => {}
            _ => best = Some((*id, name.clone(), media_class.clone())),
        }
    }
    best
}

/// 找系统默认输出 sink（monitor fallback 目标）。
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
    sinks.sort();

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

/// 拆卸当前会话的 PipeWire 对象（调用方持锁）。
/// 先注销 process 回调（不再产生新 process 事件），再 quit mainloop，
/// 并把 stream/mainloop **移出 state 返回**——调用方必须在释放锁之后
/// 再 finish_teardown drop 它们：此刻 process 回调可能正阻塞在 state 锁上，
/// 持锁 drop pw_stream 会让 mainloop 线程回调返回时对已释放对象解引用
/// （同⑤ Registry Drop bug 的一类跨线程释放问题）。
fn teardown_locked(s: &mut AudioCaptureState) -> (Option<pw::stream::Stream>, Option<pw::main_loop::MainLoop>) {
    s._listener = None;
    let ml = s._mainloop.take();
    if let Some(ml) = &ml {
        ml.quit();
    }
    s.linked = false;
    (s._stream.take(), ml)
}

/// 在**不持锁**的情况下收尾拆卸：等后台 pw_main_loop_run 返回（线程退出）
/// 后 drop stream + mainloop。两者必须等 mainloop 线程停止后再销毁。
fn finish_teardown(old: (Option<pw::stream::Stream>, Option<pw::main_loop::MainLoop>)) {
    let (stream, ml) = old;
    if stream.is_some() || ml.is_some() {
        std::thread::sleep(Duration::from_millis(150));
    }
    drop(stream);
    drop(ml);
}

/// 建立（或重建）capture 会话并连到指定目标节点。
///
/// - target_id：per-app 时为应用播放流节点 id，monitor 时为 sink 节点 id。
///   SPA_KEY_TARGET_OBJECT 用数字 id（pw_core_find_object 同时支持名字/id，
///   id 更精确：多个 node 可能同名）。
/// - stream.capture.sink 不设（=false）：per-app 不经 sink，monitor 用默认 monitor。
/// - 调用前目标会话必须已 teardown（本函数内部会兜底 teardown）。
fn connect_capture(
    state: &Arc<Mutex<AudioCaptureState>>,
    mode: CaptureMode,
    target_id: u32,
    target_name: &str,
    reason: Option<String>,
) -> Result<(), String> {
    audio_log!(
        "[audio] connect_capture: mode={} target_node={} target_name={}{}",
        mode.as_str(),
        target_id,
        target_name,
        reason.as_deref().map(|r| format!(" reason={r}")).unwrap_or_default()
    );

    // 重连场景：先拆掉旧会话（锁内摘对象 + 锁外 drop，见 teardown_locked 注释）。
    // 新建路径 state 里没有旧对象，teardown/finish 均为空操作。
    let old = {
        let mut s = state.lock().map_err(|e| format!("state lock: {}", e))?;
        teardown_locked(&mut s)
    };
    finish_teardown(old);

    pw::init();
    let mainloop = pw::main_loop::MainLoop::new(None).map_err(|e| format!("mainloop: {}", e))?;
    let context = pw::context::Context::new(&mainloop).map_err(|e| format!("context: {}", e))?;
    let core = context.connect(None).map_err(|e| format!("connect: {}", e))?;

    let stream = pw::stream::Stream::new(
        &core,
        "wc-audio-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
            // 直连目标节点（per-app=应用播放流节点 id；monitor=sink 节点 id）。
            // AUTOCONNECT 下 PipeWire router 把我们的 input 端口 link 到目标
            // 节点的 output 端口（per-app=应用输出端口，不经设备混音）。
            // 字面量 "target.object" = SPA/PW_KEY_TARGET_OBJECT（pipewire-rs
            // 里该常量键在 v0_3_44 feature 后，用字符串避免依赖 feature 开关）。
            // 值为节点数字 id：pw_core_find_object 同时支持 name 和 id，
            // id 更精确（node.name 可能重复）。stream.capture.sink 不设（=false）。
            "target.object" => target_id.to_string(),
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
                                    // watchdog 的存活依据：收到数据的最新时刻
                                    s.last_data_at = Instant::now();
                                    if s.capture_events == 1 {
                                        log_line = Some(format!(
                                            "[audio] FIRST capture — {} bytes (process 回调 LIVE, mode={})",
                                            src.len(),
                                            s.mode.as_str()
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

    // 格式：S16LE / 48k / 2ch（与接收端 SharedWindowAudioPayload 约定一致；
    // PipeWire adapter 会在目标端口格式不同时自动重采样/转格式）
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
        "[audio] connect_capture: our capture node={}（AUTOCONNECT → target {}）",
        stream_node,
        target_id
    );

    let ml_ptr = MainLoopPtr(mainloop.as_raw_ptr());
    {
        let mut s = state.lock().map_err(|e| format!("state lock: {}", e))?;
        if !s.active {
            // 建立期间会话被 stop（stop 与重连竞态）：新对象尚未挂上
            // mainloop 线程，直接随本函数返回 drop，安全。
            return Err("session stopped during connect".to_string());
        }
        // 旧会话已在函数入口 teardown（锁外 finish_teardown），此处只装入新对象
        s._stream = Some(stream);
        s._mainloop = Some(mainloop);
        s._listener = Some(listener);
        s.stream_node = stream_node;
        s.linked = true;
        s.mode = mode;
        s.target_node = target_id;
        s.target_name = target_name.to_string();
        s.fallback_reason = reason;
        if mode == CaptureMode::MonitorFallback {
            s.sink_node = target_id;
            s.sink_name = target_name.to_string();
        }
        let now = Instant::now();
        s.last_data_at = now;
        s.next_check_at = now + Duration::from_millis(STALL_MS);
        s.check_backoff = Duration::from_millis(CHECK_THROTTLE_MS);
        s.silence_logged = false;
    }

    // 裸指针：mainloop 本体由 state 持有，stop/重连时先 quit 再 drop
    std::thread::Builder::new()
        .name("wc-audio-capture".to_string())
        .spawn(move || {
            mainloop_run(ml_ptr);
        })
        .map_err(|e| format!("spawn: {}", e))?;

    Ok(())
}

/// 降级/回退到整机 sink monitor 捕获（要求 topo 已枚举）。
/// 返回 Err = 连 sink 都找不到/连不上（整个启动失败）。
fn connect_monitor_fallback(
    state: &Arc<Mutex<AudioCaptureState>>,
    topo: &PwTopology,
    reason: String,
) -> Result<(), String> {
    let (sink_node, sink_name) = find_default_sink(topo)
        .map_err(|e| format!("monitor fallback 也找不到默认 sink: {}", e))?;
    audio_log!(
        "[audio] FALLBACK monitor-fallback: sink node={} name={} — {}",
        sink_node, sink_name, reason
    );
    crate::status::note_error("warn", &format!("[audio] monitor fallback: {}", reason));
    connect_capture(
        state,
        CaptureMode::MonitorFallback,
        sink_node,
        &sink_name,
        Some(reason),
    )
}

/// 启动音频捕获：优先 per-app 直连共享窗口应用的播放流节点；
/// 找不到（应用没出声 / PID 未知）→ monitor-fallback 整机捕获。
pub fn start_audio_capture(pid: u32) -> Result<(), String> {
    // 确保没有正在运行的捕获
    stop_audio_capture();

    audio_log!(
        "[audio] ===== start_audio_capture (per-app 优先, monitor fallback; 窗口 pid={}) =====",
        pid
    );
    audio_log!("[audio] stage 1/4: 枚举 PipeWire 拓扑（节点）...");
    let topo = enumerate_topology(500).map_err(|e| {
        let msg = format!("[audio] 拓扑枚举失败: {}", e);
        audio_log!("{}", msg);
        crate::status::note_error("error", &msg);
        msg
    })?;
    audio_log!("[audio] stage 1/4: OK — {} nodes", topo.nodes.len());

    // 列出该 pid 的全部节点 + 全部播放流节点，便于对照 pw-dump 排查
    if pid > 0 {
        for (id, (mc, npid, name, _)) in topo.nodes.iter().filter(|(_, (_, p, _, _))| *p == pid) {
            audio_log!("[audio] pid节点: id={} class={} name={}", id, mc, name);
        }
    }
    for (id, (mc, npid, name, _)) in
        topo.nodes.iter().filter(|(_, (mc, _, _, _))| is_playback_stream_class(mc))
    {
        audio_log!("[audio] candidate app stream: id={} class={} pid={} name={}", id, mc, npid, name);
    }

    let state = Arc::new(Mutex::new(AudioCaptureState::new(pid)));

    audio_log!("[audio] stage 2/4: 按 pid={} 匹配应用播放流节点（Stream/Input）...", pid);
    let app_node = find_app_stream_node(&topo, pid);

    let connected: CaptureMode = match &app_node {
        Some((node_id, node_name, media_class)) => {
            audio_log!(
                "[audio] stage 2/4: OK — per-app 目标 node={} class={} name={}",
                node_id, media_class, node_name
            );
            audio_log!("[audio] stage 3/4: 创建 capture stream（直连应用输出端口）...");
            match connect_capture(&state, CaptureMode::PerApp, *node_id, node_name, None) {
                Ok(()) => CaptureMode::PerApp,
                Err(e) => {
                    // 目标节点在枚举与连接之间消失等竞态 → 直接降级 monitor
                    let msg = format!("per-app connect failed (node {}): {}", node_id, e);
                    audio_log!("[audio] stage 3/4: 直连失败: {}", msg);
                    connect_monitor_fallback(&state, &topo, msg)?;
                    CaptureMode::MonitorFallback
                }
            }
        }
        None => {
            let reason = if pid == 0 {
                "pid unknown — 无法按应用匹配，捕获整机音频".to_string()
            } else {
                format!("pid {} 没有播放流节点（应用没出声/没打开流），捕获整机音频", pid)
            };
            audio_log!("[audio] stage 2/4: 未找到应用节点 → {}", reason);
            audio_log!("[audio] stage 3/4: 回退整机 sink monitor...");
            connect_monitor_fallback(&state, &topo, reason)?;
            CaptureMode::MonitorFallback
        }
    };

    // 保存全局状态（含保活 stream + mainloop）；后台 mainloop 线程已由
    // connect_capture 启动
    {
        let mut guard = AUDIO_CAPTURE.lock().map_err(|e| format!("lock: {}", e))?;
        *guard = Some(state.clone());
    }

    audio_log!(
        "[audio] stage 4/4: OK — 捕获会话已启动（mode={}），等待 process 回调产出 PCM",
        connected.as_str()
    );
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

/// 停止音频捕获（watchdog 线程通过 active=false 自行退出）
pub fn stop_audio_capture() {
    let mut guard = AUDIO_CAPTURE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.take() {
        let old = {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.active = false;
            s.pcm.clear();
            teardown_locked(&mut s)
        };
        finish_teardown(old); // 等 mainloop 线程退出 run() 后再 drop stream/mainloop
    }
    audio_log!("[audio] capture stopped");
}

/// 返回当前音频捕获链路状态（JSON 字符串），供 Java /wl audio status 查询。
/// 覆盖全链路：是否有会话 → PID/mode → 目标节点 → 是否已 link → 回调/字节统计。
pub fn get_audio_capture_status() -> String {
    let guard = match AUDIO_CAPTURE.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let Some(state) = guard.as_ref() else {
        return r#"{"active":false,"stage":"idle","mode":"none","note":"no capture session"}"#
            .to_string();
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
    // Java/UI 的 mode 三态：per-app / monitor-fallback / none
    let mode = if s.active { s.mode.as_str() } else { "none" };

    format!(
        concat!(
            r#"{{"active":{},"stage":"{}","mode":"{}","pid":{},"#,
            r#""target_node":{},"target_name":"{}","fallback_reason":{},"#,
            r#""sink_node":{},"sink_name":"{}","stream_node":{},"linked":{},"#,
            r#""capture_events":{},"total_bytes":{},"reconnects":{},"#,
            r#""sample_rate":{},"channels":{},"last_error":{}}}"#
        ),
        s.active,
        stage,
        mode,
        s.pid,
        s.target_node,
        s.target_name.replace('"', "'"),
        match &s.fallback_reason {
            Some(r) => format!("\"{}\"", r.replace('"', "'")),
            None => "null".to_string(),
        },
        s.sink_node,
        s.sink_name.replace('"', "'"),
        s.stream_node,
        s.linked,
        s.capture_events,
        s.total_bytes,
        s.total_reconnects,
        s.sample_rate,
        s.channels,
        match &s.last_error {
            Some(e) => format!("\"{}\"", e.replace('"', "'")),
            None => "null".to_string(),
        }
    )
}

/// ⑥ 状态快照（status.rs audio_capture 子系统）用的轻量摘要。
/// try_lock：status 采集绝不与 mainloop/watchdog 抢锁阻塞帧循环，
/// 锁忙按 idle 处理（下一次采集再报）。
pub enum CaptureBrief {
    Idle(String),
    PerApp(String),
    MonitorFallback(String),
}

pub fn audio_capture_brief() -> CaptureBrief {
    let Ok(guard) = AUDIO_CAPTURE.try_lock() else {
        return CaptureBrief::Idle("status busy".to_string());
    };
    let Some(state) = guard.as_ref() else {
        return CaptureBrief::Idle("no capture session; see waylandcraft-audio.log".to_string());
    };
    let Ok(s) = state.try_lock() else {
        return CaptureBrief::Idle("status busy".to_string());
    };
    if !s.active {
        return CaptureBrief::Idle(format!("stopped (mode was {}, bytes={})", s.mode.as_str(), s.total_bytes));
    }
    let common = format!(
        "node {} \"{}\" events={} bytes={} reconnects={}",
        s.target_node, s.target_name, s.capture_events, s.total_bytes, s.total_reconnects
    );
    match s.mode {
        CaptureMode::PerApp => CaptureBrief::PerApp(common),
        CaptureMode::MonitorFallback => CaptureBrief::MonitorFallback(match &s.fallback_reason {
            Some(r) => format!("{} reason={}", common, r),
            None => common,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topo(entries: &[(u32, &str, u32, &str)]) -> PwTopology {
        let mut nodes = HashMap::new();
        for (id, mc, pid, name) in entries {
            nodes.insert(*id, (mc.to_string(), *pid, name.to_string(), String::new()));
        }
        PwTopology { nodes }
    }

    #[test]
    fn playback_class_modern_and_legacy() {
        assert!(is_playback_stream_class("Stream/Input/Audio"));
        assert!(is_playback_stream_class("Stream/Input/Audio/1")); // 带后缀变体
        assert!(is_playback_stream_class("Audio/Stream")); // 旧版命名
        // 反向排除：capture 流 / 设备 / 视频
        assert!(!is_playback_stream_class("Stream/Output/Audio"));
        assert!(!is_playback_stream_class("Audio/Sink"));
        assert!(!is_playback_stream_class("Audio/Source"));
        assert!(!is_playback_stream_class("Stream/Input/Video"));
        assert!(!is_playback_stream_class("Stream/Input/Midi"));
    }

    #[test]
    fn per_app_picks_newest_node_of_same_pid() {
        // PipeWire node id 单调递增 → 最大 id = 应用最新打开的播放流
        let t = topo(&[
            (60, "Stream/Input/Audio", 1000, "papay-output.60"),
            (75, "Stream/Input/Audio", 1000, "papay-output.75"),
            (66, "Stream/Input/Audio", 1000, "papay-output.66"),
        ]);
        let (id, name, _mc) = find_app_stream_node(&t, 1000).expect("match");
        assert_eq!(id, 75);
        assert_eq!(name, "papay-output.75");
    }

    #[test]
    fn per_app_ignores_other_pid_capture_and_devices() {
        let t = topo(&[
            (10, "Stream/Input/Audio", 2000, "other-app"),        // 别的进程
            (11, "Stream/Output/Audio", 1000, "our-capture"),     // 我们自己的 capture 流
            (12, "Audio/Sink", 0, "alsa_output"),                 // 设备
            (13, "Stream/Input/Audio", 1000, "target-app"),       // 目标
        ]);
        let (id, _, _) = find_app_stream_node(&t, 1000).expect("match");
        assert_eq!(id, 13);
    }

    #[test]
    fn legacy_class_matched_when_only_candidate() {
        let t = topo(&[(30, "Audio/Stream", 1000, "legacy-app")]);
        assert!(find_app_stream_node(&t, 1000).is_some());
    }

    #[test]
    fn pid_zero_never_matches() {
        let t = topo(&[(30, "Stream/Input/Audio", 0, "weird")]);
        assert!(find_app_stream_node(&t, 0).is_none());
    }

    #[test]
    fn no_match_returns_none() {
        let t = topo(&[(10, "Stream/Input/Audio", 2000, "other"), (12, "Audio/Sink", 0, "s")]);
        assert!(find_app_stream_node(&t, 1000).is_none());
    }
}

#[cfg(test)]
mod repro_tests {
    use super::*;

    /// 环境复现：在与 Minecraft 相同的桌面会话环境跑真实拓扑枚举，
    /// 打印全部节点（重点：所有 Stream/Input 播放流节点，验证 per-app
    /// 匹配逻辑面对真实 PipeWire 数据的行为）。
    /// CI（无 pipewire 守护）跳过；本机开发环境手动验证。
    #[test]
    #[ignore = "需要运行中的 PipeWire 守护"]
    fn enumerate_topology_live() {
        let topo = enumerate_topology(3000).expect("topology enum failed");
        eprintln!("nodes={}", topo.nodes.len());
        for (id, (mc, pid, name, proc)) in &topo.nodes {
            eprintln!("  id={id} class={mc} pid={pid} name={name} proc={proc}");
        }
        eprintln!("--- Stream/Input（播放流）节点 ---");
        let inputs: Vec<&u32> = topo
            .nodes
            .iter()
            .filter(|(_, (mc, _, _, _))| mc.starts_with("Stream/Input"))
            .map(|(id, _)| id)
            .collect();
        for id in &inputs {
            let (mc, pid, name, proc) = &topo.nodes[id];
            eprintln!("  PLAYBACK id={id} class={mc} pid={pid} name={name} proc={proc}");
        }
        eprintln!("--- is_playback_stream_class 命中集 ---");
        for (id, (mc, pid, name, _)) in &topo.nodes {
            if is_playback_stream_class(mc) {
                eprintln!("  MATCH id={id} class={mc} pid={pid} name={name}");
            }
        }
        let sink = find_default_sink(&topo);
        eprintln!("sink={sink:?}");
        assert!(!topo.nodes.is_empty(), "enumerate returned 0 nodes while pipewire running");
        assert!(sink.is_ok(), "no Audio/Sink while pipewire running: {:?}", sink);
    }
}
