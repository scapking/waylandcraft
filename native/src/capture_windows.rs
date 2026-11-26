// Windows 平台窗口/桌面捕获后端（Windows.Graphics.Capture，Win10 1903+）。
//
// 消费形态对齐 Linux 的 portal 路径（portal_capture.rs → bridge.rs portal_capture_*
// → PipeWireCaptureManager → RemoteWindowRenderer → SharedWindowDisplay）：
//   start → 独立捕获线程（"wc-win-capture"）收帧 → 写共享帧槽 →
//   Java 渲染线程经 win_capture_frame 轮询取帧 → 游戏内纹理显示。
// 差异（有意为之，都是 portal 的已知缺陷）：
//   1. 帧槽带 seq 去重：无新帧时 take 返回 None → JNI 回 null → Java 保持现纹理
//      （portal get_frame() 每次盲克隆整帧，无论有无新帧）。
//   2. stop 确定性关闭：session.Close → framePool.Close → join 捕获线程（带超时
//      预算，模式同 audio_capture::join_timeout）→ 帧缓冲清空。portal 的
//      stop 是 no-op（"PipeWire mainloop runs in a thread, will exit when process
//      exits"——僵尸线程直到进程退出），本模块以此为硬验收反例。
//   3. 像素契约 ARGB8888（小端 u32/像素，内存字节序 B,G,R,A）——与 D3D11
//      BGRA8 读回结果完全一致，零转换成本；Java 侧按 int 像素解码
//      （同 ime_popup_frame / RemoteWindowRenderer 的 ARGB 惯例）。
//
// 模块布局：
//   - portable 核心（本文件，无 cfg）：帧槽状态机、目标过滤谓词、JSON 装配、
//     wire 编码、stop 幂等机 —— Linux `cargo test --lib` 直接覆盖。
//   - #[cfg(windows)] platform：EnumWindows/EnumDisplayMonitors 枚举、
//     IGraphicsCaptureItemInterop 建项、Direct3D11CaptureFramePool(FreeThreaded,
//     BGRA8) + GraphicsCaptureSession、"wc-win-capture" 线程与 D3D11 staging 读回。
//
// JNI 面（bridge.rs win_capture_* ↔ Java WaylandCraftBridge.winCapture*）：
//   winCaptureTargets()       -> String      目标列表 JSON（windows[] + monitors[]）
//   winCaptureStartWindow(h)  -> byte[]      "ok:<session_id>"；失败抛异常
//   winCaptureStartMonitor(i) -> byte[]      "ok:<session_id>"（monitor 用负 id）
//   winCaptureFrame()         -> byte[]|[w i32][h i32][seq u32][argb32le...]；无新帧 null
//   winCaptureStop()          -> String      关停报告（joined / thread-timeout-detached / no-session）

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// ═══════════════════════════════════════════════════════
// portable 核心
// ═══════════════════════════════════════════════════════

/// 捕获源。session_id 编码进 start 的 "ok:<id>" 回执（对齐 portal "ok:<node_id>"
/// 的十进制可 Long.parseLong 约定）：
///   Window: hwnd 十进制（x64 HWND < 2^32 恒成立，正数）
///   Monitor: -1 - index（负数域，与 PipeWire node id / HWND 正数天然互斥，
///            单会话模型下 Java 可据此分辨来源类型）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    Window(isize),
    Monitor(i32),
}

impl CaptureKind {
    pub fn session_id(&self) -> i64 {
        match self {
            CaptureKind::Window(h) => *h as i64,
            CaptureKind::Monitor(i) => -1 - *i as i64,
        }
    }
}

/// 共享帧槽里的一帧。pixels 为 ARGB8888 小端字节流（w*h*4）。
/// Arc<[u8]>：take() 只做引用计数 +1，渲染线程轮询廉价
/// （portal 的 Vec<u8> 全帧克隆是每帧 8MB 级 memcpy）。
#[derive(Clone)]
pub struct FrameData {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

/// 帧槽状态机：capture 线程 publish（单生产者），JNI 轮询 take（单消费者，
/// Java 渲染线程）。seq 单调递增、clear 不回退——消费者据此去重。
pub struct FrameSlot {
    seq: AtomicU64,
    consumed: AtomicU64,
    frame: Mutex<Option<FrameData>>,
}

impl Default for FrameSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameSlot {
    pub fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            frame: Mutex::new(None),
        }
    }

    /// 发布一帧（仅捕获线程调用；单生产者假设写死在文档，不依赖 CAS）。
    /// 返回新帧号。
    pub fn publish(&self, width: u32, height: u32, pixels: Vec<u8>) -> u64 {
        let mut slot = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        let seq = self.seq.load(Ordering::Acquire) + 1;
        self.seq.store(seq, Ordering::Release);
        *slot = Some(FrameData {
            seq,
            width,
            height,
            pixels: Arc::from(pixels),
        });
        seq
    }

    /// 取最新帧；seq 已消费过（无新帧）→ None——JNI 层映射为 null，
    /// Java 保持现有纹理不重传。
    pub fn take(&self) -> Option<FrameData> {
        // 无锁快路径：绝大多数轮询周期无新帧。
        if self.consumed.load(Ordering::Acquire) >= self.seq.load(Ordering::Acquire) {
            return None;
        }
        let slot = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        let frame = slot.as_ref()?;
        self.consumed.store(frame.seq, Ordering::Release);
        Some(frame.clone())
    }

    pub fn has_new(&self) -> bool {
        self.consumed.load(Ordering::Acquire) < self.seq.load(Ordering::Acquire)
    }

    /// stop/换会话时清空像素缓冲（释放最后一帧的 w*h*4）。
    /// seq 保留：旧消费者的去重水位不重置，避免"clear 后又拿到幽灵帧"。
    pub fn clear(&self) {
        *self.frame.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// wire 编码：`[width i32][height i32][seq u32][argb32le pixels]`。
/// seq 截到 u32：30fps 下约 4.5 年绕回，绕回后相邻两帧号仍不同（去重按
/// "与上一帧不等"判断，与 ime_popup_frame 的合同一致）。
pub const WIRE_HEADER_LEN: usize = 12;

pub fn encode_frame_wire(frame: &FrameData) -> Vec<u8> {
    let mut out = Vec::with_capacity(WIRE_HEADER_LEN + frame.pixels.len());
    out.extend_from_slice(&frame.width.to_le_bytes());
    out.extend_from_slice(&frame.height.to_le_bytes());
    out.extend_from_slice(&(frame.seq as u32).to_le_bytes());
    out.extend_from_slice(&frame.pixels);
    out
}

/// EnumWindows 回调收集的原始事实（全部来自 Win32 查询），过滤裁决是下面的
/// 纯函数——决策表 Linux 可测，不需要 Win32。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowCandidateFacts {
    pub visible: bool,
    /// DWMWA_CLOAKED：UWP 挂起窗口 / 其他虚拟桌面的窗口，捕获会拿到空白。
    pub cloaked: bool,
    /// WS_EX_TOOLWINDOW：工具条/辅助窗，不是用户意义上的"应用窗口"。
    pub tool_window: bool,
    /// WS_EX_NOREDIRECTIONBITMAP：无重定向位图，DWM 捕获恒黑。
    pub no_redirection_bitmap: bool,
    /// 本进程自己的窗口（避免捕获到自家渲染/控制台）。
    pub own_process: bool,
    /// IsIconic：最小化窗口 DWM 不再合成新内容。
    pub minimized: bool,
    /// GetWindowTextLengthW。
    pub title_chars: usize,
    pub width: i32,
    pub height: i32,
}

/// 目标过滤谓词（需求：跳过 cloaked/工具窗/自己进程，另加无标题/零尺寸/
/// 最小化/无重定向位图这些"捕获必坏"的等价类）。
#[allow(dead_code)] // 仅被 #[cfg(windows)] 平台层调用；Linux 由单测覆盖
pub fn window_candidate_accepts(f: &WindowCandidateFacts) -> bool {
    f.visible
        && !f.cloaked
        && !f.tool_window
        && !f.no_redirection_bitmap
        && !f.own_process
        && !f.minimized
        && f.title_chars > 0
        && f.width > 0
        && f.height > 0
}

/// "C:\Windows\System32\notepad.exe" → "notepad"；取不到 → "unknown"。
#[allow(dead_code)]
pub fn app_name_from_image_path(path: &str) -> String {
    // 先剥尾部分隔符（防御性：QueryFullProcessImageNameW 不该给目录，但
    // "C:\x\" 这类输入若直接 rsplit 会得到空段 → 误报 unknown）。
    let trimmed = path.trim_end_matches(['\\', '/']);
    let base = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    // 去扩展名（只切最后一个 '.'，名字里含点的保留，如 "a.b.exe" → "a.b"）
    let stem = match base.rsplit_once('.') {
        Some((s, ext)) if !s.is_empty() && !ext.is_empty() => s,
        _ => base,
    };
    let name = stem.trim();
    if name.is_empty() {
        "unknown".to_string()
    } else {
        name.to_string()
    }
}

pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// list_targets 的一个应用窗口（字段名即 JSON 合同，见 docs/CAPTURE_WINDOWS.md）。
#[allow(dead_code)]
pub struct WindowTargetInfo {
    pub hwnd: isize,
    pub title: String,
    pub app_name: String,
    pub width: i32,
    pub height: i32,
}

/// list_targets 的一个显示器。
#[allow(dead_code)]
pub struct MonitorTargetInfo {
    pub index: i32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}

/// 目标列表 JSON（手写序列化：字段少且全数字/字符串，避免为 Windows 后端
/// 新增 serde 依赖面）。schema：
/// {"available":bool,"reason":"…","windows":[{"hwnd":N,"title":"…",
///  "app_name":"…","width":N,"height":N}],"monitors":[{"index":N,
///  "name":"…","x":N,"y":N,"width":N,"height":N,"primary":bool}]}
#[allow(dead_code)]
pub fn build_targets_json(
    available: bool,
    reason: Option<&str>,
    windows: &[WindowTargetInfo],
    monitors: &[MonitorTargetInfo],
) -> String {
    let mut s = String::from("{\"available\":");
    s.push_str(if available { "true" } else { "false" });
    if let Some(r) = reason {
        s.push_str(",\"reason\":\"");
        s.push_str(&json_escape(r));
        s.push('"');
    }
    s.push_str(",\"windows\":[");
    for (i, w) in windows.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"hwnd\":{},\"title\":\"{}\",\"app_name\":\"{}\",\"width\":{},\"height\":{}}}",
            w.hwnd,
            json_escape(&w.title),
            json_escape(&w.app_name),
            w.width,
            w.height
        ));
    }
    s.push_str("],\"monitors\":[");
    for (i, m) in monitors.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"index\":{},\"name\":\"{}\",\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"primary\":{}}}",
            m.index,
            json_escape(&m.name),
            m.x,
            m.y,
            m.width,
            m.height,
            m.primary
        ));
    }
    s.push_str("]}");
    s
}

/// 已启动会话的跨线程句柄。COM 对象全在捕获线程内部持有/销毁，
/// 这里只有纯同步原语（模式同 audio_capture::RunningSession）。
pub struct ActiveSession {
    pub kind: CaptureKind,
    /// 置位后捕获线程在 poll 间隔内自退出并在线程内完成 COM teardown。
    pub stop: Arc<AtomicBool>,
    pub thread: Option<JoinHandle<()>>,
    pub slot: Arc<FrameSlot>,
}

/// 单会话（与 portal 路径一致：同一时间一个捕获；换目标 = 先 stop 再 start）。
static ACTIVE: Mutex<Option<ActiveSession>> = Mutex::new(None);
/// start/stop 串行化，避免两个 start 交错、或 start 半途被 stop 插队
///（同 audio_capture::SESSION_LIFECYCLE 的教训）。
static SESSION_LIFECYCLE: Mutex<()> = Mutex::new(());

/// join 预算：捕获线程的退出延迟 ≤ poll 间隔（~10ms）+ 一次读回（<5ms），
/// 3s 给 D3D/驱动 stall 兜底；超时则 forget handle（线程仍会自行收尾，
/// 不存在跨线程 COM 操作），语义同 audio_capture::join_timeout。
const STOP_JOIN_BUDGET: Duration = Duration::from_millis(3000);
/// start 回执等待：首次 RoGetActivationFactory/D3D11 初始化在冷系统上可慢。
const START_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// stop 结果（对外拼进报告字符串，供 Java 日志与 UI 反馈——portal 的
/// void stop 让"线程到底没退出"不可观测，这里必须可观测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReport {
    /// 本来就没有会话（幂等路径：重复 stop / 从未 start）。
    NoSession,
    /// 捕获线程在线程内完成 COM teardown 并被 join。
    Joined,
    /// 超时未 join：线程仍会自退出（stop 标志已置位），handle 被遗忘。
    ThreadTimeoutDetached,
}

impl StopReport {
    pub fn as_str(&self) -> &'static str {
        match self {
            StopReport::NoSession => "no-session",
            StopReport::Joined => "joined",
            StopReport::ThreadTimeoutDetached => "thread-timeout-detached",
        }
    }
}

/// join with budget；true=线程已结束。超时 forget handle（线程仍自行收尾，
/// 不存在跨线程 COM 操作）防二次 join——模式即 audio_capture::join_timeout。
pub fn join_with_timeout(handle: JoinHandle<()>, budget: Duration) -> bool {
    let start = Instant::now();
    loop {
        if handle.is_finished() {
            // 线程函数返回后 join 立即完成；其 Drop（COM 对象）已在返回前执行。
            let _ = handle.join();
            return true;
        }
        if start.elapsed() >= budget {
            std::mem::forget(handle);
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 对外停止入口（lifecycle 锁内走 stop_locked，语义同 audio_capture）。
pub fn stop_capture() -> StopReport {
    let _lifecycle = SESSION_LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut guard = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    stop_locked(&mut guard, STOP_JOIN_BUDGET)
}

/// stop 主体（必须持 SESSION_LIFECYCLE 调）：摘全局 → 置 stop 标志 →
/// join（带预算）→ 帧缓冲清空。幂等：Option::take，第二次得到 NoSession。
pub fn stop_locked(active: &mut Option<ActiveSession>, budget: Duration) -> StopReport {
    let Some(session) = active.take() else {
        return StopReport::NoSession;
    };
    // 解构移交：stop 标志先置位（线程在 poll 间隔内自退出），再 join。
    let ActiveSession { stop, thread, slot, .. } = session;
    stop.store(true, Ordering::Release);
    // thread 字段只在 stop 路径 take，此处必然 Some；None 兜底按已停处理。
    let report = match thread {
        Some(h) => {
            if join_with_timeout(h, budget) {
                StopReport::Joined
            } else {
                StopReport::ThreadTimeoutDetached
            }
        }
        None => StopReport::Joined,
    };
    // 帧缓冲释放：即便 handle 超时被遗忘，线程后续只会 publish 进已无人引用的
    // slot（Arc<FrameSlot> 被 session 移交给线程共享），再随线程退出整体 Drop。
    slot.clear();
    eprintln!("[win-capture] stop: {report:?}");
    report
}

/// 目标列表 JSON。非 Windows 返回 Err（Java 侧只在 Windows 接线，见文档）。
pub fn list_targets() -> Result<String, String> {
    #[cfg(windows)]
    return platform::list_targets_json();
    #[cfg(not(windows))]
    {
        Err("win_capture_unsupported_on_this_platform".to_string())
    }
}

/// 捕获指定顶层窗口（hwnd 来自 list_targets 的 "hwnd" 字段）。
pub fn start_window_capture(hwnd: isize) -> Result<String, String> {
    #[cfg(windows)]
    return platform::start_capture(CaptureKind::Window(hwnd));
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        Err("win_capture_unsupported_on_this_platform".to_string())
    }
}

/// 捕获指定显示器（index 来自 list_targets monitors 数组，0 起）。
pub fn start_monitor_capture(index: i32) -> Result<String, String> {
    #[cfg(windows)]
    return platform::start_capture(CaptureKind::Monitor(index));
    #[cfg(not(windows))]
    {
        let _ = index;
        Err("win_capture_unsupported_on_this_platform".to_string())
    }
}

/// 取最新帧（seq 去重；无新帧/无会话 → None）。
pub fn take_frame() -> Option<FrameData> {
    let guard = ACTIVE.lock().ok()?;
    guard.as_ref().and_then(|s| s.slot.take())
}

/// 取最新帧的 wire 编码（bridge 直用）。
pub fn take_frame_wire() -> Option<Vec<u8>> {
    let frame = take_frame()?;
    Some(encode_frame_wire(&frame))
}

/// 当前是否有会话（诊断用）。
pub fn is_active() -> bool {
    ACTIVE
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false)
}

// ═══════════════════════════════════════════════════════
// Windows 平台层（Windows 10 1903+ / Windows 11）
// ═══════════════════════════════════════════════════════

#[cfg(windows)]
mod platform {
    use super::*;

    use std::ffi::c_void;
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use windows::core::{factory, Interface};
    use windows::Graphics::Capture::{
        Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::{IDirect3DDevice, IDirect3DSurface};
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Graphics::{SizeInt32};
    use windows::Foundation::TimeSpan;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
        D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HMONITOR, HDC, MONITORINFO, MONITORINFOEXW,
        MONITORENUMPROC,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, MONITORINFOF_PRIMARY,
        WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WNDENUMPROC,
    };

    /// 目标节流：30fps（100ns 单位）。渲染消费上限 24-30fps，读回成本由
    /// MinUpdateInterval 直接封顶，比"收满 60/120 再丢"省整条 staging 拷贝。
    const MIN_UPDATE_INTERVAL_100NS: i64 = 333_333;
    /// TryGetNextFrame 无帧时的轮询间隔。
    const POLL_INTERVAL: Duration = Duration::from_millis(10);
    /// 帧池缓冲数：FreeThreaded 惯例 2-3；取最新即可，积压无用。
    const POOL_BUFFERS: i32 = 2;

    // ── 目标枚举 ────────────────────────────────────────────────

    struct EnumWinCtx {
        items: Vec<WindowTargetInfo>,
        own_pid: u32,
    }

    pub fn list_targets_json() -> Result<String, String> {
        // IsSupported = 进程可激活 GraphicsCaptureSession（1903+ 且企业策略未禁用）。
        let available = GraphicsCaptureSession::IsSupported().unwrap_or(false);
        if !available {
            return Ok(build_targets_json(
                false,
                Some("Windows.Graphics.Capture unavailable (requires Windows 10 1903+, capture disabled by policy, or non-Desktop SKU)"),
                &[],
                &[],
            ));
        }
        let windows = enumerate_windows();
        let monitors = enumerate_monitor_infos();
        Ok(build_targets_json(true, None, &windows, &monitors))
    }

    fn enumerate_windows() -> Vec<WindowTargetInfo> {
        let mut ctx = EnumWinCtx {
            items: Vec::new(),
            own_pid: unsafe { GetCurrentProcessId() },
        };
        // 显式标注回调类型：fn item 只在类型位置已知的绑定处强制 coerce 成
        // fn pointer（Option 的 Some(...) 推导会保留 fn item 类型 → E0308）。
        let cb: WNDENUMPROC = Some(enum_windows_proc);
        let lparam = LPARAM(&mut ctx as *mut EnumWinCtx as isize);
        unsafe {
            let _ = EnumWindows(cb, lparam);
        }
        ctx.items
    }

    unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut EnumWinCtx) };
        collect_window(ctx, hwnd);
        // 恒 continue：单个窗口的判定失败不影响枚举。
        BOOL::from(true)
    }

    fn collect_window(ctx: &mut EnumWinCtx, hwnd: HWND) {
        if hwnd.is_invalid() {
            return;
        }
        let mut pid: u32 = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        let mut rect = RECT::default();
        let has_rect = unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok();
        let facts = WindowCandidateFacts {
            visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
            cloaked: query_cloaked(hwnd),
            tool_window: ex_style_has(hwnd, WS_EX_TOOLWINDOW.0),
            no_redirection_bitmap: ex_style_has(hwnd, WS_EX_NOREDIRECTIONBITMAP.0),
            own_process: pid != 0 && pid == ctx.own_pid,
            minimized: unsafe { IsIconic(hwnd) }.as_bool(),
            title_chars: unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize,
            width: if has_rect { rect.right - rect.left } else { 0 },
            height: if has_rect { rect.bottom - rect.top } else { 0 },
        };
        if !window_candidate_accepts(&facts) {
            return;
        }
        let title = get_window_text(hwnd, facts.title_chars);
        ctx.items.push(WindowTargetInfo {
            hwnd: hwnd.0 as isize,
            app_name: app_name_of(pid),
            title,
            width: facts.width,
            height: facts.height,
        });
    }

    fn ex_style_has(hwnd: HWND, flag: u32) -> bool {
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        (ex as u64 & flag as u64) != 0
    }

    fn query_cloaked(hwnd: HWND) -> bool {
        let mut cloaked: u32 = 0;
        unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut c_void,
                std::mem::size_of::<u32>() as u32,
            )
        }
        .is_ok()
            && cloaked != 0
    }

    fn get_window_text(hwnd: HWND, len_hint: usize) -> String {
        let mut buf = vec![0u16; len_hint + 2];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }

    fn app_name_of(pid: u32) -> String {
        let path = query_image_path(pid).unwrap_or_default();
        app_name_from_image_path(&path)
    }

    fn query_image_path(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        unsafe {
            let hprocess =
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut size = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                hprocess,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
            .is_ok();
            let _ = windows::Win32::Foundation::CloseHandle(hprocess);
            if ok && size > 0 && (size as usize) <= buf.len() {
                Some(String::from_utf16_lossy(&buf[..size as usize]))
            } else {
                None
            }
        }
    }

    struct EnumMonCtx {
        items: Vec<(isize, MonitorTargetInfo)>, // (HMONITOR raw, info)
    }

    /// 枚举显示器。返回 (hmon raw, info)——index 即 info.index，
    /// start_monitor_capture 按 index 取 hmon。
    fn enumerate_monitors_raw() -> Vec<(isize, MonitorTargetInfo)> {
        let mut ctx = EnumMonCtx { items: Vec::new() };
        let cb: MONITORENUMPROC = Some(enum_display_monitors_proc);
        let lparam = LPARAM(&mut ctx as *mut EnumMonCtx as isize);
        unsafe {
            let _ = EnumDisplayMonitors(HDC::default(), None, cb, lparam);
        }
        ctx.items
    }

    fn enumerate_monitor_infos() -> Vec<MonitorTargetInfo> {
        enumerate_monitors_raw().into_iter().map(|(_, m)| m).collect()
    }

    unsafe extern "system" fn enum_display_monitors_proc(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut EnumMonCtx) };
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let ok = unsafe {
            GetMonitorInfoW(hmonitor, &mut mi.monitorInfo as *mut MONITORINFO).as_bool()
        };
        if ok {
            let r = mi.monitorInfo.rcMonitor;
            let index = ctx.items.len() as i32;
            ctx.items.push((
                hmonitor.0 as isize,
                MonitorTargetInfo {
                    index,
                    name: utf16_array_to_string(&mi.szDevice),
                    x: r.left,
                    y: r.top,
                    width: r.right - r.left,
                    height: r.bottom - r.top,
                    primary: (mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0,
                },
            ));
        }
        BOOL::from(true)
    }

    fn utf16_array_to_string(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    // ── 会话启动 ────────────────────────────────────────────────

    /// 初始帧尺寸回退值：GraphicsCaptureItem::Size() 在个别驱动/版本上返回
    /// (0,0)（真机边界，见 CAPTURE_WINDOWS.md 已知风险），此时用 Win32 几何。
    fn fallback_size(kind: CaptureKind) -> (u32, u32) {
        match kind {
            CaptureKind::Window(h) => {
                let hwnd = HWND(h as *mut c_void);
                let mut rect = RECT::default();
                if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
                    (
                        (rect.right - rect.left).max(0) as u32,
                        (rect.bottom - rect.top).max(0) as u32,
                    )
                } else {
                    (0, 0)
                }
            }
            CaptureKind::Monitor(i) => {
                let mons = enumerate_monitors_raw();
                match mons.into_iter().nth(i as usize) {
                    Some((_, m)) => (m.width.max(0) as u32, m.height.max(0) as u32),
                    None => (0, 0),
                }
            }
        }
    }

    pub fn start_capture(kind: CaptureKind) -> Result<String, String> {
        let _lifecycle = SESSION_LIFECYCLE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut guard = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());

        // 单会话模型：换目标先确定性关掉旧会话（与 portal 路径"同一时间一个
        // 捕获"一致；stop_locked 幂等，没有旧会话时零成本）。
        stop_locked(&mut guard, STOP_JOIN_BUDGET);

        let hmon_raw: isize = match kind {
            CaptureKind::Window(_) => 0,
            CaptureKind::Monitor(i) => {
                let mons = enumerate_monitors_raw();
                let (h, _) = mons.into_iter().nth(i as usize).ok_or_else(|| {
                    format!("monitor index {i} out of range")
                })?;
                h
            }
        };
        let initial = fallback_size(kind);

        let (ack_tx, ack_rx) = std::sync::mpsc::channel::<Result<String, String>>();
        let stop = Arc::new(AtomicBool::new(false));
        let slot = Arc::new(FrameSlot::new());

        let thread_stop = stop.clone();
        let thread_slot = slot.clone();
        let handle = std::thread::Builder::new()
            .name("wc-win-capture".to_string())
            .spawn(move || {
                capture_thread_body(kind, hmon_raw, initial, thread_slot, thread_stop, ack_tx)
            })
            .map_err(|e| format!("spawn wc-win-capture: {e}"))?;

        // 同步回执：COM/WinRT 建项成败只有捕获线程自己知道（RoGetActivationFactory
        // 必须在 MTA 已初始化的线程上跑）。
        match ack_rx.recv_timeout(START_ACK_TIMEOUT) {
            Ok(Ok(report)) => {
                *guard = Some(ActiveSession {
                    kind,
                    stop,
                    thread: Some(handle),
                    slot,
                });
                Ok(report)
            }
            Ok(Err(e)) => {
                // setup 失败：线程自行退出（未建会话资源或已在线程内清理）。
                let _ = join_with_timeout(handle, STOP_JOIN_BUDGET);
                Err(e)
            }
            Err(_) => {
                eprintln!("[win-capture] setup ack timeout (>{}s)", START_ACK_TIMEOUT.as_secs());
                stop.store(true, Ordering::Release);
                let _ = join_with_timeout(handle, STOP_JOIN_BUDGET);
                Err("win_capture_setup_timeout".to_string())
            }
        }
    }

    /// 线程主体：COM 生命周期完全在本线程内（CoInitializeEx → 建项/收帧/读回
    /// → Close/Dispose/Release → CoUninitialize）。外部世界只有 stop 原子标志
    /// 和帧槽——不存在跨线程 COM 调用，这是"僵尸线程"问题的根治点：
    /// 线程退出路径不依赖任何外部对象存活。
    fn capture_thread_body(
        kind: CaptureKind,
        hmon_raw: isize,
        initial: (u32, u32),
        slot: Arc<FrameSlot>,
        stop: Arc<AtomicBool>,
        ack: Sender<Result<String, String>>,
    ) {
        let com = ComGuard::init();
        match capture_flow(kind, hmon_raw, initial, &slot, &stop, &ack) {
            Ok(()) => {}
            Err(e) => {
                let _ = ack.send(Err(e));
            }
        }
        drop(com); // CoUninitialize：本线程所有 COM 引用已在此前释放
    }

    struct ComGuard {
        owns: bool,
    }

    impl ComGuard {
        fn init() -> Self {
            // 本线程 MTA 初始化。已初始化（S_FALSE）或 JVM 已按别的模式初始化
            // （RPC_E_CHANGED_MODE）都能继续用（后者说明 COM 已活，不归我们关）。
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            ComGuard {
                owns: hr.is_ok(),
            }
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.owns {
                unsafe { CoUninitialize() };
            }
        }
    }

    /// 建项 → 建池 → 建会话 → StartCapture →（线程内 ack 成功）→ 帧循环 →
    /// 逆序 teardown。返回 Err 仅表示 setup 失败（未进入帧循环）。
    fn capture_flow(
        kind: CaptureKind,
        hmon_raw: isize,
        initial: (u32, u32),
        slot: &Arc<FrameSlot>,
        stop: &AtomicBool,
        ack: &Sender<Result<String, String>>,
    ) -> Result<(), String> {
        // 1. D3D11 设备 + 立即上下文（BGRA support 显式置位）。
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
        let device = device.ok_or("D3D11CreateDevice: null device")?;
        let context = context.ok_or("D3D11CreateDevice: null context")?;

        // 2. Win32 device → Windows.Graphics.DirectX Direct3DDevice。
        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|e| format!("cast IDXGIDevice: {e}"))?;
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice: {e}"))?;
        let d3d: IDirect3DDevice = inspectable
            .cast()
            .map_err(|e| format!("cast IDirect3DDevice: {e}"))?;
        drop(inspectable);
        drop(dxgi);

        // 3. 捕获项：IGraphicsCaptureItemInterop 是 GraphicsCaptureItem 工厂上的
        //    Win32 interop 接口（factory! 取工厂，无需 RoActivateInstance）。
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("GraphicsCaptureItem factory (Win10 1903+?): {e}"))?;
        let item = unsafe {
            match kind {
                CaptureKind::Window(h) => interop
                    .CreateForWindow::<HWND, GraphicsCaptureItem>(HWND(h as *mut c_void)),
                CaptureKind::Monitor(_) => interop
                    .CreateForMonitor::<HMONITOR, GraphicsCaptureItem>(HMONITOR(hmon_raw as *mut c_void)),
            }
        }
        .map_err(|e| format!("CreateFor{}: {e}", if matches!(kind, CaptureKind::Window(_)) { "Window" } else { "Monitor" }))?;

        // 4. 初始尺寸：优先 item.Size()（DWM 合成尺寸），(0,0) 时回退 Win32 几何。
        let mut size = item.Size().unwrap_or(SizeInt32 {
            Width: initial.0 as i32,
            Height: initial.1 as i32,
        });
        if size.Width <= 0 || size.Height <= 0 {
            size = SizeInt32 {
                Width: initial.0 as i32,
                Height: initial.1 as i32,
            };
        }
        if size.Width <= 0 || size.Height <= 0 {
            return Err("zero-size capture target (window closed?)".to_string());
        }

        // 5. 帧池 + 会话。FreeThreaded：无需 dispatcherqueue，任意线程
        //    TryGetNextFrame——正是"独立捕获线程轮询收帧"模型的前提。
        let mut pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            POOL_BUFFERS,
            size,
        )
        .map_err(|e| format!("CreateFreeThreaded: {e}"))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|e| format!("CreateCaptureSession: {e}"))?;

        // 黄色隐私边框（Win11 默认必开，游戏内观感差）与鼠标指针都关掉。
        // SetIsBorderRequired 在部分企业策略/构建上可能失败——失败仅降级
        // （边框保留），不中断捕获；两个标志的可用性经由 IGraphicsCaptureSession2/3
        // 在类上聚合，0.58 直接暴露 Set*。
        if let Err(e) = session.SetIsCursorCaptureEnabled(false) {
            eprintln!("[win-capture] cursor capture flag unsupported: {e}");
        }
        if let Err(e) = session.SetIsBorderRequired(false) {
            eprintln!("[win-capture] border flag unsupported (yellow border stays): {e}");
        }
        let _ = session.SetMinUpdateInterval(TimeSpan {
            Duration: MIN_UPDATE_INTERVAL_100NS,
        });

        session
            .StartCapture()
            .map_err(|e| format!("StartCapture: {e}"))?;

        let _ = ack.send(Ok(format!("ok:{}", kind.session_id())));
        eprintln!(
            "[win-capture] started {:?} size={}x{}",
            kind, size.Width, size.Height
        );

        // 6. 帧循环。staging 纹理复用；尺寸变化（窗口拖拽/最大化/DPI）时
        //    pool.Recreate 同步缓冲尺寸并重建 staging——portal 无 resize 处理，
        //    这里必须处理，否则窗口一改尺寸就永久黑屏。
        let mut cur_w = size.Width.max(0) as u32;
        let mut cur_h = size.Height.max(0) as u32;
        let mut staging: Option<ID3D11Texture2D> = None;
        let mut frame_errs: u32 = 0;

        while !stop.load(Ordering::Acquire) {
            let frame = match pool.TryGetNextFrame() {
                Ok(f) => f,
                Err(_) => {
                    // 无帧（含超时/池空）：标准轮询节拍。
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }
            };
            match readback_frame(&device, &context, &d3d, &mut pool, &mut staging,
                                 &mut cur_w, &mut cur_h, &frame, slot) {
                Ok(()) => frame_errs = 0,
                Err(e) => {
                    frame_errs += 1;
                    // 节流：坏帧风暴只报前几次 + 每 60 次一次。
                    if frame_errs <= 3 || frame_errs % 60 == 0 {
                        eprintln!("[win-capture] frame error #{frame_errs}: {e}");
                    }
                }
            }
            // frame 在此 drop → 缓冲归还池；下一轮 TryGetNextFrame 拿最新。
        }

        // 7. 确定性 teardown（逆构造序）：先停源（session），再停池，
        //    释放 staging，其余 COM 对象随栈 Drop 逐个 Release。全程本线程。
        if let Err(e) = session.Close() {
            eprintln!("[win-capture] session.Close: {e}");
        }
        drop(session);
        if let Err(e) = pool.Close() {
            eprintln!("[win-capture] pool.Close: {e}");
        }
        drop(pool);
        drop(staging);
        drop(item);
        drop(d3d);
        drop(context);
        drop(device);
        eprintln!("[win-capture] thread exiting (deterministic teardown done)");
        Ok(())
    }

    /// 单帧 CPU 读回：frame.Surface() → IDXGISurface → 同设备 staging
    /// CopyResource → Map(READ) → 按 RowPitch 压实 → publish。
    #[allow(clippy::too_many_arguments)]
    fn readback_frame(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        d3d: &IDirect3DDevice,
        pool: &mut Direct3D11CaptureFramePool,
        staging: &mut Option<ID3D11Texture2D>,
        cur_w: &mut u32,
        cur_h: &mut u32,
        frame: &windows::Graphics::Capture::Direct3D11CaptureFrame,
        slot: &FrameSlot,
    ) -> Result<(), String> {
        let surface: IDirect3DSurface = frame
            .Surface()
            .map_err(|e| format!("frame.Surface: {e}"))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|e| format!("cast IDirect3DDxgiInterfaceAccess: {e}"))?;
        // 捕获表面即本设备上的 ID3D11Texture2D（BGRA8）。
        let tex: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|e| format!("GetInterface(ID3D11Texture2D): {e}"))?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(format!("unexpected surface format {:?}", desc.Format.0));
        }
        let w = desc.Width;
        let h = desc.Height;
        if w == 0 || h == 0 {
            return Err("zero-size frame".to_string());
        }

        if w != *cur_w || h != *cur_h || staging.is_none() {
            // 尺寸变化：池缓冲跟上（否则后续帧永远与池不匹配），staging 重建。
            let new_size = SizeInt32 {
                Width: w as i32,
                Height: h as i32,
            };
            if let Err(e) = pool.Recreate(
                d3d,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                POOL_BUFFERS,
                new_size,
            ) {
                return Err(format!("pool.Recreate({w}x{h}): {e}"));
            }
            let sdesc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut t: Option<ID3D11Texture2D> = None;
            unsafe { device.CreateTexture2D(&sdesc, None, Some(&mut t)) }
                .map_err(|e| format!("CreateTexture2D(staging {w}x{h}): {e}"))?;
            eprintln!("[win-capture] resize: {}x{} -> {}x{}", *cur_w, *cur_h, w, h);
            *staging = t;
            *cur_w = w;
            *cur_h = h;
        }

        let staging_tex = staging
            .as_ref()
            .ok_or_else(|| "staging texture missing".to_string())?;
        unsafe { context.CopyResource(staging_tex, &tex) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let map_res =
            unsafe { context.Map(staging_tex, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) };
        if let Err(e) = map_res {
            return Err(format!("Map: {e}"));
        }

        // BGRA8 内存布局 == 小端 u32 ARGB8888：逐字节原样搬运即"转 ARGB8888"，
        // 无 per-pixel 置换。RowPitch 可能大于 w*4（对齐），按行压实。
        let row = w as usize * 4;
        let mut pixels = vec![0u8; row * h as usize];
        unsafe {
            let src = mapped.pData as *const u8;
            if !src.is_null() {
                let pitch = mapped.RowPitch as usize;
                if pitch == row {
                    std::ptr::copy_nonoverlapping(src, pixels.as_mut_ptr(), row * h as usize);
                } else {
                    for y in 0..h as usize {
                        std::ptr::copy_nonoverlapping(
                            src.add(y * pitch),
                            pixels.as_mut_ptr().add(y * row),
                            row,
                        );
                    }
                }
            }
        }
        unsafe { context.Unmap(staging_tex, 0) };

        slot.publish(w, h, pixels);
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════
// 纯逻辑单测（Linux 可跑；cfg 无关部分全部在此覆盖）
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── 目标过滤谓词 ───────────────────────────────────────────

    fn good_facts() -> WindowCandidateFacts {
        WindowCandidateFacts {
            visible: true,
            cloaked: false,
            tool_window: false,
            no_redirection_bitmap: false,
            own_process: false,
            minimized: false,
            title_chars: 10,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn window_filter_accepts_normal_window() {
        assert!(window_candidate_accepts(&good_facts()));
    }

    #[test]
    fn window_filter_rejects_each_bad_class() {
        let mut f;

        f = good_facts();
        f.visible = false;
        assert!(!window_candidate_accepts(&f), "invisible");

        f = good_facts();
        f.cloaked = true;
        assert!(!window_candidate_accepts(&f), "cloaked");

        f = good_facts();
        f.tool_window = true;
        assert!(!window_candidate_accepts(&f), "tool window");

        f = good_facts();
        f.no_redirection_bitmap = true;
        assert!(!window_candidate_accepts(&f), "no redirection bitmap");

        f = good_facts();
        f.own_process = true;
        assert!(!window_candidate_accepts(&f), "own process");

        f = good_facts();
        f.minimized = true;
        assert!(!window_candidate_accepts(&f), "minimized");

        f = good_facts();
        f.title_chars = 0;
        assert!(!window_candidate_accepts(&f), "empty title");

        f = good_facts();
        f.width = 0;
        assert!(!window_candidate_accepts(&f), "zero width");

        f = good_facts();
        f.height = -5;
        assert!(!window_candidate_accepts(&f), "negative height");
    }

    #[test]
    fn window_filter_allows_one_pixel_window() {
        // 边界：1px 仍算有效目标（内容虽小但存在）。
        let mut f = good_facts();
        f.width = 1;
        f.height = 1;
        assert!(window_candidate_accepts(&f));
    }

    // ── app_name / json ────────────────────────────────────────

    #[test]
    fn app_name_strips_path_and_extension() {
        assert_eq!(app_name_from_image_path("C:\\Windows\\System32\\notepad.exe"), "notepad");
        // 目录名带点不影响：取的是 basename 去扩展名。
        assert_eq!(app_name_from_image_path("C:\\Program Files\\a.b\\app.exe"), "app");
    }

    #[test]
    fn app_name_edge_cases() {
        assert_eq!(app_name_from_image_path(""), "unknown");
        assert_eq!(app_name_from_image_path("C:\\x\\"), "x");
        assert_eq!(app_name_from_image_path("chrome"), "chrome");
        // 点开头的名字 stem 为空，按整段保留（罕见，不构成误判为 unknown 的路径）。
        assert_eq!(app_name_from_image_path("C:\\a\\.exe"), ".exe");
        assert_eq!(app_name_from_image_path("C:\\微信 WeChat.exe"), "微信 WeChat");
    }

    #[test]
    fn json_escape_quotes_and_control() {
        assert_eq!(json_escape("a\"b\\c\nd\te\u{1}"), "a\\\"b\\\\c\\nd\\te\\u0001");
        assert_eq!(json_escape("ok"), "ok");
    }

    #[test]
    fn targets_json_schema() {
        let wins = vec![WindowTargetInfo {
            hwnd: 0x1234,
            title: "Untitled — Notepad\"x".to_string(),
            app_name: "notepad".to_string(),
            width: 800,
            height: 600,
        }];
        let mons = vec![MonitorTargetInfo {
            index: 0,
            name: "\\\\.\\DISPLAY1".to_string(),
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
            primary: true,
        }];
        let json = build_targets_json(true, None, &wins, &mons);
        // 粗校验：能被逐字段断言的扁平结构（不引 serde 依赖）。
        assert!(json.starts_with("{\"available\":true,\"windows\":["));
        assert!(json.contains("\"hwnd\":4660"));
        assert!(json.contains("Notepad\\\"x"));
        assert!(json.contains("\"app_name\":\"notepad\""));
        assert!(json.contains("\"width\":800,\"height\":600"));
        assert!(json.contains("\"index\":0"));
        assert!(json.contains("\"primary\":true"));
        // 转义后再解析回去验证 round-trip（手动检查无未转义引号即可）。
        let body = json.as_str();
        let mut in_str = false;
        let mut esc = false;
        let mut depth = 0i32;
        for ch in body.chars() {
            if esc {
                esc = false;
                continue;
            }
            match ch {
                '\\' if in_str => esc = true,
                '"' => in_str = !in_str,
                '{' | '[' if !in_str => depth += 1,
                '}' | ']' if !in_str => depth -= 1,
                _ => {}
            }
        }
        assert!(!in_str, "unterminated string");
        assert_eq!(depth, 0, "unbalanced brackets");
    }

    #[test]
    fn targets_json_unavailable_carries_reason() {
        let json = build_targets_json(false, Some("need 1903+"), &[], &[]);
        assert!(json.starts_with("{\"available\":false,\"reason\":\"need 1903+\""));
        assert!(json.contains("\"windows\":[]"));
        assert!(json.contains("\"monitors\":[]"));
    }

    // ── session id 编码 ───────────────────────────────────────

    #[test]
    fn session_ids_are_disjoint_domains() {
        // 窗口 = 正（hwnd），显示器 = 负（-1-index），互不冲突且不与
        // portal 的 PipeWire node id（正小整数）语义混淆——Java 单会话即可。
        assert_eq!(CaptureKind::Window(0x1234).session_id(), 0x1234);
        assert_eq!(CaptureKind::Monitor(0).session_id(), -1);
        assert_eq!(CaptureKind::Monitor(3).session_id(), -4);
        assert!(CaptureKind::Window(1).session_id() > CaptureKind::Monitor(0).session_id());
    }

    // ── 帧槽状态机 ────────────────────────────────────────────

    #[test]
    fn slot_publish_take_dedup_cycle() {
        let slot = FrameSlot::new();
        assert!(!slot.has_new());
        assert!(slot.take().is_none(), "empty slot takes None");

        let s1 = slot.publish(4, 2, vec![9u8; 4 * 2 * 4]);
        assert_eq!(s1, 1);
        assert!(slot.has_new());
        let f = slot.take().expect("new frame available");
        assert_eq!(f.seq, 1);
        assert_eq!(f.width, 4);
        assert_eq!(f.height, 2);
        assert_eq!(f.pixels.len(), 32);
        assert!(!slot.has_new());
        assert!(slot.take().is_none(), "same seq deduped");

        let s2 = slot.publish(4, 2, vec![7u8; 4 * 2 * 4]);
        assert_eq!(s2, 2);
        let f2 = slot.take().expect("second frame");
        assert_eq!(f2.seq, 2);
        assert_eq!(&*f2.pixels, &[7u8; 32][..]);
    }

    #[test]
    fn slot_publish_many_takes_latest() {
        let slot = FrameSlot::new();
        for i in 1..=5u8 {
            slot.publish(1, 1, vec![i]);
        }
        let f = slot.take().expect("latest published frame");
        assert_eq!(f.seq, 5);
        assert_eq!(&*f.pixels, &[5u8][..]);
        assert!(slot.take().is_none());
    }

    #[test]
    fn slot_clear_keeps_seq_and_blocks_ghost() {
        let slot = FrameSlot::new();
        slot.publish(2, 2, vec![0u8; 16]);
        slot.clear();
        // 帧已被释放：即便水位落后也拿不到"幽灵帧"。
        assert!(slot.take().is_none());
        // clear 后继续发布，seq 不回绕（去重水位单调有效）。
        let s = slot.publish(2, 2, vec![1u8; 16]);
        assert_eq!(s, 2, "seq monotonic across clear");
        let f = slot.take().expect("post-clear frame");
        assert_eq!(f.seq, 2);
    }

    #[test]
    fn slot_take_returns_stable_frame_under_concurrent_publish() {
        // 生产者连续 publish、消费者随机 take：take 拿到的帧号必须严格单调
        // 不减（Arc 快照不被覆写）。
        let slot = Arc::new(FrameSlot::new());
        let producer = {
            let slot = slot.clone();
            std::thread::spawn(move || {
                for i in 0..2000u64 {
                    slot.publish(1, 1, vec![(i % 256) as u8; 4]);
                }
            })
        };
        let mut last_seq = 0u64;
        let mut got = 0usize;
        while !producer.is_finished() || slot.has_new() {
            if let Some(f) = slot.take() {
                assert!(f.seq > last_seq, "seq regressed: {} after {}", f.seq, last_seq);
                last_seq = f.seq;
                got += 1;
            } else {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
        assert!(got >= 1);
        assert!(last_seq >= got as u64);
    }

    // ── wire 编码 ─────────────────────────────────────────────

    #[test]
    fn frame_wire_header_golden() {
        let frame = FrameData {
            seq: 0xdead_beef_0001_2345, // u64 → 低 32 位上 wire
            width: 0x0403_0201,
            height: 0x0807_0605,
            pixels: Arc::from(vec![0xAAu8; 4]),
        };
        let wire = encode_frame_wire(&frame);
        assert_eq!(wire.len(), WIRE_HEADER_LEN + 4);
        assert_eq!(&wire[0..4], &0x0403_0201u32.to_le_bytes());
        assert_eq!(&wire[4..8], &0x0807_0605u32.to_le_bytes());
        assert_eq!(&wire[8..12], &0x0001_2345u32.to_le_bytes());
        assert_eq!(&wire[12..], &[0xAA; 4][..]);
    }

    // ── stop 幂等机 ───────────────────────────────────────────

    /// 假捕获线程：每 2ms 检查 stop 标志，置位即返回（模拟真实线程的
    /// POLL_INTERVAL 退出路径）。
    fn fake_session(sleep_poll: Duration, ignore_stop: bool) -> ActiveSession {
        let stop = Arc::new(AtomicBool::new(false));
        let slot = Arc::new(FrameSlot::new());
        slot.publish(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let t_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("wc-win-capture-fake".to_string())
            .spawn(move || {
                let started = Instant::now();
                while ignore_stop && started.elapsed() < Duration::from_millis(400) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                while !t_stop.load(Ordering::Acquire) {
                    std::thread::sleep(sleep_poll);
                }
                // 真实线程在这里做 COM teardown；假线程直接返回。
            })
            .unwrap();
        ActiveSession {
            kind: CaptureKind::Window(1),
            stop,
            thread: Some(handle),
            slot,
        }
    }

    #[test]
    fn stop_is_idempotent_and_joins_thread() {
        let mut active = Some(fake_session(Duration::from_millis(5), false));
        let session_slot = active.as_ref().unwrap().slot.clone();

        let r1 = stop_locked(&mut active, Duration::from_millis(2000));
        assert_eq!(r1, StopReport::Joined, "first stop joins");
        assert!(active.is_none());

        let r2 = stop_locked(&mut active, Duration::from_millis(2000));
        assert_eq!(r2, StopReport::NoSession, "second stop is a no-op");

        // 帧缓冲已释放（clear 在 stop 路径里）。
        assert!(session_slot.take().is_none(), "slot cleared by stop");
    }

    #[test]
    fn stop_detach_on_timeout_leaves_no_active_session() {
        // 超时路径：线程无视 stop 400ms，join 预算 80ms → ThreadTimeoutDetached；
        // 会话仍被摘除（ACTIVE 语义等价），帧槽已清。
        let mut active = Some(fake_session(Duration::from_millis(5), true));
        let r = stop_locked(&mut active, Duration::from_millis(80));
        assert_eq!(r, StopReport::ThreadTimeoutDetached);
        assert!(active.is_none());
    }

    #[test]
    fn join_with_timeout_fast_and_slow() {
        let quick = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(20));
        });
        assert!(join_with_timeout(quick, Duration::from_millis(2000)));

        let slow = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
        });
        assert!(!join_with_timeout(slow, Duration::from_millis(60)));
        // 超时路径 handle 已被 forget（线程 detach 后自行退出）；等它结束再收尾，
        // 不要求但保证测试进程内无常驻残线程。
        std::thread::sleep(Duration::from_millis(400));
    }

    #[test]
    fn global_stop_without_session_is_noop() {
        // Linux（无 start 可能）全局幂等：stop 无会话 → NoSession，不 panic。
        // 不手动持 SESSION_LIFECYCLE——stop_capture 内部已加该锁（std Mutex 非重入）。
        // 串行化到别的用例会互相污染全局 ACTIVE，但本用例不依赖别的用例的时序。
        assert_eq!(stop_capture(), StopReport::NoSession);
        assert_eq!(stop_capture(), StopReport::NoSession);
        assert!(!is_active());
        assert!(take_frame().is_none());
        assert!(take_frame_wire().is_none());
    }

    #[test]
    fn start_returns_error_on_non_windows() {
        #[cfg(not(windows))]
        {
            assert!(start_window_capture(1).is_err());
            assert!(start_monitor_capture(0).is_err());
            assert!(list_targets().is_err());
            // 失败路径不得污染全局：stop 仍报 no-session。
            assert_eq!(stop_capture(), StopReport::NoSession);
        }
    }
}
