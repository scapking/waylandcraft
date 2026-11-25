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
// 断流自愈：per-app 直连的节点会随应用重开流/退出而消失。watchdog 定期重枚举
// + 校验目标节点存在性与 id（应用重开流 id 会变 → 重连最新节点），并辅以
// "process 回调 ~2s 无字节" 的断流信号；连续失败降级 monitor-fallback；
// 全部重试有节流 + 指数退避。
// 注意（本机 PipeWire 1.4 实测）：目标节点消失后 daemon 会把孤儿 capture
// stream 重新路由到默认 sink monitor 并继续全速投递（静音）数据——只看字节流
// 会误判"捕获正常"，所以存在性重验是主判据，0 字节只是辅助信号。
//
// 已知局限：单进程多窗口应用（如 Firefox 多标签）的声音按进程合并 ——
// PipeWire 层面无法细分到窗口，这是粒度极限。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pipewire as pw;

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
/// per-app 模式下"目标节点存在性"定期重验周期（数据正常时也用，见 watchdog 注释）
const REVALIDATE_MS: u64 = 2000;
/// 断流但目标节点仍在（应用只是静音）时的退避上限，避免长时间静音下高频枚举
const MAX_BACKOFF_MS: u64 = 15_000;
/// monitor-fallback 下尝试升级回 per-app 的周期（应用可能稍后才出声）
const PROMOTION_MS: u64 = 5_000;
/// per-app 连续失败达到该次数 → 降级 monitor-fallback
const MAX_RECONNECT_FAILURES: u32 = 3;

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

/// 该节点的 props 值得我们 bind 成 Node proxy 去取全量 info（含 app pid）。
/// 只 bind 音频相关 node，省得给 Video/Midi/Port 建 proxy。
fn node_worth_binding(media_class: &str) -> bool {
    media_class.contains("Audio") || media_class.contains("Stream")
}

/// 通过 registry 枚举一次 PipeWire 拓扑（起线程跑 mainloop，主线程等 collect_ms 后 quit）
fn enumerate_topology(collect_ms: u64) -> Result<PwTopology, String> {
    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None).map_err(|e| format!("mainloop: {}", e))?;
    let context = pw::context::Context::new(&mainloop).map_err(|e| format!("context: {}", e))?;
    let core = context.connect(None).map_err(|e| format!("connect: {}", e))?;

    let topology = Arc::new(Mutex::new(PwTopology::default()));

    // ⑤ 关键修复：Registry 的 Drop 会 pw_proxy_destroy——旧写法把
    // get_registry() 的临时 Registry 在链式表达式结束时立即销毁，
    // 监听器挂在一个已销毁的 proxy 上，global 事件永远 0 个
    // （= 音频"找不到默认 sink"的真根因，与 PipeWire 是否运行无关）。
    // 必须用具名变量持有到 mainloop 结束。
    //
    // ⑦ per-app 关键修复：PipeWire ≥1.0 的 registry global 事件对 node
    // 只导出**过滤后的属性集**（实测 1.4.2：object.serial / factory.id /
    // client.id / client.api / application.name / node.name / media.class），
    // **不含 application.process.id**——只读 global props 时 pid 恒 0，
    // per-app 匹配永远失败。真正的 pid 在绑定后的 Node proxy info props 里
    // （pw-dump 就是绑定后读 info）。不能用 client.id→sec.pid 代替：
    // PulseAudio 应用（paplay/浏览器等）的 node 由 pipewire-pulse 进程
    // 创建，其 client pid 是 daemon 的。做法：global 回调里把音频相关
    // node bind 成 Node proxy（pw-mon 同款 Rc<Registry>+upgrade 模式），
    // info 回调把 application.process.id 合并进拓扑。
    // proxy+listener 必须保活到 mainloop 结束（同 ⑤ 教训）。
    let registry = std::rc::Rc::new(
        core.get_registry().map_err(|e| format!("registry: {}", e))?,
    );
    let registry_weak = std::rc::Rc::downgrade(&registry);
    type BoundProxies = std::rc::Rc<
        std::cell::RefCell<Vec<(Box<dyn pw::proxy::ProxyT>, Box<dyn pw::proxy::Listener>)>>,
    >;
    let bound: BoundProxies = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

    let topo_ref = topology.clone();
    let bound_ref = bound.clone();
    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            let (media_class, name, proc_name, pid_from_global) = match global.type_ {
                pw::types::ObjectType::Node => {
                    match global.props.and_then(|p| p.get("media.class")) {
                        Some(mc) => {
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
                            (mc.to_string(), name, proc_name, app_pid)
                        }
                        None => return,
                    }
                }
                _ => return,
            };
            let node_id = global.id;
            {
                let mut t = topo_ref.lock().unwrap();
                t.nodes.insert(
                    node_id,
                    (media_class.clone(), pid_from_global, name, proc_name),
                );
            }
            // global props 没给 pid → bind Node proxy 从 info 事件补
            // （某些客户端/版本可能在 global props 就带 pid，那就不用 bind）
            if pid_from_global == 0 && node_worth_binding(&media_class) {
                let Some(reg) = registry_weak.upgrade() else { return };
                if let Ok(node) = reg.bind::<pw::node::Node, _>(global) {
                    let topo_pid = topo_ref.clone();
                    let nl = node
                        .add_listener_local()
                        .info(move |info| {
                            if let Some(props) = info.props() {
                                if let Some(v) = props.get("application.process.id") {
                                    if let Ok(pid) = v.parse::<u32>() {
                                        if let Some(entry) =
                                            topo_pid.lock().unwrap().nodes.get_mut(&node_id)
                                        {
                                            entry.1 = pid;
                                        }
                                    }
                                }
                            }
                        })
                        .register();
                    bound_ref.borrow_mut().push((Box::new(node), Box::new(nl)));
                }
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

    // 等 daemon 处理 connect。node_id 要等 mainloop 分发到 info 事件后才
    // 有效（connect 刚返回时恒为 ID_NONE——旧实现状态里 stream_node 一直
    // 是 -1 的原因），mainloop 线程启动后再补读一次。
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

    // mainloop 已运行 → 补读自己的 node id（诊断字段）
    if stream_node == u32::MAX {
        std::thread::sleep(Duration::from_millis(300));
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = s._stream.as_ref() {
            let id = st.node_id();
            if id != u32::MAX {
                s.stream_node = id;
            }
        }
    }

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
        for (id, (mc, _npid, name, _)) in topo.nodes.iter().filter(|(_, (_, p, _, _))| *p == pid) {
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

    // watchdog：断流自愈（重枚举+重连同一 pid / 降级 / 节流）
    spawn_watchdog(state);

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

/// 启动断流自愈 watchdog（独立线程，靠 active=false 退出）。
fn spawn_watchdog(state: Arc<Mutex<AudioCaptureState>>) {
    if let Err(e) = std::thread::Builder::new()
        .name("wc-audio-watchdog".to_string())
        .spawn(move || watchdog_loop(state))
    {
        audio_log!("[audio] watchdog 启动失败（无断流自愈）: {}", e);
    }
}

/// watchdog 每轮需要的会话快照（持锁取一次，重连决策在无锁区做）。
struct WatchdogSnapshot {
    mode: CaptureMode,
    pid: u32,
    target_node: u32,
    stalled: bool,
    promote_due: bool,
}

fn watchdog_loop(state: Arc<Mutex<AudioCaptureState>>) {
    audio_log!(
        "[audio] watchdog started (stall={}ms, throttle={}ms, promotion={}ms)",
        STALL_MS,
        CHECK_THROTTLE_MS,
        PROMOTION_MS
    );
    loop {
        std::thread::sleep(Duration::from_millis(500));

        // ── 持锁读快照 + 节流判定 ──────────────────────────────
        let snap = {
            let mut s = match state.lock() {
                Ok(s) => s,
                Err(e) => e.into_inner(),
            };
            if !s.active {
                audio_log!("[audio] watchdog: session inactive, exiting");
                return;
            }
            let now = Instant::now();
            let stalled = now.duration_since(s.last_data_at) >= Duration::from_millis(STALL_MS);
            let promote_due = s.mode == CaptureMode::MonitorFallback
                && !s.promotion_disabled
                && s.pid > 0
                && now.duration_since(s.last_promotion_at) >= Duration::from_millis(PROMOTION_MS);

            // 轮次触发条件：
            // - per-app：定期重验"目标节点是否还在 / id 是否变了"。**不能只靠
            //   "0 字节"判断流**——实测 PipeWire 1.4：应用节点消失后 daemon 会
            //   把孤儿 capture stream 重新路由（连到默认 sink monitor），process
            //   回调继续全速投递静音数据，字节流完全正常但内容已经不是该应用。
            //   因此无论有无数据都按周期枚举核对；持续无数据（应用真静音）时
            //   指数退避降低枚举频率。
            // - monitor-fallback：仅"断流（sink 变了）"或"到点尝试升级"才跑。
            let due = match s.mode {
                CaptureMode::PerApp => now >= s.next_check_at,
                CaptureMode::MonitorFallback => {
                    now >= s.next_check_at && (stalled || promote_due)
                }
            };
            if !due {
                continue;
            }

            if stalled {
                // 退避：无数据持续时逐步拉长检查间隔，避免高频枚举
                s.check_backoff = (s.check_backoff * 2).min(Duration::from_millis(MAX_BACKOFF_MS));
                s.next_check_at = now + s.check_backoff;
            } else {
                s.check_backoff = Duration::from_millis(CHECK_THROTTLE_MS);
                s.next_check_at = now
                    + Duration::from_millis(if s.mode == CaptureMode::PerApp {
                        REVALIDATE_MS
                    } else {
                        CHECK_THROTTLE_MS
                    });
            }
            if promote_due {
                s.last_promotion_at = now;
            }

            audio_log!(
                "[audio] watchdog: round mode={} stalled={} promote_due={} events={} bytes={}",
                s.mode.as_str(), stalled, promote_due, s.capture_events, s.total_bytes
            );
            WatchdogSnapshot {
                mode: s.mode,
                pid: s.pid,
                target_node: s.target_node,
                stalled,
                promote_due,
            }
        };

        // ── 无锁区：重新枚举 + 决策 ────────────────────────────
        let topo = match enumerate_topology(500) {
            Ok(t) => t,
            Err(e) => {
                audio_log!("[audio] watchdog: 重枚举失败: {}", e);
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.last_error = Some(format!("watchdog enum: {}", e));
                continue;
            }
        };

        match snap.mode {
            CaptureMode::PerApp => watchdog_perapp(&state, &topo, snap),
            CaptureMode::MonitorFallback => watchdog_monitor(&state, &topo, snap),
        }
    }
}

/// per-app 重验（每轮都做，见 watchdog_loop 注释）：
/// 应用重开流（节点 id 变了）→ 重连最新节点；节点消失 → 计入失败，
/// 连续达阈值降级 monitor；节点还在且 id 一致 → 链路有效（无数据只是静音）。
fn watchdog_perapp(state: &Arc<Mutex<AudioCaptureState>>, topo: &PwTopology, snap: WatchdogSnapshot) {
    match find_app_stream_node(topo, snap.pid) {
        Some((node_id, node_name, _)) if node_id != snap.target_node => {
            audio_log!(
                "[audio] watchdog: per-app 节点更新 {} → {}（应用重开流），重连",
                snap.target_node, node_id
            );
            match connect_capture(state, CaptureMode::PerApp, node_id, &node_name, None) {
                Ok(()) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.reconnect_failures = 0;
                    s.total_reconnects += 1;
                    audio_log!("[audio] watchdog: per-app 重连成功 node={}", node_id);
                }
                Err(e) => note_reconnect_failure(state, format!("per-app reconnect: {}", e)),
            }
        }
        Some((_, _, _)) => {
            // 节点还在、id 没变 → 直连链路指向的就是它。持续无数据只可能是
            // 应用静音（不是断链：daemon 若已把我们挪走，节点消失轮会先命中）。
            // 不清零失败计数没有意义——目标有效即清零；静音只播报一次。
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.reconnect_failures = 0;
            if snap.stalled && !s.silence_logged {
                s.silence_logged = true;
                audio_log!(
                    "[audio] watchdog: per-app 节点 {} 存活但无数据（应用静音？）— 保持链路",
                    snap.target_node
                );
            }
        }
        None => {
            // 应用节点消失（进程退了/流关了）→ 计入失败，达阈值降级
            audio_log!("[audio] watchdog: pid {} 播放流节点已消失", snap.pid);
            note_reconnect_failure(state, "app stream node gone".to_string());
        }
    }
}

/// monitor-fallback：定时探测该 pid 是否已出现应用流（应用开始出声）→ 升级
/// 回 per-app；否则 monitor 也断流时重连 sink（sink 一般不消失，少见）。
fn watchdog_monitor(state: &Arc<Mutex<AudioCaptureState>>, topo: &PwTopology, snap: WatchdogSnapshot) {
    // 升级探测：应用现在有播放流节点了 → 切回 per-app
    if snap.promote_due {
        if let Some((node_id, node_name, _)) = find_app_stream_node(topo, snap.pid) {
            audio_log!(
                "[audio] watchdog: 应用出现播放流 node={}，从 monitor 升级到 per-app",
                node_id
            );
            match connect_capture(state, CaptureMode::PerApp, node_id, &node_name, None) {
                Ok(()) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.total_reconnects += 1;
                    s.reconnect_failures = 0;
                    s.fallback_reason = None;
                }
                Err(e) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.last_error = Some(format!("promotion: {}", e));
                }
            }
            return;
        }
    }

    // monitor 也持续无数据 → 重枚举后重连 sink（sink 消失/换默认设备的情况）
    if snap.stalled {
        match find_default_sink(topo) {
            Ok((sink_node, sink_name)) if sink_node != snap.target_node => {
                audio_log!(
                    "[audio] watchdog: monitor sink 变化 {} → {}，重连",
                    snap.target_node, sink_node
                );
                if let Err(e) = connect_capture(
                    state,
                    CaptureMode::MonitorFallback,
                    sink_node,
                    &sink_name,
                    snap_reason(state),
                ) {
                    note_reconnect_failure(state, format!("monitor reconnect: {}", e));
                }
            }
            Ok(_) => {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                if !s.silence_logged {
                    s.silence_logged = true;
                    audio_log!("[audio] watchdog: monitor sink 存活但无数据（整机静音）");
                }
            }
            Err(e) => note_reconnect_failure(state, format!("monitor enum sink: {}", e)),
        }
    }
}

/// per-app 重连失败计数；达 MAX_RECONNECT_FAILURES 降级 monitor-fallback。
fn note_reconnect_failure(state: &Arc<Mutex<AudioCaptureState>>, err: String) {
    let (failures, should_fallback, topo_pid) = {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.last_error = Some(err.clone());
        s.reconnect_failures += 1;
        (
            s.reconnect_failures,
            s.reconnect_failures >= MAX_RECONNECT_FAILURES,
            s.pid,
        )
    };
    audio_log!(
        "[audio] watchdog: 失败 {}/{} — {}",
        failures,
        MAX_RECONNECT_FAILURES,
        err
    );
    if !should_fallback {
        return;
    }
    // 降级：连当前 sink（用新枚举，避免用过期的失败目标）
    let topo = match enumerate_topology(400) {
        Ok(t) => t,
        Err(e) => {
            audio_log!("[audio] watchdog: 降级前枚举失败: {}", e);
            return;
        }
    };
    let reason = format!("per-app 连续失败 {} 次（最后: {}），降级整机", failures, err);
    match connect_monitor_fallback(state, &topo, reason) {
        Ok(()) => {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            // 降级后短期不再自动升级回来（应用可能反复重开导致抖动），
            // 但保留 promotion 探测——设 promotion_disabled=false 让 watchdog
            // 继续按 PROMOTION_MS 尝试切回 per-app（一旦真稳定出声就升级）。
            s.promotion_disabled = false;
            s.reconnect_failures = 0;
            audio_log!("[audio] watchdog: 已降级 monitor-fallback (pid={})", topo_pid);
        }
        Err(e) => {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.last_error = Some(format!("fallback failed: {}", e));
            // 连 sink 都没有 → 放弃后续升级，避免死循环枚举
            s.promotion_disabled = true;
            audio_log!("[audio] watchdog: 降级也失败: {} — 暂停自动升级", e);
        }
    }
}

/// 读取当前 fallback_reason（monitor 重连时保留原降级原因）。
fn snap_reason(state: &Arc<Mutex<AudioCaptureState>>) -> Option<String> {
    state.lock().unwrap_or_else(|e| e.into_inner()).fallback_reason.clone()
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

    /// 生成 10s 440Hz 正弦 wav（48k stereo S16LE）到临时目录。
    fn write_test_wav(path: &std::path::Path) -> std::io::Result<()> {
        const RATE: u32 = 48000;
        const CH: u16 = 2;
        const SECS: u32 = 10;
        let n_frames = RATE * SECS;
        let data_len = n_frames as usize * CH as usize * 2;
        let mut pcm = Vec::with_capacity(data_len);
        for i in 0..n_frames as usize {
            let v = (12000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / RATE as f64).sin()) as i16;
            pcm.extend_from_slice(&v.to_le_bytes());
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        let file_len = 36 + data_len as u32;
        let mut out: Vec<u8> = Vec::with_capacity(44 + data_len);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&file_len.to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&CH.to_le_bytes());
        out.extend_from_slice(&RATE.to_le_bytes());
        out.extend_from_slice(&(RATE * CH as u32 * 2).to_le_bytes()); // byte rate
        out.extend_from_slice(&(CH * 2).to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        out.extend_from_slice(&pcm);
        std::fs::write(path, out)
    }

    /// 端到端 live 验证（本机 PipeWire）：
    /// paplay 起一条真实播放流 → start_audio_capture(该 pid) →
    /// 断言 mode=per-app 且 process 回调产出 PCM 字节；再验证未知 pid
    /// 走 monitor-fallback。paplay 不存在时跳过（不误报）。
    ///
    /// 注意：验证的是"捕获管道拿到该应用流的字节"，不代表任何人听到了声音。
    #[test]
    #[ignore = "需要运行中的 PipeWire 守护 + paplay"]
    fn per_app_capture_live_e2e() {
        if std::process::Command::new("paplay").arg("--version").output().is_err() {
            eprintln!("SKIP: paplay 不存在，无法制造真实播放流");
            return;
        }
        let wav = std::env::temp_dir().join("waylandcraft-audio-test-tone.wav");
        write_test_wav(&wav).expect("write wav");

        // -r 循环播放，避免测试窗口内流自然结束
        let mut child = std::process::Command::new("paplay")
            .arg("-r")
            .arg(&wav)
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("spawn paplay");
        let pid = child.id();
        eprintln!("paplay pid={pid}");

        // 等 PipeWire 出现该 pid 的 Stream/Input 节点（最多 5s）
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut found = None;
        while std::time::Instant::now() < deadline {
            let topo = enumerate_topology(300).expect("enum");
            if let Some(n) = find_app_stream_node(&topo, pid) {
                found = Some(n);
                break;
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        let (node_id, node_name, class) = found.expect("paplay 的 Stream/Input 节点未出现");
        eprintln!("app node: id={node_id} class={class} name={node_name}");

        // per-app 捕获
        start_audio_capture(pid).expect("start per-app capture");
        let status0 = get_audio_capture_status();
        eprintln!("status@start: {status0}");
        assert!(status0.contains(r#""mode":"per-app""#), "应为 per-app: {status0}");
        let want_target = format!("\"target_node\":{},", node_id);
        assert!(status0.contains(&want_target), "target 应为应用节点: {status0}");

        // 等 process 回调产数据（最多 4s）
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        let mut got = 0usize;
        while std::time::Instant::now() < deadline {
            if let Some(p) = poll_audio_capture() {
                got += p.len().saturating_sub(8);
                if got > 4096 {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let status1 = get_audio_capture_status();
        eprintln!("status@data: {status1}");
        stop_audio_capture();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&wav);

        assert!(got > 0, "per-app 捕获 process 回调无数据（status: {status1}）");
        assert!(status1.contains(r#""mode":"per-app""#), "捕获中 mode 应稳定为 per-app: {status1}");

        // 未知 pid → monitor-fallback（本环境有 sink）
        let bogus = u32::MAX;
        start_audio_capture(bogus).expect("start fallback capture");
        let status2 = get_audio_capture_status();
        eprintln!("status@fallback: {status2}");
        stop_audio_capture();
        assert!(
            status2.contains(r#""mode":"monitor-fallback""#) && status2.contains("fallback_reason"),
            "未知 pid 应降级 monitor-fallback 并带 reason: {status2}"
        );
    }

    /// watchdog 生命周期 live 验证（本机 PipeWire，全程 ~40s）：
    /// 1. per-app 捕获有数据；
    /// 2. 杀掉应用 → 节点消失 → 连续失败后自动降级 monitor-fallback（带 reason）；
    /// 3. 会话启动时应用还没有节点（sh sleep→exec paplay，pid 不变）→
    ///    启动即 monitor-fallback，节点出现后 watchdog 自动升级回 per-app。
    #[test]
    #[ignore = "需要运行中的 PipeWire 守护 + paplay；全程约 40s"]
    fn watchdog_fallback_promotion_live() {
        if std::process::Command::new("paplay").arg("--version").output().is_err() {
            eprintln!("SKIP: paplay 不存在");
            return;
        }
        let wav = std::env::temp_dir().join("waylandcraft-audio-test-tone.wav");
        write_test_wav(&wav).expect("write wav");

        // ── 1. per-app 有数据 ─────────────────────────────────
        let mut child = std::process::Command::new("paplay")
            .arg("-r")
            .arg(&wav)
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("spawn paplay");
        let pid = child.id();
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        loop {
            if find_app_stream_node(&enumerate_topology(300).unwrap(), pid).is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "paplay 节点未出现");
            std::thread::sleep(Duration::from_millis(400));
        }
        start_audio_capture(pid).expect("start");
        let st = get_audio_capture_status();
        assert!(st.contains(r#""mode":"per-app""#), "初始应 per-app: {st}");
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        loop {
            if poll_audio_capture().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "per-app 无数据: {}",
                get_audio_capture_status()
            );
            std::thread::sleep(Duration::from_millis(300));
        }
        eprintln!("[test] stage1 per-app streaming OK");

        // ── 2. 应用消失 → 自动降级 monitor-fallback ───────────
        let _ = child.kill();
        let _ = child.wait();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut degraded = String::new();
        while std::time::Instant::now() < deadline {
            let st = get_audio_capture_status();
            eprintln!("[test] stage2 poll: {st}");
            if st.contains(r#""mode":"monitor-fallback""#) {
                degraded = st;
                break;
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
        assert!(
            !degraded.is_empty() && degraded.contains("fallback_reason"),
            "应用消失后应降级 monitor-fallback（带 reason）: {degraded}"
        );
        eprintln!("[test] stage2 degraded OK: {degraded}");

        // ── 3. 节点后到 → watchdog 自动升级回 per-app ─────────
        stop_audio_capture();
        // sh 先 sleep 再起 paplay（exec 不换 pid）：捕获会话启动时该 pid
        // 必然还没有播放流节点 → 先 monitor-fallback，节点出现后升级。
        let mut child2 = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 2; exec paplay -r \"$1\"")
            .arg("wc-delayed")
            .arg(&wav)
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("spawn delayed paplay");
        let pid2 = child2.id();
        assert!(
            find_app_stream_node(&enumerate_topology(300).unwrap(), pid2).is_none(),
            "前置条件失败：sleep 期间不该已有 pid2 节点"
        );
        start_audio_capture(pid2).expect("start2");
        let st2 = get_audio_capture_status();
        eprintln!("[test] stage3 start status: {st2}");
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let mut promoted = String::new();
        while std::time::Instant::now() < deadline {
            let st = get_audio_capture_status();
            if st.contains(r#""mode":"per-app""#) && st.contains(&format!("\"pid\":{},", pid2)) {
                promoted = st;
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        stop_audio_capture();
        let _ = child2.kill();
        let _ = child2.wait();
        let _ = std::fs::remove_file(&wav);
        assert!(!promoted.is_empty(), "节点出现后应自动升级回 per-app: {promoted}");
        eprintln!("[test] stage3 promoted OK: {promoted}");
    }

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
