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
// 断流自愈：per-app 直连的节点会随应用重开流/退出而消失。会话内注册的循环
// timer 定期重验目标节点存在性与 id（应用重开流 id 会变 → 重连最新节点），并辅以
// "process 回调 ~2s 无字节" 的断流信号；连续失败降级 monitor-fallback；
// 全部重试有节流 + 指数退避。
// 注意（本机 PipeWire 1.4 实测）：目标节点消失后 daemon 会把孤儿 capture
// stream 重新路由到默认 sink monitor 并继续全速投递（静音）数据——只看字节流
// 会误判"捕获正常"，所以存在性重验是主判据，0 字节只是辅助信号。
//
// 已知局限：单进程多窗口应用（如 Firefox 多标签）的声音按进程合并 ——
// PipeWire 层面无法细分到窗口，这是粒度极限。
//
// ── 线程模型（hs_err_pid8053 崩溃修复后的单一所有者模型）──────────────
// 崩溃复盘：旧实现里 capture stream / mainloop / listener 的生命周期横跨多个
// 线程——Java stop 线程与 watchdog 线程各自 `pw_main_loop_quit` 后只 sleep
// 150ms 就**在自己线程上 drop pw 对象**，而 mainloop 线程可能仍在
// `pw_main_loop_run` 里引用它们；重连又整套更换 mainloop/context/core。
// hs_err：SIGSEGV@libspa-support.so+0x7030（循环 source 迭代解引用一个已被
// 字符串数据覆写的指针——典型 use-after-free），崩溃线程 "native thread"。
//
// 新模型铁律：**capture 会话的所有 pw 对象只存在于 owner 线程**
// （"wc-audio-main"），只在该线程创建/操作/销毁：
//   - owner 线程跑 pw_main_loop_run；MainLoop/Context/Core/Registry/
//     Node proxies/Stream/StreamListener 全部由它持有；收到 Quit 先
//     mainloop.quit()，run 返回后在本线程按 依赖反向 释放（stream listener →
//     stream → registry listener → registry → core → context）。
//     任何 pw 对象都不进入跨线程共享状态。
//   - 外部线程（Java JNI / 测试）与 owner 的通信只有两条线程安全通道：
//     mpsc 命令队列 + eventfd 唤醒（owner 用 pw_loop add_io 注册读端；
//     Java 只 write 计数 + send 命令，永不调用 pw）。
//   - watchdog / 重连 / 降级 / 升级 = owner 线程上 pw_loop add_timer 注册的
//     周期回调（500ms tick），在循环线程内直接执行——不存在跨线程 pw 调用。
//   - 拓扑（node → pid/class/name）由会话内**常驻 Registry 监听**实时维护
//     （global / global_remove / core.done），不再"另起一条 pw 连接做枚举"
//     （旧 watchdog 每轮新建 mainloop+context+core 同样是跨线程抖动源）。
//   - stop：Java 置 active=false → 发 Quit → 唤醒 → join owner 线程。
//     join 返回即 owner 线程已完成全部 pw 对象释放——不存在旧实现
//     "sleep(150ms) 赌线程已退出"的竞态。
//   - start/stop 全程被 SESSION_LIFECYCLE 互斥量串行化：任意时刻最多一个
//     owner 线程活着，两个会话不可能并存。
//   - poll 的 pcm 交换经 Mutex 原子完成；共享状态里只有纯数据（Vec<u8> +
//     数值/字符串，PCM 带 PCM_BUFFER_CAP_BYTES 环形上限防 poll 停摆 OOM），
//     Send+Sync 由类型系统保证（静态测试断言）。
//
// `enumerate_topology`（独立连接、调用线程自持自放 + quit 回执 join）仅供
// 测试/诊断使用，不参与捕获会话生命周期。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pipewire as pw;

/// [audio] 日志宏（**分级=里程碑/失败行**：start/stop 结果、stage 1-4
/// 摘要、FALLBACK、watchdog 状态迁移与失败——任何档位都写。分级表见
/// docs/LOGGING.md）。同时写 stderr 和 audio 日志文件（bridge::audio_log_write）。
macro_rules! audio_log {
    ($($arg:tt)*) => {
        crate::bridge::audio_log_write(&format!($($arg)*))
    };
}

/// [audio] 细节级日志（poll 周期行：watchdog round、capture ongoing
/// 计数、拓扑逐节点 dump）：发布档在判定处短路——不 format、不写。
macro_rules! audio_log_v {
    ($($arg:tt)*) => {{
        if $crate::bridge::verbose_logging() {
            $crate::bridge::audio_log_write(&format!($($arg)*));
        }
    }};
}

/// 全局捕获会话（native 侧单例，一次只允许一个音频捕获会话）。
/// 只在 SESSION_LIFECYCLE 锁内替换；其中没有任何 pw 对象。
static AUDIO_CAPTURE: Mutex<Option<Arc<RunningSession>>> = Mutex::new(None);

/// start/stop 串行化：保证"同一时刻至多一个 owner 线程"，并且 stop 的 join
/// 与下一个 start 的 spawn 之间不会交错（旧实现此处靠 sleep 150ms 兜底）。
static SESSION_LIFECYCLE: Mutex<()> = Mutex::new(());

/// owner 线程名（hs_err 线程列表 / ps 定位用）。
const SESSION_THREAD_NAME: &str = "wc-audio-main";

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
/// process 回调持续无数据超过该时长 → 判定断流，触发重验
const STALL_MS: u64 = 2000;
/// 相邻两次断流检查的最小间隔（节流）
const CHECK_THROTTLE_MS: u64 = 1500;
/// per-app 模式下"目标节点存在性"定期重验周期（数据正常时也用，见 watchdog 注释）
const REVALIDATE_MS: u64 = 2000;
/// 断流但目标节点仍在（应用只是静音）时的退避上限，避免长时间静音下高频轮询
const MAX_BACKOFF_MS: u64 = 15_000;
/// monitor-fallback 下尝试升级回 per-app 的周期（应用可能稍后才出声）
const PROMOTION_MS: u64 = 5_000;
/// per-app 连续失败达到该次数 → 降级 monitor-fallback
const MAX_RECONNECT_FAILURES: u32 = 3;
/// 循环 timer 轮询周期（原 watchdog 线程的 500ms sleep 步长，行为一致）
const TICK_MS: u64 = 500;
/// start 后等待 registry 初始同步（core.done）的上限；超时用现有拓扑尽力开始
const START_SYNC_BUDGET_MS: u64 = 1500;
/// start 等 owner 线程就绪（boot）的上限
const BOOT_BUDGET_MS: u64 = 10_000;
/// start 等连接结果回执的上限（>= START_SYNC_BUDGET + pw 操作余量）
const START_ACK_BUDGET_MS: u64 = 15_000;
/// stop join owner 线程的兜底上限（正常 <50ms；超时说明 pw daemon stall，
/// 记录后放弃等待——即便超时也不存在跨线程 pw 操作：对象仍由 owner 线程
/// 自行释放，外部线程只是不再等它）。
const STOP_JOIN_BUDGET_MS: u64 = 10_000;

/// 单流 PCM 缓冲上限（字节）。process 回调只 append，消费靠 Java poll 取走；
/// 若 poll 停摆（GC 长暂停 / 渲染卡顿 / 断线），缓冲原本会无限增长直到 OOM。
/// 依据：48kHz × 2ch × 16bit(S16LE) = 192,000 B/s，2 秒 PCM 仅 ~384KB；Java
/// 正常 poll 间隔为亚秒级（渲染循环每帧取走一次）。取 8MB ≈ 43s 音频余量，
/// 即把最坏情形（poll 彻底停摆）的内存上界钉死在单流 8MB，超限丢最旧保最新。
const PCM_BUFFER_CAP_BYTES: usize = 8 * 1024 * 1024;

/// 跨线程共享的捕获状态：累积 PCM 缓冲 + 诊断字段。
/// **只允许线程安全纯数据**——历史上 `_stream/_mainloop/_listener` 字段 +
/// `unsafe impl Send` 就是本次崩溃的载体（pw 对象被 move 进 state 后由任意
/// 调 stop/重连的线程销毁）。本类型 Send+Sync 由静态测试
/// `shared_state_is_send_sync_by_construction` 把关。
pub struct AudioCaptureState {
    pub pcm: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
    pub active: bool,
    /// 诊断计数：process 回调触发次数 / 累计捕获字节
    pub capture_events: u64,
    pub total_bytes: u64,
    /// PCM 环形上限触发后从最旧端丢弃的累计字节数（透出状态 JSON
    /// pcm_overflow_drops，供 /wl audio status 观察 poll 停摆丢数据）。
    pub pcm_overflow_drops: u64,
    /// "溢出丢数据"只播报一次，防 poll 长时间停摆时日志刷屏
    /// （poll 取空缓冲后复位 → 下一次溢出事件再报一行）。同 silence_logged 模式。
    pub pcm_overflow_logged: bool,
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

/// 计算为维持 PCM 环形上限（cap）需从缓冲**最旧端**丢弃的字节数。
/// 纯函数，便于单测：
/// - `Some(0)`：未溢出，整块追加；
/// - `Some(n)`：先从头部丢 n 字节再追加 incoming（append 后 len 恒 <= cap）；
/// - `None`：单块 incoming 本身 >= cap（正常 PipeWire quantum 不可能，
///   防御分支）——整块丢弃不追加，保持缓冲长度不变式。
fn pcm_ring_drop_for(buffered: usize, incoming: usize, cap: usize) -> Option<usize> {
    if incoming >= cap {
        return None;
    }
    Some((buffered + incoming).saturating_sub(cap))
}

impl AudioCaptureState {
    fn new(pid: u32) -> Self {
        let now = Instant::now();
        Self {
            pcm: Vec::new(),
            sample_rate: 48000,
            channels: 2,
            active: true,
            capture_events: 0,
            total_bytes: 0,
            pcm_overflow_drops: 0,
            pcm_overflow_logged: false,
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

    /// process 回调的字节入口：追加 PCM 并维护环形上限（超限丢最旧保最新）。
    /// 返回本次丢弃的字节数（0 = 未溢出）。**调用方必须已持有 shared 锁**
    /// （owner 线程 process 回调 / 测试）——缓冲操作全部在锁内完成，
    /// 不引入任何跨线程 pw/缓冲访问（v1.3.0-sync7 单 owner 线程模型不变）。
    fn push_pcm(&mut self, src: &[u8]) -> u64 {
        match pcm_ring_drop_for(self.pcm.len(), src.len(), PCM_BUFFER_CAP_BYTES) {
            Some(0) => {
                self.pcm.extend_from_slice(src);
                0
            }
            Some(n) => {
                self.pcm.drain(..n);
                self.pcm.extend_from_slice(src);
                let dropped = n as u64;
                self.pcm_overflow_drops += dropped;
                dropped
            }
            // 防御分支：单块 >= 上限，整块丢弃不追加（缓冲长度不变式保持）
            None => {
                let dropped = src.len() as u64;
                self.pcm_overflow_drops += dropped;
                dropped
            }
        }
    }
}

// ── 线程安全命令通道 ──────────────────────────────────────────────

/// 投递给 owner 线程的命令（只在 owner 线程的循环回调里被消费）。
enum Command {
    /// 启动捕获：pid + 结果回执（owner 线程 send，start 调用方 recv）
    Start(u32, Sender<Result<(), String>>),
    /// 停止并退出 owner 线程
    Quit,
}

/// 已启动会话的线程安全句柄（存在 AUDIO_CAPTURE 全局里；不含 pw 对象）。
struct RunningSession {
    shared: Arc<Mutex<AudioCaptureState>>,
    cmd_tx: Sender<Command>,
    /// eventfd 写端 dup（与 owner 线程注册进 pw_loop 的读端同一 open file
    /// description）。写计数器唤醒循环——外部线程唯一允许的"触 pw"动作。
    wake_tx: OwnedFd,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl RunningSession {
    fn wake(&self) {
        let one: [u8; 8] = 1u64.to_ne_bytes();
        // eventfd write：单写者计数递增；读端在循环线程 drain。
        let _ = unsafe { libc::write(self.wake_tx.as_raw_fd(), one.as_ptr().cast(), 8) };
    }
}

/// owner 线程注册进 pw_loop 的唤醒 fd（读端；回调里把计数读干）。
struct WakeFd(OwnedFd);

impl AsRawFd for WakeFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// 读干 eventfd 计数（EAGAIN 即止）。
fn drain_wake(fd: RawFd) {
    let mut buf = [0u8; 8];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), 8) };
        if n <= 0 {
            break;
        }
    }
}

/// 创建 eventfd（NONBLOCK|CLOEXEC）。
fn make_wake_eventfd() -> Result<OwnedFd, String> {
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if fd < 0 {
        return Err(format!("eventfd: {}", std::io::Error::last_os_error()));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn dup_fd(fd: &OwnedFd) -> Result<OwnedFd, String> {
    fd.try_clone().map_err(|e| format!("eventfd clone: {}", e))
}

// ── 拓扑（owner 线程常驻 registry 实时维护） ──────────────────────

/// PipeWire 节点拓扑快照
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

/// 拓扑 + 已 bind 的 Node proxies。**与 Session 分开**放独立 Rc<RefCell>：
/// registry 事件回调只碰这个 cell，begin/connect 流程只碰 Session——避免
/// pw 事件上下文与重连流程互踩 RefCell 借用。
#[derive(Default)]
struct TopoState {
    topo: PwTopology,
    bound: HashMap<u32, (Box<dyn pw::proxy::ProxyT>, Box<dyn pw::proxy::Listener>)>,
}

/// owner 线程内部会话状态——**全部 pw 对象只在这里出现、只在这个线程访问**。
struct Session {
    shared: Arc<Mutex<AudioCaptureState>>,
    cmd_rx: Receiver<Command>,
    /// 写端 dup（owner 线程自唤醒用：core.done 回调只置 flag + 写 eventfd，
    /// 把"开始捕获"排到 io 回调统一处理，不在 pw 事件上下文里做重操作）。
    wake_self_fd: RawFd,
    _wake_self_keep: OwnedFd,
    /// registry 初始同步标志（core.done 事件置位；Rc<Cell> 让 done 闭包不必
    /// 借用 Session）。
    synced_flag: Rc<Cell<bool>>,
    _context: Option<pw::context::Context>,
    core: Option<pw::core::Core>,
    registry: Option<Rc<pw::registry::Registry>>,
    _registry_listener: Option<pw::registry::Listener>,
    _core_listener: Option<pw::core::Listener>,
    /// capture 会话对象（重连 = 在本线程 drop 旧的再建新的，复用 core）
    cap_stream: Option<pw::stream::Stream>,
    cap_listener: Option<pw::stream::StreamListener<()>>,
    pending_start: Option<(u32, Sender<Result<(), String>>, Instant)>,
    quit: bool,
}

// ── owner 线程主体 ────────────────────────────────────────────────

/// owner 线程主体：pw 对象在这里创建、在这里销毁；外部世界只有
/// cmd_rx + eventfd + shared 状态。
fn session_main(
    cmd_rx: Receiver<Command>,
    wake_for_loop: WakeFd,
    wake_self_fd: RawFd,
    wake_self_keep: OwnedFd,
    shared: Arc<Mutex<AudioCaptureState>>,
    boot_tx: Sender<Result<(), String>>,
) {
    pw::init();

    let mainloop = match pw::main_loop::MainLoop::new(None) {
        Ok(m) => m,
        Err(e) => {
            let _ = boot_tx.send(Err(format!("mainloop: {}", e)));
            return;
        }
    };

    let synced_flag = Rc::new(Cell::new(false));
    let sess_rc = Rc::new(RefCell::new(Session {
        shared: shared.clone(),
        cmd_rx,
        wake_self_fd,
        _wake_self_keep: wake_self_keep,
        synced_flag: synced_flag.clone(),
        _context: None,
        core: None,
        registry: None,
        _registry_listener: None,
        _core_listener: None,
        cap_stream: None,
        cap_listener: None,
        pending_start: None,
        quit: false,
    }));
    let topo_rc = Rc::new(RefCell::new(TopoState::default()));

    if let Err(e) = session_setup(&mainloop, &sess_rc, &topo_rc) {
        audio_log!("[audio] session setup failed: {}", e);
        // 失败路径：可能已建了部分 pw 对象——仍在本线程、run 从未开始，
        // 同线程丢弃即可，绝不留给别的线程。
        release_pw_objects(&sess_rc, &topo_rc);
        let _ = boot_tx.send(Err(e));
        return;
    }

    let raw_loop = mainloop.loop_();
    let ml_ptr = mainloop.as_raw_ptr();

    // 命令通道唯一消费入口：eventfd io source（循环线程回调）。
    let sess_io = sess_rc.clone();
    let topo_io = topo_rc.clone();
    let _wake_io = raw_loop.add_io(
        wake_for_loop,
        pw::spa::support::system::IoFlags::IN,
        move |fd| {
            drain_wake(fd.as_raw_fd());
            handle_commands(&sess_io, &topo_io, ml_ptr);
        },
    );

    // watchdog：循环 timer（500ms tick），替代原独立 watchdog 线程——
    // 重枚举判据、重连、降级、升级全部在循环线程内执行。
    let sess_t = sess_rc.clone();
    let topo_t = topo_rc.clone();
    let timer = raw_loop.add_timer(move |_expirations| {
        watchdog_tick(&sess_t, &topo_t);
    });
    if timer
        .update_timer(
            Some(Duration::from_millis(TICK_MS)),
            Some(Duration::from_millis(TICK_MS)),
        )
        .into_result()
        .is_err()
    {
        audio_log!("[audio] watchdog timer arm 失败（无断流自愈）");
    }

    audio_log!("[audio] session owner thread started (watchdog tick={}ms)", TICK_MS);
    let _ = boot_tx.send(Ok(()));

    mainloop.run();

    // run 返回后：本线程已不会再被 pw 回调进入。统一释放。
    audio_log!("[audio] mainloop exited; releasing pw objects on owner thread");
    release_pw_objects(&sess_rc, &topo_rc);
    // 局部 drop 顺序（逆声明序）：timer/_wake_io（source 销毁，loop 仍活）
    // → topo_rc / sess_rc（pw 对象已在 release_pw_objects 里清空）→ mainloop。
}

/// 在 owner 线程建立 context/core/registry + 常驻监听。
fn session_setup(
    mainloop: &pw::main_loop::MainLoop,
    sess_rc: &Rc<RefCell<Session>>,
    topo_rc: &Rc<RefCell<TopoState>>,
) -> Result<(), String> {
    let context = pw::context::Context::new(mainloop).map_err(|e| format!("context: {}", e))?;
    let core = context.connect(None).map_err(|e| format!("core connect: {}", e))?;

    // ⑤⑦ 沿用已验证教训（git 历史）：
    //  - Registry 必须具名保活到循环结束（Drop 会 pw_proxy_destroy；监听器
    //    挂早 = 0 个 global，即旧版"找不到默认 sink"真根因）。
    //  - PipeWire ≥1.0 registry global props **不含 application.process.id**
    //    （实测 1.4.2），必须 bind Node proxy 从 info 事件补 pid
    //    （PulseAudio 应用的 node 由 pipewire-pulse 进程创建，client pid
    //    不是目标应用的，不能用 client.id 代替）。
    //  - proxy+listener 保活在 topo_rc.bound；global_remove 时剔除。
    // ⑦新模型：registry 常驻整个会话——watchdog 不再另起一条 pw 连接枚举。
    let registry = Rc::new(core.get_registry().map_err(|e| format!("registry: {}", e))?);
    let reg_weak = Rc::downgrade(&registry);

    let topo_c = topo_rc.clone();
    let topo_c_remove = topo_rc.clone();
    let reg_listener = registry
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
                let mut t = topo_c.borrow_mut();
                t.topo.nodes.insert(node_id, (media_class.clone(), pid_from_global, name, proc_name));
                if pid_from_global != 0 || !node_worth_binding(&media_class) {
                    return;
                }
                if t.bound.contains_key(&node_id) {
                    return; // 重同步的重复 global 事件不重复 bind
                }
            }
            let Some(reg) = reg_weak.upgrade() else { return };
            if let Ok(node) = reg.bind::<pw::node::Node, _>(global) {
                let topo_c2 = topo_c.clone();
                let nl = node
                    .add_listener_local()
                    .info(move |info| {
                        let Some(props) = info.props() else { return };
                        let Some(v) = props.get("application.process.id") else { return };
                        let Ok(pid) = v.parse::<u32>() else { return };
                        if let Some(entry) = topo_c2.borrow_mut().topo.nodes.get_mut(&node_id) {
                            entry.1 = pid;
                        }
                    })
                    .register();
                topo_c
                    .borrow_mut()
                    .bound
                    .insert(node_id, (Box::new(node), Box::new(nl)));
            }
        })
        .global_remove(move |id| {
            // 节点消失：拓扑立即剔除 + 本线程 drop Node proxy/listener。
            // （旧 watchdog 靠"500ms 枚举一次"发现节点消失，这里事件驱动，
            //  判据更快且不再触碰第二条 pw 连接。）
            let mut t = topo_c_remove.borrow_mut();
            t.topo.nodes.remove(&id);
            t.bound.remove(&id);
        })
        .register();

    // core.done = registry 初始同步完成。回调不碰 Session（避免与 begin
    // 流程互踩借用、也不在 pw 事件上下文里重入建流）：只置 flag + 自唤醒。
    let wake_fd_raw = sess_rc.borrow().wake_self_fd;
    let flag = sess_rc.borrow().synced_flag.clone();
    let core_listener = core
        .add_listener_local()
        .done(move |_id, _seq| {
            flag.set(true);
            let one: [u8; 8] = 1u64.to_ne_bytes();
            unsafe {
                libc::write(wake_fd_raw, one.as_ptr().cast(), 8);
            }
        })
        .register();

    let mut sess = sess_rc.borrow_mut();
    sess._context = Some(context);
    sess.core = Some(core);
    sess.registry = Some(registry);
    sess._registry_listener = Some(reg_listener);
    sess._core_listener = Some(core_listener);
    Ok(())
}

/// owner 线程退出时的 pw 对象释放（依赖反向；此刻 run() 已返回，
/// 不可能再有 pw 回调引用它们）。
fn release_pw_objects(sess_rc: &Rc<RefCell<Session>>, topo_rc: &Rc<RefCell<TopoState>>) {
    {
        let mut sess = sess_rc.borrow_mut();
        sess.cap_listener = None;
        sess.cap_stream = None;
        sess._registry_listener = None;
        sess._core_listener = None;
        sess.registry = None;
        sess.pending_start = None;
        sess.core = None; // pw_proxy_destroy（owner 线程、循环已停）
        sess._context = None; // pw_context_destroy
    }
    let mut t = topo_rc.borrow_mut();
    t.bound.clear();
    t.topo.nodes.clear();
}

/// 消费命令队列（eventfd io 回调 / 循环线程内）。
fn handle_commands(
    sess_rc: &Rc<RefCell<Session>>,
    topo_rc: &Rc<RefCell<TopoState>>,
    ml_ptr: *mut pw::sys::pw_main_loop,
) {
    loop {
        let cmd = match sess_rc.borrow().cmd_rx.try_recv() {
            Ok(c) => Some(c),
            Err(mpsc::TryRecvError::Empty) => None,
            // 通道断开 = 所有句柄已 drop（外部世界不会再发 Quit）：等价 Quit。
            Err(mpsc::TryRecvError::Disconnected) => Some(Command::Quit),
        };
        let Some(cmd) = cmd else { break };
        match cmd {
            Command::Start(pid, ack) => {
                let begin_now = {
                    let mut sess = sess_rc.borrow_mut();
                    if sess.quit {
                        let _ = ack.send(Err("session quitting".to_string()));
                        continue;
                    }
                    {
                        let Ok(mut s) = sess.shared.lock() else {
                            let _ = ack.send(Err("state lock poisoned".to_string()));
                            continue;
                        };
                        s.pid = pid;
                    }
                    sess.pending_start = Some((pid, ack, Instant::now()));
                    sess.synced_flag.get()
                };
                if begin_now {
                    try_begin_start(sess_rc, topo_rc);
                } else {
                    audio_log!(
                        "[audio] start 等待 registry 同步（core.done，上限 {}ms）",
                        START_SYNC_BUDGET_MS
                    );
                }
            }
            Command::Quit => {
                {
                    let mut sess = sess_rc.borrow_mut();
                    sess.quit = true;
                    sess.pending_start = None;
                }
                // 先在同一循环上下文里丢弃 capture 对象，再 quit。
                // pw_stream_destroy 在主循环线程执行——正是崩溃修复点。
                release_captured_stream(sess_rc);
                // SAFETY: 在循环线程自身调用 pw_main_loop_quit 合法；
                // ml_ptr 指向本 session_main 栈上仍存活的 mainloop。
                unsafe { pw::sys::pw_main_loop_quit(ml_ptr) };
                // 通道可能已空：把剩余命令一次处理完
            }
        }
    }
}

/// 丢弃 capture stream/listener（重连前 / quit 时）。**owner 线程内**执行：
/// 此刻我们要么在循环回调里（pw 允许在主线程销毁本线程拥有的对象），
/// 要么在 run 返回后的收尾路径——两种情况都不存在并发 pw 回调。
fn release_captured_stream(sess_rc: &Rc<RefCell<Session>>) {
    let shared = {
        let mut sess = sess_rc.borrow_mut();
        sess.cap_listener = None; // 先注销 process 回调
        sess.cap_stream = None; // 再销毁 stream
        sess.shared.clone()
    };
    if let Ok(mut s) = shared.lock() {
        s.linked = false;
    }
}

/// start 请求 → 选目标 → 建 capture 会话。仅在循环线程、registry 同步后
/// （或同步预算超时后尽力）调用。
fn try_begin_start(sess_rc: &Rc<RefCell<Session>>, topo_rc: &Rc<RefCell<TopoState>>) {
    let Some((pid, ack, _since)) = sess_rc.borrow_mut().pending_start.take() else {
        return;
    };
    let topo = topo_rc.borrow().topo.clone();
    audio_log!("[audio] stage 1/4: registry 同步完成 — {} nodes", topo.nodes.len());

    // 分级=细节（拓扑逐节点 dump，节点数不定）→ verbose 档；上面的
    // stage 1/4 计数行与下面的 stage 摘要保留（启动摘要级）。
    if pid > 0 {
        for (id, (mc, _npid, name, _)) in topo.nodes.iter().filter(|(_, (_, p, _, _))| *p == pid) {
            audio_log_v!("[audio] pid节点: id={} class={} name={}", id, mc, name);
        }
    }
    for (id, (mc, npid, name, _)) in
        topo.nodes.iter().filter(|(_, (mc, _, _, _))| is_playback_stream_class(mc))
    {
        audio_log_v!("[audio] candidate app stream: id={} class={} pid={} name={}", id, mc, npid, name);
    }

    audio_log!("[audio] stage 2/4: 按 pid={} 匹配应用播放流节点（Stream/Input）...", pid);
    let app_node = find_app_stream_node(&topo, pid);

    let result = match &app_node {
        Some((node_id, node_name, media_class)) => {
            audio_log!(
                "[audio] stage 2/4: OK — per-app 目标 node={} class={} name={}",
                node_id, media_class, node_name
            );
            audio_log!("[audio] stage 3/4: 创建 capture stream（直连应用输出端口）...");
            match connect_capture(sess_rc, CaptureMode::PerApp, *node_id, node_name, None) {
                Ok(()) => Ok(()),
                Err(e) => {
                    // 目标节点在枚举与连接之间消失等竞态 → 直接降级 monitor
                    let msg = format!("per-app connect failed (node {}): {}", node_id, e);
                    audio_log!("[audio] stage 3/4: 直连失败: {}", msg);
                    connect_monitor_fallback(sess_rc, &topo, msg)
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
            connect_monitor_fallback(sess_rc, &topo, reason)
        }
    };

    match result {
        Ok(()) => {
            let mode = with_shared(sess_rc, |s| s.mode);
            audio_log!(
                "[audio] stage 4/4: OK — 捕获会话已启动（mode={}），等待 process 回调产出 PCM",
                mode.as_str()
            );
            let _ = ack.send(Ok(()));
        }
        Err(e) => {
            let msg = format!("[audio] 启动捕获失败: {}", e);
            audio_log!("{}", msg);
            crate::status::note_error("error", &msg);
            let _ = ack.send(Err(e));
        }
    }
}

/// 降级/回退到整机 sink monitor 捕获（要求 topo 已同步）。
/// 返回 Err = 连 sink 都找不到/连不上（整个启动失败）。
fn connect_monitor_fallback(
    sess_rc: &Rc<RefCell<Session>>,
    topo: &PwTopology,
    reason: String,
) -> Result<(), String> {
    let (sink_node, sink_name) =
        find_default_sink(topo).map_err(|e| format!("monitor fallback 也找不到默认 sink: {}", e))?;
    audio_log!(
        "[audio] FALLBACK monitor-fallback: sink node={} name={} — {}",
        sink_node,
        sink_name,
        reason
    );
    crate::status::note_error("warn", &format!("[audio] monitor fallback: {}", reason));
    connect_capture(sess_rc, CaptureMode::MonitorFallback, sink_node, &sink_name, Some(reason))
}

/// 建立（或重建）capture 会话并连到指定目标节点。**只允许 owner 线程调用**：
/// stream/listener 在本线程创建、连接、销毁；共享状态只写纯数据。
/// - target_id：per-app 时为应用播放流节点 id，monitor 时为 sink 节点 id。
///   SPA_KEY_TARGET_OBJECT 用数字 id（pw_core_find_object 同时支持名字/id，
///   id 更精确：多个 node 可能同名）。
/// - stream.capture.sink 不设（=false）：per-app 不经 sink，monitor 用默认 monitor。
/// - 内部自行丢弃旧 stream（同线程），调用方无需预 teardown。
fn connect_capture(
    sess_rc: &Rc<RefCell<Session>>,
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

    // 重连场景：在本线程丢弃旧 stream/listener（此刻就在循环线程里，
    // process 回调不可能并发执行——旧实现的"跨线程销毁"问题不复存在）。
    release_captured_stream(sess_rc);

    // 新建 stream（复用会话 core——不再每次重连都新开 mainloop/context/core）。
    let stream = {
        let sess = sess_rc.borrow();
        let core = sess.core.as_ref().ok_or("session core gone")?;
        pw::stream::Stream::new(
            core,
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
                "target.object" => target_id.to_string(),
            },
        )
        .map_err(|e| format!("stream: {}", e))?
    };

    let shared = { sess_rc.borrow().shared.clone() };
    let shared_process = shared.clone();
    let shared_state = shared.clone();
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
                                let mut s = shared_process.lock().unwrap();
                                if s.active {
                                    let incoming = src.len();
                                    let dropped = s.push_pcm(src);
                                    s.total_bytes += incoming as u64;
                                    s.capture_events += 1;
                                    // watchdog 的存活依据：收到数据的最新时刻
                                    s.last_data_at = Instant::now();
                                    if dropped > 0 && !s.pcm_overflow_logged {
                                        // 分级=失败行（poll 停摆丢数据）：每次
                                        // "溢出事件段"只报一行，poll 取空缓冲后复位。
                                        s.pcm_overflow_logged = true;
                                        log_line = Some(format!(
                                            "[audio] PCM buffer overflow: dropped {} bytes this event, {} total (cap={} bytes, Java poll 停摆？) — 后续丢弃仅累计 pcm_overflow_drops",
                                            dropped, s.pcm_overflow_drops, PCM_BUFFER_CAP_BYTES
                                        ));
                                    }
                                    if s.capture_events == 1 {
                                        // 分级=里程碑（首包 LIVE 证据，会话一次）：保留。
                                        log_line = Some(format!(
                                            "[audio] FIRST capture — {} bytes (process 回调 LIVE, mode={})",
                                            src.len(),
                                            s.mode.as_str()
                                        ));
                                    } else if s.capture_events % 2000 == 0
                                        && crate::bridge::verbose_logging()
                                    {
                                        // 分级=细节（process 周期计数行）→ verbose 档，
                                        // 判定在持锁热路径内联，跳过 format。
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
        // node id 要等 daemon 分发到 info/state 事件后才有效（connect 刚
        // 返回时恒为 ID_NONE——旧实现状态里 stream_node 一直是 -1 的原因）。
        // 回调在 owner 线程，读自己的 stream 合法。
        .state_changed(move |stream, _, _old, _new| {
            let id = stream.node_id();
            if id != u32::MAX {
                if let Ok(mut s) = shared_state.lock() {
                    if s.stream_node != id {
                        s.stream_node = id;
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

    let stream_node = stream.node_id(); // 可能仍 u32::MAX，state_changed 会补
    audio_log!(
        "[audio] connect_capture: our capture node={}（AUTOCONNECT → target {}）",
        stream_node,
        target_id
    );

    {
        let sess = sess_rc.borrow();
        if sess.quit {
            // 建立期间会话被 stop：对象只在本线程挂过，随本函数返回 drop，安全。
            return Err("session stopped during connect".to_string());
        }
        // 旧 stream 已在函数入口 release_captured_stream（本线程、锁外 pw 调用）。
    }
    {
        let mut sess = sess_rc.borrow_mut();
        sess.cap_stream = Some(stream);
        sess.cap_listener = Some(listener);
    }
    {
        let mut s = shared.lock().map_err(|e| format!("state lock: {}", e))?;
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

    Ok(())
}

// ── 对外 JNI 入口（Java 线程）：只投递命令 / join，绝不碰 pw 对象 ────

/// 启动音频捕获：优先 per-app 直连共享窗口应用的播放流节点；
/// 找不到（应用没出声 / PID 未知）→ monitor-fallback 整机捕获。
/// 全程被 SESSION_LIFECYCLE 串行化（与 stop 互斥）。
pub fn start_audio_capture(pid: u32) -> Result<(), String> {
    let _lifecycle = SESSION_LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // 确保没有正在运行的捕获：stop_locked join 完上一个 owner 线程后才继续。
    stop_locked();

    audio_log!(
        "[audio] ===== start_audio_capture (per-app 优先, monitor fallback; 窗口 pid={}) =====",
        pid
    );

    // eventfd：读端进 pw_loop（WakeFd），两个写端 dup——一个给外部命令通道
    // （RunningSession.wake_tx），一个给 owner 线程自唤醒（done 事件）。
    let wake = make_wake_eventfd()?;
    let wake_for_sender = dup_fd(&wake)?;
    let wake_self_keep = dup_fd(&wake)?;
    let wake_self_fd = wake_self_keep.as_raw_fd();
    let wake_for_loop = WakeFd(wake);

    let shared = Arc::new(Mutex::new(AudioCaptureState::new(pid)));
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (boot_tx, boot_rx) = mpsc::channel::<Result<(), String>>();

    let thread_shared = shared.clone();
    let join = std::thread::Builder::new()
        .name(SESSION_THREAD_NAME.to_string())
        .spawn(move || {
            session_main(
                cmd_rx,
                wake_for_loop,
                wake_self_fd,
                wake_self_keep,
                thread_shared,
                boot_tx,
            );
        })
        .map_err(|e| format!("spawn: {}", e))?;

    let session = Arc::new(RunningSession {
        shared,
        cmd_tx: cmd_tx.clone(),
        wake_tx: wake_for_sender,
        join: Mutex::new(Some(join)),
    });

    // 1) 等 owner 线程完成 pw setup 并进入 run。
    let boot = boot_rx.recv_timeout(Duration::from_millis(BOOT_BUDGET_MS));
    if !matches!(boot, Ok(Ok(()))) {
        let err = match boot {
            Ok(Err(e)) => e,
            Ok(Ok(())) => unreachable!(),
            Err(e) => format!("session boot wait: {}", e),
        };
        let msg = format!("[audio] 会话启动失败: {}", err);
        audio_log!("{}", msg);
        crate::status::note_error("error", &msg);
        join_owner(&session); // 线程可能已退出（setup 失败路径自行收尾）
        return Err(err);
    }

    // 2) 投 Start 命令 + 唤醒（Java 线程到此为止只做这两件事）。
    let (ack_tx, ack_rx) = mpsc::channel();
    if cmd_tx.send(Command::Start(pid, ack_tx)).is_err() {
        join_owner(&session);
        return Err("session exited before start".to_string());
    }
    session.wake();

    // 3) 等循环线程的连接回执。
    let result = ack_rx.recv_timeout(Duration::from_millis(START_ACK_BUDGET_MS));
    match result {
        Ok(Ok(())) => {
            let mut guard = AUDIO_CAPTURE.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(session.clone());
            Ok(())
        }
        Ok(Err(e)) => {
            // 连接失败：回收线程（本会话没进全局，直接 Quit+join）
            join_owner(&session);
            Err(e)
        }
        Err(e) => {
            let err = format!("start ack: {}", e);
            audio_log!("[audio] start 超时: {}", err);
            join_owner(&session);
            Err(err)
        }
    }
}

/// 停止音频捕获（对外）：lifecycle 锁内走 stop_locked。
pub fn stop_audio_capture() {
    let _lifecycle = SESSION_LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    stop_locked();
}

/// stop 主体（**必须持 SESSION_LIFECYCLE 调用**）：
/// 摘全局 → active=false/clear pcm → Quit 命令 → 唤醒 → join owner 线程。
fn stop_locked() {
    let session = AUDIO_CAPTURE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let Some(session) = session else {
        return;
    };
    {
        let mut s = session.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.active = false;
        s.pcm.clear();
        s.pcm_overflow_logged = false;
    }
    join_owner(&session);
    audio_log!("[audio] capture stopped");
}

/// 发 Quit、唤醒、join owner 线程回收。join 返回 = 该线程已释放全部
/// pw 对象（新模型把旧实现"sleep(150) 赌线程退出"换成了确定性的线程汇合）。
fn join_owner(session: &Arc<RunningSession>) {
    let _ = session.cmd_tx.send(Command::Quit);
    session.wake();
    let joiner = session
        .join
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(h) = joiner {
        if !join_timeout(h, Duration::from_millis(STOP_JOIN_BUDGET_MS)) {
            audio_log!("[audio] WARN: owner 线程 join 超时（pw daemon stall？）；线程仍自行收尾，不存在跨线程 pw 操作");
        }
    }
}

/// join with budget；true=线程已结束。超时时保留线程（它只在自己线程里
/// 释放自己的 pw 对象，外部已无人引用），但把 JoinHandle 遗忘避免二次 join。
fn join_timeout(h: std::thread::JoinHandle<()>, budget: Duration) -> bool {
    let start = Instant::now();
    loop {
        if h.is_finished() {
            let _ = h.join();
            return true;
        }
        if start.elapsed() >= budget {
            std::mem::forget(h);
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 取走累积的 PCM（返回 [sampleRate(4), channels(4), pcm...]）。
/// 与 process 回调的互斥只在 memcpy 级别——owner 线程回调等锁最长
/// 一个缓冲拷贝的时间，poll 调用方（Java 渲染线程）同理。
pub fn poll_audio_capture() -> Option<Vec<u8>> {
    let guard = AUDIO_CAPTURE.lock().ok()?;
    let session = guard.as_ref()?;
    let mut s = session.shared.lock().ok()?;
    if s.pcm.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(s.pcm.len() + 8);
    out.extend_from_slice(&(s.sample_rate as u32).to_le_bytes());
    out.extend_from_slice(&(s.channels as u32).to_le_bytes());
    // append：一次锁内整段转移（等价原子交换；s.pcm 变空、保留分配）
    out.append(&mut s.pcm);
    // 缓冲已取空 → 下一次溢出事件重新播报一行（与 pcm_overflow_logged 的一次性语义配套）
    s.pcm_overflow_logged = false;
    Some(out)
}

/// 返回当前音频捕获链路状态（JSON 字符串），供 Java /wl audio status 查询。
/// 覆盖全链路：是否有会话 → PID/mode → 目标节点 → 是否已 link → 回调/字节统计。
pub fn get_audio_capture_status() -> String {
    let guard = match AUDIO_CAPTURE.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let Some(session) = guard.as_ref() else {
        return r#"{"active":false,"stage":"idle","mode":"none","note":"no capture session"}"#
            .to_string();
    };
    let s = match session.shared.lock() {
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
            r#""capture_events":{},"total_bytes":{},"pcm_overflow_drops":{},"#,
            r#""reconnects":{},"#,
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
        s.pcm_overflow_drops,
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
    let Some(session) = guard.as_ref() else {
        return CaptureBrief::Idle("no capture session; see waylandcraft-audio.log".to_string());
    };
    let Ok(s) = session.shared.try_lock() else {
        return CaptureBrief::Idle("status busy".to_string());
    };
    if !s.active {
        return CaptureBrief::Idle(format!(
            "stopped (mode was {}, bytes={})",
            s.mode.as_str(),
            s.total_bytes
        ));
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

// ── watchdog（owner 线程循环 timer，取代独立 watchdog 线程）──────────

/// 每轮需要的会话快照。
struct WatchdogSnapshot {
    mode: CaptureMode,
    pid: u32,
    target_node: u32,
    stalled: bool,
    promote_due: bool,
}

/// 循环 timer 回调（500ms）：start 同步预算兜底 + 断流自愈轮次。
/// 判据与旧 watchdog_loop 完全一致；区别有二：
///   1. 拓扑来自常驻 registry 监听（实时 global/global_remove），
///      不再也不能另起第二条 pw 连接做"每轮枚举"；
///   2. 所有重连动作在本线程执行（见 connect_capture 注释）。
fn watchdog_tick(sess_rc: &Rc<RefCell<Session>>, topo_rc: &Rc<RefCell<TopoState>>) {
    // 1) pending_start：迟到的 core.done → 立刻开始；同步预算超时 → 尽力开始
    let (begin_now, timed_out, quit) = {
        let sess = sess_rc.borrow_mut();
        let (begin_now, timed_out) = match &sess.pending_start {
            Some((_, _, since)) => {
                if sess.synced_flag.get() {
                    (true, false)
                } else {
                    (
                        since.elapsed() >= Duration::from_millis(START_SYNC_BUDGET_MS),
                        true,
                    )
                }
            }
            None => (false, false),
        };
        (begin_now, timed_out, sess.quit)
    };
    if begin_now && !quit {
        if timed_out {
            audio_log!("[audio] registry 同步等待超时，用现有拓扑尽力开始");
        }
        try_begin_start(sess_rc, topo_rc);
    }

    // 2) 断流自愈轮次（锁的粒度：快照 + 计时推进；pw 操作全部在锁外/本线程）
    let snap = {
        let (shared, quit) = {
            let sess = sess_rc.borrow();
            (sess.shared.clone(), sess.quit)
        };
        if quit {
            return;
        }
        let Ok(mut shared) = shared.lock() else { return };
        if !shared.active {
            return;
        }
        let now = Instant::now();
        let stalled = now.duration_since(shared.last_data_at) >= Duration::from_millis(STALL_MS);
        let promote_due = shared.mode == CaptureMode::MonitorFallback
            && !shared.promotion_disabled
            && shared.pid > 0
            && now.duration_since(shared.last_promotion_at) >= Duration::from_millis(PROMOTION_MS);

        // 轮次触发条件（与旧 watchdog_loop 一致）：
        // - per-app：定期重验"目标节点是否还在 / id 是否变了"。不能只靠
        //   "0 字节"判断流——PipeWire 1.4 实测：应用节点消失后 daemon 会把
        //   孤儿 capture stream 重新路由（连默认 sink monitor），process 回调
        //   继续全速投递静音数据，字节流正常但内容已不是该应用。
        //   因此无论有无数据都按周期核对；持续无数据（应用真静音）时指数
        //   退避降低轮询频率。
        // - monitor-fallback：仅"断流（sink 变了）"或"到点尝试升级"才跑。
        let due = match shared.mode {
            CaptureMode::PerApp => now >= shared.next_check_at,
            CaptureMode::MonitorFallback => now >= shared.next_check_at && (stalled || promote_due),
        };
        if !due {
            return;
        }

        if stalled {
            shared.check_backoff =
                (shared.check_backoff * 2).min(Duration::from_millis(MAX_BACKOFF_MS));
            shared.next_check_at = now + shared.check_backoff;
        } else {
            shared.check_backoff = Duration::from_millis(CHECK_THROTTLE_MS);
            shared.next_check_at = now
                + Duration::from_millis(if shared.mode == CaptureMode::PerApp {
                    REVALIDATE_MS
                } else {
                    CHECK_THROTTLE_MS
                });
        }
        if promote_due {
            shared.last_promotion_at = now;
        }

        // 分级=细节（poll 周期行，500ms tick 一条）→ verbose 档；同函数
        // 的状态迁移行（重连/降级/节点消失/升级）全部保留。
        audio_log_v!(
            "[audio] watchdog: round mode={} stalled={} promote_due={} events={} bytes={}",
            shared.mode.as_str(),
            stalled,
            promote_due,
            shared.capture_events,
            shared.total_bytes
        );
        WatchdogSnapshot {
            mode: shared.mode,
            pid: shared.pid,
            target_node: shared.target_node,
            stalled,
            promote_due,
        }
    };

    let topo = topo_rc.borrow().topo.clone();
    match snap.mode {
        CaptureMode::PerApp => watchdog_perapp(sess_rc, topo_rc, &topo, snap),
        CaptureMode::MonitorFallback => watchdog_monitor(sess_rc, topo_rc, &topo, snap),
    }
}

/// per-app 重验（每轮都做）：应用重开流（节点 id 变了）→ 重连最新节点；
/// 节点消失 → 计入失败，连续达阈值降级 monitor；节点还在且 id 一致 →
/// 链路有效（无数据只是静音）。
fn watchdog_perapp(
    sess_rc: &Rc<RefCell<Session>>,
    topo_rc: &Rc<RefCell<TopoState>>,
    topo: &PwTopology,
    snap: WatchdogSnapshot,
) {
    match find_app_stream_node(topo, snap.pid) {
        Some((node_id, node_name, _)) if node_id != snap.target_node => {
            audio_log!(
                "[audio] watchdog: per-app 节点更新 {} → {}（应用重开流），重连",
                snap.target_node,
                node_id
            );
            match connect_capture(sess_rc, CaptureMode::PerApp, node_id, &node_name, None) {
                Ok(()) => {
                    with_shared(sess_rc, |s| {
                        s.reconnect_failures = 0;
                        s.total_reconnects += 1;
                    });
                    audio_log!("[audio] watchdog: per-app 重连成功 node={}", node_id);
                }
                Err(e) => note_reconnect_failure(sess_rc, topo_rc, format!("per-app reconnect: {}", e)),
            }
        }
        Some((_, _, _)) => {
            // 节点还在、id 没变 → 直连链路指向的就是它。持续无数据只可能是
            // 应用静音（不是断链：daemon 若已把我们挪走，节点消失轮会先命中）。
            // 目标有效即清零失败计数；静音只播报一次。
            let silence_note = with_shared(sess_rc, |s| {
                s.reconnect_failures = 0;
                if snap.stalled && !s.silence_logged {
                    s.silence_logged = true;
                    true
                } else {
                    false
                }
            });
            if silence_note {
                audio_log!(
                    "[audio] watchdog: per-app 节点 {} 存活但无数据（应用静音？）— 保持链路",
                    snap.target_node
                );
            }
        }
        None => {
            // 应用节点消失（进程退了/流关了）→ 计入失败，达阈值降级
            audio_log!("[audio] watchdog: pid {} 播放流节点已消失", snap.pid);
            note_reconnect_failure(sess_rc, topo_rc, "app stream node gone".to_string());
        }
    }
}

/// monitor-fallback：定时探测该 pid 是否已出现应用流（应用开始出声）→ 升级
/// 回 per-app；否则 monitor 也断流时重连 sink（sink 一般不消失，少见）。
fn watchdog_monitor(
    sess_rc: &Rc<RefCell<Session>>,
    topo_rc: &Rc<RefCell<TopoState>>,
    topo: &PwTopology,
    snap: WatchdogSnapshot,
) {
    if snap.promote_due {
        if let Some((node_id, node_name, _)) = find_app_stream_node(topo, snap.pid) {
            audio_log!(
                "[audio] watchdog: 应用出现播放流 node={}，从 monitor 升级到 per-app",
                node_id
            );
            match connect_capture(sess_rc, CaptureMode::PerApp, node_id, &node_name, None) {
                Ok(()) => {
                    with_shared(sess_rc, |s| {
                        s.total_reconnects += 1;
                        s.reconnect_failures = 0;
                        s.fallback_reason = None;
                    });
                }
                Err(e) => {
                    with_shared(sess_rc, |s| {
                        s.last_error = Some(format!("promotion: {}", e));
                    });
                }
            }
            return;
        }
    }

    if snap.stalled {
        match find_default_sink(topo) {
            Ok((sink_node, sink_name)) if sink_node != snap.target_node => {
                audio_log!(
                    "[audio] watchdog: monitor sink 变化 {} → {}，重连",
                    snap.target_node,
                    sink_node
                );
                let reason = with_shared(sess_rc, |s| s.fallback_reason.clone());
                if let Err(e) = connect_capture(
                    sess_rc,
                    CaptureMode::MonitorFallback,
                    sink_node,
                    &sink_name,
                    reason,
                ) {
                    note_reconnect_failure(sess_rc, topo_rc, format!("monitor reconnect: {}", e));
                }
            }
            Ok(_) => {
                let note = with_shared(sess_rc, |s| {
                    if !s.silence_logged {
                        s.silence_logged = true;
                        true
                    } else {
                        false
                    }
                });
                if note {
                    audio_log!("[audio] watchdog: monitor sink 存活但无数据（整机静音）");
                }
            }
            Err(e) => note_reconnect_failure(sess_rc, topo_rc, format!("monitor enum sink: {}", e)),
        }
    }
}

/// per-app 重连失败计数；达 MAX_RECONNECT_FAILURES 降级 monitor-fallback。
fn note_reconnect_failure(
    sess_rc: &Rc<RefCell<Session>>,
    topo_rc: &Rc<RefCell<TopoState>>,
    err: String,
) {
    let (failures, should_fallback, topo_pid) = {
        let (shared, quit) = {
            let sess = sess_rc.borrow();
            (sess.shared.clone(), sess.quit)
        };
        if quit {
            return;
        }
        let Ok(mut s) = shared.lock() else { return };
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
    // 降级：连当前 sink（常驻 registry 实时拓扑 = 旧实现的"重新枚举"，
    // 且 global_remove 保证失败目标不会残留）。
    let topo = topo_rc.borrow().topo.clone();
    let reason = format!("per-app 连续失败 {} 次（最后: {}），降级整机", failures, err);
    match connect_monitor_fallback(sess_rc, &topo, reason) {
        Ok(()) => {
            with_shared(sess_rc, |s| {
                // 降级后保留 promotion 探测——应用一旦真稳定出声就升级回
                // per-app（与旧实现一致：promotion_disabled=false）。
                s.promotion_disabled = false;
                s.reconnect_failures = 0;
            });
            audio_log!("[audio] watchdog: 已降级 monitor-fallback (pid={})", topo_pid);
        }
        Err(e) => {
            with_shared(sess_rc, |s| {
                s.last_error = Some(format!("fallback failed: {}", e));
                // 连 sink 都没有 → 放弃后续升级，避免死循环
                s.promotion_disabled = true;
            });
            audio_log!("[audio] watchdog: 降级也失败: {} — 暂停自动升级", e);
        }
    }
}

/// watchdog_tick/watchdog_* 共用的短临界区状态修改（避免到处手写
/// borrow→clone→lock 三步；**锁在闭包返回时立即释放**，闭包内禁止
/// 任何 pw 调用——由调用点纪律保证）。
fn with_shared<T>(sess_rc: &Rc<RefCell<Session>>, f: impl FnOnce(&mut AudioCaptureState) -> T) -> T {
    let shared = sess_rc.borrow().shared.clone();
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s)
}

// ── 测试/诊断用的独立枚举（不进会话线程模型） ─────────────────────

#[cfg(test)]
/// pw_main_loop 指针的 Send 包装：libpipewire 保证 pw_main_loop_quit 可从
/// 其它线程调用（标准用法）；Rust 绑定因 NonNull 保守标记 !Send。
/// **仅** 用于 enumerate_topology（quit 之后调用方 join 回执再 drop——
/// 消除旧实现 quit-after-destroy 窗口）。
struct MainLoopPtr(*mut pipewire::sys::pw_main_loop);
#[cfg(test)]
unsafe impl Send for MainLoopPtr {}

/// 定时线程体：sleep → quit（libpipewire 允许跨线程）→ 回执。
/// 以 fn 参数整体接收 MainLoopPtr——避免 edition 2024 闭包按字段捕获裸指针
/// （字段捕获会把 !Send 的 `*mut pw_main_loop` 漏进闭包类型）。
#[cfg(test)]
fn mainloop_quit_after(ml: MainLoopPtr, delay: u64, ack: Sender<()>) {
    std::thread::sleep(Duration::from_millis(delay));
    unsafe { pipewire::sys::pw_main_loop_quit(ml.0) };
    let _ = ack.send(());
}

/// 通过 registry 枚举一次 PipeWire 拓扑（**仅供测试/诊断**）。
/// mainloop 在调用线程上 run/drop（单一所有者）；辅助线程只 quit 并回执，
/// 调用方收到回执才返回——任何线程都不会在 loop 销毁后触碰它。
#[cfg(test)]
fn enumerate_topology(collect_ms: u64) -> Result<PwTopology, String> {
    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None).map_err(|e| format!("mainloop: {}", e))?;
    let context = pw::context::Context::new(&mainloop).map_err(|e| format!("context: {}", e))?;
    let core = context.connect(None).map_err(|e| format!("connect: {}", e))?;

    let topo_state = Rc::new(RefCell::new(TopoState::default()));

    let registry = Rc::new(core.get_registry().map_err(|e| format!("registry: {}", e))?);
    let registry_weak = Rc::downgrade(&registry);

    let topo_ref = topo_state.clone();
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
                let mut t = topo_ref.borrow_mut();
                t.topo.nodes.insert(node_id, (media_class.clone(), pid_from_global, name, proc_name));
                if pid_from_global != 0 || !node_worth_binding(&media_class) {
                    return;
                }
                if t.bound.contains_key(&node_id) {
                    return;
                }
            }
            let Some(reg) = registry_weak.upgrade() else { return };
            if let Ok(node) = reg.bind::<pw::node::Node, _>(global) {
                let st = topo_ref.clone();
                let nl = node
                    .add_listener_local()
                    .info(move |info| {
                        if let Some(props) = info.props() {
                            if let Some(v) = props.get("application.process.id") {
                                if let Ok(pid) = v.parse::<u32>() {
                                    if let Some(entry) = st.borrow_mut().topo.nodes.get_mut(&node_id)
                                    {
                                        entry.1 = pid;
                                    }
                                }
                            }
                        }
                    })
                    .register();
                topo_ref.borrow_mut().bound.insert(node_id, (Box::new(node), Box::new(nl)));
            }
        })
        .register();

    let (quit_done_tx, quit_done_rx) = mpsc::channel();
    let ml_ptr = MainLoopPtr(mainloop.as_raw_ptr());
    let quiter = std::thread::Builder::new()
        .name("wc-audio-enum-quit".to_string())
        .spawn(move || mainloop_quit_after(ml_ptr, collect_ms, quit_done_tx))
        .map_err(|e| format!("spawn: {}", e))?;

    mainloop.run();

    // 等 quit 调用真正返回（消除 quit-after-destroy 窗口）；join 是权威汇合。
    let _ = quit_done_rx.recv_timeout(Duration::from_secs(5));
    let _ = quiter.join();

    // 以下全部在调用线程：run 已返回 + quiter 已退出，无并发 pw 引用。
    // 依赖反向释放：listener → proxies → registry → core → context → mainloop。
    let topo = topo_state.borrow().topo.clone();
    drop(_listener);
    topo_state.borrow_mut().bound.clear();
    drop(registry);
    drop(core);
    drop(context);
    drop(mainloop);
    Ok(topo)
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

    // 分级=细节（sink 枚举 dump；本函数在 watchdog 降级/重连路径会被
    // 周期性调用）→ verbose 档；空集 Err（失败证据）保留。
    for (id, name) in &sinks {
        audio_log_v!("[audio] candidate sink: id={} name={}", id, name);
    }

    if sinks.is_empty() {
        return Err("no Audio/Sink node found — PipeWire 可能没在跑，或没有音频输出设备".to_string());
    }

    // 选第一个（通常即默认输出）；如需精确默认 sink 可后续读 metadata default.audio.sink
    let (id, name) = sinks.remove(0);
    Ok((id, name))
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

    /// 回归护栏：共享状态不得再容纳 pw 对象（崩溃根因的编译期检查——
    /// AudioCaptureState 被多线程访问，任何非 Send/Sync 字段都会让本断言
    /// 编译失败；历史上 `_stream/_mainloop/_listener` + unsafe impl Send
    /// 正是崩溃载体）。
    #[test]
    fn shared_state_is_send_sync_by_construction() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AudioCaptureState>();
    }

    // ── PCM 环形上限（poll 停摆防 OOM）───────────────────────────────

    #[test]
    fn pcm_ring_no_drop_below_cap() {
        let cap = 1000;
        assert_eq!(pcm_ring_drop_for(0, 400, cap), Some(0));
        // 恰好填满：不丢
        assert_eq!(pcm_ring_drop_for(600, 400, cap), Some(0));
    }

    #[test]
    fn pcm_ring_overflow_drops_oldest_keeps_newest() {
        let cap = 1000;
        // 800 + 400 = 1200 > cap → 丢最旧 200（append 后 len == cap）
        assert_eq!(pcm_ring_drop_for(800, 400, cap), Some(200));
        // 丢弃量永远不会吃掉"保住的新数据"：drop = buffered + incoming - cap < buffered
        assert_eq!(pcm_ring_drop_for(99, 98, 100), Some(97));
    }

    #[test]
    fn pcm_ring_oversized_chunk_dropped_whole() {
        // 单块 >= cap（正常 quantum 不可能的防御分支）：整块丢弃
        assert_eq!(pcm_ring_drop_for(10, 1000, 1000), None);
        assert_eq!(pcm_ring_drop_for(10, 2000, 1000), None);
    }

    #[test]
    fn push_pcm_caps_buffer_counts_drops_and_keeps_newest() {
        let mut s = AudioCaptureState::new(0);
        let half = PCM_BUFFER_CAP_BYTES / 2;
        let a = vec![0xAAu8; half];
        let b = vec![0xBBu8; half];
        let c = vec![0xCCu8; half];

        assert_eq!(s.push_pcm(&a), 0);
        assert_eq!(s.pcm.len(), half);
        assert_eq!(s.push_pcm(&b), 0);
        assert_eq!(s.pcm.len(), PCM_BUFFER_CAP_BYTES);
        assert_eq!(s.pcm_overflow_drops, 0);

        // 第三块触发溢出：丢最旧 half（整块 a），保留 b + c，缓冲钉在 cap
        assert_eq!(s.push_pcm(&c), half as u64);
        assert_eq!(s.pcm.len(), PCM_BUFFER_CAP_BYTES);
        assert_eq!(&s.pcm[..half], &b[..], "应保留中间块 b（丢最旧保最新）");
        assert_eq!(&s.pcm[half..], &c[..], "最新块 c 完整保留在尾部");
        assert_eq!(s.pcm_overflow_drops, half as u64);
    }

    #[test]
    fn push_pcm_steady_overflow_keeps_buffer_at_cap() {
        // 模拟 poll 长期停摆：连续小块写入 4x cap 的量，
        // 缓冲长度恒 <= cap，丢弃计数 = 超出部分，且后续仍持续接收新数据。
        let mut s = AudioCaptureState::new(0);
        let chunk = vec![0x7Fu8; 4096]; // 正常 PipeWire quantum 量级
        let chunks = (PCM_BUFFER_CAP_BYTES / 4096) * 4;
        let mut total_dropped = 0u64;
        for _ in 0..chunks {
            total_dropped += s.push_pcm(&chunk);
        }
        assert!(s.pcm.len() <= PCM_BUFFER_CAP_BYTES, "缓冲必须钉在 cap 内");
        assert_eq!(s.pcm.len(), PCM_BUFFER_CAP_BYTES);
        assert_eq!(
            s.pcm_overflow_drops,
            (chunks as usize * 4096 - PCM_BUFFER_CAP_BYTES) as u64
        );
        assert_eq!(total_dropped, s.pcm_overflow_drops);
        // 丢弃后仍能继续接收（不进入任何"拒收"死态）
        assert_eq!(s.push_pcm(&chunk), 4096);
    }

    #[test]
    fn push_pcm_oversized_chunk_counts_but_appends_nothing() {
        let mut s = AudioCaptureState::new(0);
        s.pcm.extend_from_slice(&[1u8; 100]);
        let huge = vec![0xDDu8; PCM_BUFFER_CAP_BYTES + 16];
        assert_eq!(s.push_pcm(&huge), huge.len() as u64);
        assert_eq!(s.pcm.len(), 100, "超大块不追加，旧数据原样保留");
        assert_eq!(s.pcm_overflow_drops, huge.len() as u64);
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
            let v = (12000.0
                * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / RATE as f64).sin())
                as i16;
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
        assert!(
            status1.contains(r#""mode":"per-app""#),
            "捕获中 mode 应稳定为 per-app: {status1}"
        );

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

    /// 事故回归压力测试（本机 PipeWire；64 主循环 + 16 并发 churn，
    /// 全程约 3-6 分钟）：复现用户实测崩溃场景——
    ///   多窗口共享期间：Java 侧（= 测试线程）快速 start/stop 切换窗口，
    ///   同时 owner 线程 watchdog 正在重验/重连/降级/升级，
    ///   同时目标应用节点（paplay）在会话中途消失/换流。
    /// 旧线程模型在此场景下"运行一段时间后 SIGSEGV@libspa-support"
    /// （hs_err_pid8053）；新模型要求全部跑完、进程存活、状态机始终合法。
    ///
    /// 跑法：
    ///   cargo test -- --ignored --test-threads=1 audio_threading_stress_live
    #[test]
    #[ignore = "事故回归压力测试：需要 PipeWire + paplay；全程约 3-6 分钟"]
    fn audio_threading_stress_live() {
        if std::process::Command::new("paplay").arg("--version").output().is_err() {
            eprintln!("SKIP: paplay 不存在");
            return;
        }
        let wav = std::env::temp_dir().join("waylandcraft-audio-stress-tone.wav");
        write_test_wav(&wav).expect("write wav");

        const CYCLES: usize = 64;
        let mut started_ok = 0usize;
        let mut started_err = 0usize;

        // ── 并发 churn 线程：模拟另一窗口开关，专门打旧实现
        //    "stop 线程 drop stream vs watchdog 重连" 的交错窗口。
        let churn_wav = wav.clone();
        let churn = std::thread::Builder::new()
            .name("stress-churn".to_string())
            .spawn(move || {
                let mut n = 0usize;
                while n < 16 {
                    // 每轮带一个真实 paplay，让 churn 也会命中 per-app 路径
                    let mut child = std::process::Command::new("paplay")
                        .arg("-r")
                        .arg(&churn_wav)
                        .stdin(std::process::Stdio::null())
                        .spawn()
                        .expect("spawn paplay (churn)");
                    let pid = child.id();
                    std::thread::sleep(Duration::from_millis(400));
                    if start_audio_capture(pid).is_ok() {
                        let _ = poll_audio_capture();
                        std::thread::sleep(Duration::from_millis(40));
                        n += 1;
                    }
                    stop_audio_capture();
                    let _ = child.kill();
                    let _ = child.wait();
                    std::thread::sleep(Duration::from_millis(200));
                }
                n
            })
            .expect("spawn churn");

        for i in 0..CYCLES {
            // 每轮起一条真实播放流
            let mut child = std::process::Command::new("paplay")
                .arg("-r")
                .arg(&wav)
                .stdin(std::process::Stdio::null())
                .spawn()
                .expect("spawn paplay");
            let pid = child.id();

            // 等节点出现（最多 2s；允许"节点没出现"的竞态轮）
            let node_deadline = std::time::Instant::now() + Duration::from_millis(2000);
            while std::time::Instant::now() < node_deadline {
                let topo = enumerate_topology(250).expect("enum");
                if find_app_stream_node(&topo, pid).is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(150));
            }

            match start_audio_capture(pid) {
                Ok(()) => started_ok += 1,
                Err(e) => {
                    started_err += 1;
                    eprintln!("[stress] cycle {i}: start Err（允许：daemon 忙/节点竞态）: {e}");
                }
            }

            for _ in 0..3 {
                let _ = poll_audio_capture();
                std::thread::sleep(Duration::from_millis(80));
            }

            match i % 4 {
                0 => {
                    // 杀 app → watchdog 重验轮启动（失败计数/可能降级）→ 中途 stop
                    let _ = child.kill();
                    let _ = child.wait();
                    std::thread::sleep(Duration::from_millis(1400));
                    stop_audio_capture();
                }
                1 => {
                    // 干净停止 → 再杀 app
                    stop_audio_capture();
                    let _ = child.kill();
                    let _ = child.wait();
                    std::thread::sleep(Duration::from_millis(60));
                }
                2 => {
                    // 不 stop 直接换新会话（start 内部先 stop）——模拟切窗口
                    let mut child2 = std::process::Command::new("paplay")
                        .arg("-r")
                        .arg(&wav)
                        .stdin(std::process::Stdio::null())
                        .spawn()
                        .expect("spawn paplay 2");
                    let pid2 = child2.id();
                    std::thread::sleep(Duration::from_millis(600));
                    let _ = start_audio_capture(pid2);
                    let st = get_audio_capture_status();
                    assert!(st.contains("stage"), "status JSON 异常: {st}");
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = child2.kill();
                    let _ = child2.wait();
                    stop_audio_capture();
                }
                _ => {
                    // 会话存活期间让 watchdog 完整跑一轮重连（节点消失→
                    // 出现新节点 → per-app 重连与并发 stop 交错）：
                    // kill 后在同 pid 语义不可能；用"旧流停止→应用流消失→
                    // watchdog 失败计数→降级"路径，再在降级轮之后立即 stop。
                    std::thread::sleep(Duration::from_millis(2600));
                    let _ = child.kill();
                    let _ = child.wait();
                    std::thread::sleep(Duration::from_millis(400));
                    stop_audio_capture();
                }
            }

            if i % 16 == 15 {
                let st = get_audio_capture_status();
                assert!(st.contains("stage"), "status JSON 异常: {st}");
                eprintln!("[stress] cycle {i} done: ok={started_ok} err={started_err} {st}");
            }
        }

        let churn_done = churn.join().expect("churn 线程 panic（=捕获层线程模型缺陷）");
        assert_eq!(churn_done, 16, "churn 完成数异常");

        // 收尾：确保无残留会话 + 状态机回到 idle
        stop_audio_capture();
        let final_st = get_audio_capture_status();
        assert!(final_st.contains(r#""stage":"idle""#), "stop 后应为 idle: {final_st}");
        eprintln!(
            "[stress] ALL {} cycles + {} churn done, ok={} err={}",
            CYCLES, churn_done, started_ok, started_err
        );
        let _ = std::fs::remove_file(&wav);
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
