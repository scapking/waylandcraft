//! macOS 窗口/显示器捕获后端（ScreenCaptureKit，macOS 14+）。
//!
//! 消费形态镜像 `portal_capture`（Linux XDG portal + PipeWire）：
//! Java 侧 start → 每 tick `take_frame(since_seq)` 拉帧 → stop。
//!
//! 与 portal 的关键差异（本模块的设计红线）：
//! - **stop 确定性**：置停止标志 + condvar 唤醒 + `join()` 有界回收捕获线程，
//!   所有 ObjC 对象（SCContentFilter/SCStreamConfiguration）只由捕获线程持有、
//!   线程退出时 drop——portal 的"pw-stream 僵尸线程"教训绝不重演。
//! - **帧槽带 seq**：`FrameSlot.publish` 单调递增序号，`poll(since)` 仅在
//!   seq 更大时返回新帧，UI 侧可跳过无变化帧（portal 无 seq，每 tick 盲拷）。
//! - **权限如实透出**：preflight →（未授）CGRequest 拉起系统弹窗 → 二次
//!   preflight 定论；denied 一律 `Err("permission_denied")`，不假成功。
//! - **版本门**：SCScreenshotManager 是 macOS 14+ 类，运行时探测类与 selector
//!   存在性（比按 OS 版本号猜更可靠）；macOS 13 及以下返回
//!   `Err("macos14_required")`——CGWindowListCreateImage 在 macOS 15 已失效，
//!   不作 fallback。
//! - **抓帧轮询**（上限 ~12fps），非 SCStream 全速流：objc2 手写
//!   SCStreamDelegate/SCStreamOutput delegate 在无法真机验证的条件下风险
//!   过高。选型结论与帧率上限见 docs/CAPTURE_MACOS.md。
//!
//! 模块分区：
//! - 本文件前半（cfg 无关）：窗口过滤、权限态机、帧槽状态机、CGImage 像素
//!   格式判定/转换、线协议编码——Linux `cargo test --lib` 直接测。
//! - `#[cfg(target_os = "macos")] mod ffi`：CoreGraphics / ScreenCaptureKit
//!   调用与捕获线程。非 macOS 平台对外 API 同签名、一律"不支持"。

// ═══════════════════════════════════════════════════════
// cfg 无关：纯逻辑（Linux 可单测）
// ═══════════════════════════════════════════════════════

use std::sync::Mutex;

/// 单个可捕获窗口的快照（来源：CGWindowListCopyWindowInfo）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// CGWindowID（== kCGWindowNumber）
    pub id: u32,
    pub owner: String,
    pub title: String,
    pub pid: i32,
    /// kCGWindowLayer：0 = 普通应用窗口；非 0 = 菜单/Dock/壁纸层等
    pub layer: i64,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 单个显示器快照（来源：CGGetActiveDisplayList）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    /// 稳定序号（= CGGetActiveDisplayList 下标）；`start_display` 入参
    pub index: u32,
    /// CGDirectDisplayID
    pub id: u32,
    /// 逻辑点尺寸（Retina 实际像素为其 scale 倍，首帧返回真实像素尺寸）
    pub width: u32,
    pub height: u32,
    pub builtin: bool,
    pub main: bool,
}

/// 过滤窗口枚举结果：只保留普通应用窗口，排除自身与退化项（保序、按 id 去重）。
///
/// 规则：
/// - `layer == 0`（普通窗口）
/// - 宽高为正
/// - 排除本进程 pid（游戏自己的 GLFW 窗口）
/// - 同 id 只保留首个（CG 返回已按 z 序）
pub fn filter_windows(raw: Vec<WindowInfo>, self_pid: i32) -> Vec<WindowInfo> {
    let mut seen_ids = Vec::with_capacity(raw.len());
    raw.into_iter()
        .filter(|w| w.layer == 0 && w.width > 0 && w.height > 0 && w.pid != self_pid)
        .filter(|w| {
            if seen_ids.contains(&w.id) {
                false
            } else {
                seen_ids.push(w.id);
                true
            }
        })
        .collect()
}

/// 屏幕录制权限状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermState {
    /// 尚未探测 / 非 macOS 平台
    Unknown,
    Granted,
    Denied,
}

/// 权限探测流程的纯态机。
///
/// 流程：`preflight1 = CGPreflightScreenCaptureAccess()`；未通过时
/// `requested = Some(CGRequestScreenCaptureAccess())`（拉起系统弹窗；其返回值
/// 只代表"调用瞬间是否已授权"，不能作为终态）；随后
/// `preflight2 = CGPreflightScreenCaptureAccess()` 定论。
/// `requested = None` 表示 request API 不可用，此时以 preflight1 为唯一证据。
pub fn perm_state(preflight1: bool, requested: Option<bool>, preflight2: bool) -> PermState {
    if preflight1 {
        return PermState::Granted;
    }
    match requested {
        None => PermState::Denied,
        Some(_) if preflight2 => PermState::Granted,
        Some(_) => PermState::Denied,
    }
}

/// 一帧捕获结果（ARGB8888：内存字节序 B,G,R,A，即小端 uint32 0xAARRGGBB）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotFrame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// `FrameSlot::poll` 的三态结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePoll {
    /// 有新帧
    Fresh,
    /// 会话已终止（目标窗口关闭 / 连续抓帧失败），不会再有新帧
    Dead,
    /// 暂无比 since 更新的帧
    Idle,
}

struct SlotInner {
    seq: u64,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    dead: bool,
    /// publish 之后、clear 之前为 true——clear 后即便 seq 大于调用方的
    /// since 也不得再交出（像素已释放）。
    valid: bool,
}

/// 共享帧槽：单槽、最新帧覆盖 + 单调 seq。
///
/// 捕获线程 `publish/mark_dead`，JNI 线程 `poll`；`clear` 只在 stop 的 join
/// 完成后调用（此刻已无并发写者）。锁中毒一律 `into_inner` 取回数据——
/// release 构建 panic=abort，正常路径零 panic，这里只是纵深防御。
pub struct FrameSlot {
    inner: Mutex<SlotInner>,
}

impl FrameSlot {
    pub fn new() -> Self {
        FrameSlot {
            inner: Mutex::new(SlotInner {
                seq: 0,
                width: 0,
                height: 0,
                pixels: Vec::new(),
                dead: false,
                valid: false,
            }),
        }
    }

    /// 写入新帧，返回新 seq（从 1 起单调递增）。
    pub fn publish(&self, width: u32, height: u32, pixels: Vec<u8>) -> u64 {
        let mut g = lock(&self.inner);
        g.seq = g.seq.wrapping_add(1);
        g.width = width;
        g.height = height;
        g.pixels = pixels;
        g.valid = true;
        g.seq
    }

    pub fn mark_dead(&self) {
        lock(&self.inner).dead = true;
    }

    pub fn is_dead(&self) -> bool {
        lock(&self.inner).dead
    }

    /// 当前帧序号（未 publish 过为 0）。
    pub fn seq(&self) -> u64 {
        lock(&self.inner).seq
    }

    /// 若 `seq > since_seq` 则克隆返回当前帧。
    pub fn take(&self, since_seq: u64) -> Option<SlotFrame> {
        let g = lock(&self.inner);
        if g.valid && g.seq > since_seq {
            Some(SlotFrame {
                seq: g.seq,
                width: g.width,
                height: g.height,
                pixels: g.pixels.clone(),
            })
        } else {
            None
        }
    }

    /// 状态机：先尝试给新帧，无新帧且 dead 才报 Dead——窗口关闭前抓到的
    /// 最后一帧仍会被消费掉（Java 侧先刷最后一帧纹理，再在下一 tick 收到
    /// DEAD_SENTINEL 做清理）。
    pub fn poll(&self, since_seq: u64) -> (FramePoll, Option<SlotFrame>) {
        let frame = self.take(since_seq);
        match frame {
            Some(f) => (FramePoll::Fresh, Some(f)),
            None if self.is_dead() => (FramePoll::Dead, None),
            None => (FramePoll::Idle, None),
        }
    }

    /// 会话彻底结束后清空像素、失效当前帧并复位 dead；
    /// seq 不回退（保持单调语义，新会话 publish 的 seq 更大）。
    pub fn clear(&self) {
        let mut g = lock(&self.inner);
        g.pixels = Vec::new();
        g.width = 0;
        g.height = 0;
        g.dead = false;
        g.valid = false;
    }
}

impl Default for FrameSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// 锁获取：中毒时取回守卫内容（见 `FrameSlot` 注释）。
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// CGImage 像素内存通道序（由 bpp + alphaInfo + byteOrder 判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOrder {
    /// 内存序 B,G,R,A——SCK SDR 输出的标准形态，即 ARGB8888（直通）
    Bgra,
    /// 内存序 B,G,R,X（32bpp 无 alpha 小端）——输出时 X→0xFF
    Bgrx,
    /// 内存序 R,G,B,A——输出时交换 R/B
    Rgba,
    /// 内存序 R,G,B（24bpp）——输出时补 A=0xFF
    Rgb,
}

// Apple <CoreGraphics/CGImage.h> 常量镜像（字面量保持本函数 cfg 无关）。
// FFI 区传入绑定常量的同值编码。
pub const ALPHA_NONE: u32 = 0;
pub const ALPHA_PREMULTIPLIED_LAST: u32 = 1;
pub const ALPHA_PREMULTIPLIED_FIRST: u32 = 2;
pub const ALPHA_LAST: u32 = 3;
pub const ALPHA_FIRST: u32 = 4;
pub const ALPHA_ONLY: u32 = 5;
pub const BITMAP_INFO_32_LITTLE: u32 = 2 << 5;

/// 由 CGImage 属性判定通道序；不支持的组合（灰度/16bit/Only-alpha 等）
/// 显式 Err——宁可失败，不猜格式。
pub fn classify_channel_order(
    bpp: u32,
    alpha: u32,
    bitmap_info: u32,
) -> Result<ChannelOrder, String> {
    match bpp {
        32 => match alpha {
            ALPHA_PREMULTIPLIED_FIRST | ALPHA_FIRST => Ok(ChannelOrder::Bgra),
            ALPHA_PREMULTIPLIED_LAST | ALPHA_LAST => Ok(ChannelOrder::Rgba),
            ALPHA_NONE => {
                if bitmap_info & BITMAP_INFO_32_LITTLE != 0 {
                    Ok(ChannelOrder::Bgrx)
                } else {
                    Err(format!("unsupported CGImage 32bpp alpha=none byteOrder={bitmap_info:#x}"))
                }
            }
            other => Err(format!("unsupported CGImage alphaInfo={other} for 32bpp")),
        },
        24 => {
            if alpha == ALPHA_NONE {
                Ok(ChannelOrder::Rgb)
            } else {
                Err(format!("unsupported CGImage alphaInfo={alpha} for 24bpp"))
            }
        }
        other => Err(format!("unsupported CGImage bitsPerPixel={other}")),
    }
}

/// 源通道序 → ARGB8888（内存字节序 B,G,R,A），同时按 bytes_per_row 去行
/// padding 压实。长度校验失败返回 Err（防越界）。
pub fn convert_to_argb(
    src: &[u8],
    width: usize,
    height: usize,
    bytes_per_row: usize,
    order: ChannelOrder,
) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("empty frame".to_string());
    }
    let per = match order {
        ChannelOrder::Rgb => 3,
        _ => 4,
    };
    let need = (height - 1)
        .checked_mul(bytes_per_row)
        .and_then(|v| v.checked_add(width * per))
        .ok_or_else(|| "frame size overflow".to_string())?;
    if bytes_per_row < width * per || src.len() < need {
        return Err(format!(
            "pixel buffer too small: len={} need>={}",
            src.len(),
            need
        ));
    }
    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        let s = &src[y * bytes_per_row..y * bytes_per_row + width * per];
        let d = &mut out[y * width * 4..(y + 1) * width * 4];
        match order {
            // BGRA → 直通（含行裁剪）
            ChannelOrder::Bgra => d.copy_from_slice(s),
            // BGRX → 每像素 X 字节替换为 0xFF
            ChannelOrder::Bgrx => {
                for (dst, px) in d.chunks_mut(4).zip(s.chunks_exact(4)) {
                    dst[0] = px[0];
                    dst[1] = px[1];
                    dst[2] = px[2];
                    dst[3] = 0xFF;
                }
            }
            // RGBA → BGRA（交换 0/2 通道）
            ChannelOrder::Rgba => {
                for (dst, px) in d.chunks_mut(4).zip(s.chunks_exact(4)) {
                    dst[0] = px[2];
                    dst[1] = px[1];
                    dst[2] = px[0];
                    dst[3] = px[3];
                }
            }
            // RGB → BGRA，补 A=0xFF
            ChannelOrder::Rgb => {
                for (dst, px) in d.chunks_mut(4).zip(s.chunks_exact(3)) {
                    dst[0] = px[2];
                    dst[1] = px[1];
                    dst[2] = px[0];
                    dst[3] = 0xFF;
                }
            }
        }
    }
    Ok(out)
}

/// 计算 SCStreamConfiguration 输出尺寸：长边超过 cap 时等比缩小。
/// 返回 (0,0) = 用原生像素分辨率（SCK 默认）。SCK 侧完成缩放（GPU 路径）。
pub fn compute_capture_size(nat_w: u32, nat_h: u32, cap: u32) -> (usize, usize) {
    if cap == 0 || nat_w == 0 || nat_h == 0 {
        return (0, 0);
    }
    let long = nat_w.max(nat_h);
    if long <= cap {
        return (nat_w as usize, nat_h as usize);
    }
    // 定点等比，短边至少 1
    let (w, h) = if nat_w >= nat_h {
        (cap, ((nat_h as u64 * cap as u64) / nat_w as u64).max(1) as u32)
    } else {
        (((nat_w as u64 * cap as u64) / nat_h as u64).max(1) as u32, cap)
    };
    (w as usize, h as usize)
}

/// 线协议哨兵：`mac_capture_frame` 返回单字节 `[DEAD_SENTINEL]` 表示会话已
/// 终止（目标关闭/连续失败），Java 应清理显示并调用 stop。
pub const DEAD_SENTINEL: u8 = 1;

/// `mac_capture_frame` 返回的 byte[]：
/// - 空 = 无新帧
/// - `[DEAD_SENTINEL]` = 会话已终止
/// - `[width u32 LE][height u32 LE][seq u64 LE][ARGB8888...]` = 新帧
pub fn frame_wire(poll: FramePoll, frame: Option<SlotFrame>) -> Vec<u8> {
    match (poll, frame) {
        (FramePoll::Idle, _) => Vec::new(),
        (FramePoll::Dead, _) => vec![DEAD_SENTINEL],
        (FramePoll::Fresh, Some(f)) => {
            let mut out = Vec::with_capacity(16 + f.pixels.len());
            out.extend_from_slice(&f.width.to_le_bytes());
            out.extend_from_slice(&f.height.to_le_bytes());
            out.extend_from_slice(&f.seq.to_le_bytes());
            out.extend_from_slice(&f.pixels);
            out
        }
        // poll=Fresh 必伴帧（成对设计），防御性按 Idle
        (FramePoll::Fresh, None) => Vec::new(),
    }
}

/// 窗口列表 → Java 侧行格式（对齐 `get_desktop_windows`：
/// `id|title|app|pid|hash|x|y|w|h`；macOS 无 wayland hash，用窗口 id 派生）。
pub fn window_lines(windows: &[WindowInfo]) -> Vec<String> {
    windows
        .iter()
        .map(|w| {
            format!(
                "{}|{}|{}|{}|mac{:x}|{}|{}|{}|{}",
                w.id,
                w.title.replace('|', "/"),
                w.owner.replace('|', "/"),
                w.pid,
                w.id,
                w.x,
                w.y,
                w.width,
                w.height
            )
        })
        .collect()
}

/// 显示器列表 → 行格式 `index|id|WxH|builtin|main|name`。
pub fn display_lines(displays: &[DisplayInfo]) -> Vec<String> {
    displays
        .iter()
        .map(|d| {
            let kind = if d.builtin { "built-in" } else { "external" };
            let name = if d.main {
                format!("{kind} (main)")
            } else {
                kind.to_string()
            };
            format!(
                "{}|{}|{}x{}|{}|{}|{}",
                d.index, d.id, d.width, d.height, d.builtin, d.main, name
            )
        })
        .collect()
}

// ═══════════════════════════════════════════════════════
// 对外统一 API（所有平台可调用；非 macOS 一律"不支持"）
// ═══════════════════════════════════════════════════════

pub const ERR_UNSUPPORTED: &str = "macos_capture_unsupported_platform";

/// 可捕获窗口列表（需屏幕录制权限；未授权返回 Err("permission_denied")）。
pub fn list() -> Result<Vec<WindowInfo>, String> {
    #[cfg(target_os = "macos")]
    {
        ffi::list_windows()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(ERR_UNSUPPORTED.to_string())
    }
}

/// 活动显示器列表。
pub fn list_displays() -> Result<Vec<DisplayInfo>, String> {
    #[cfg(target_os = "macos")]
    {
        ffi::list_displays()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(ERR_UNSUPPORTED.to_string())
    }
}

/// 捕获指定 CGWindowID 窗口。返回会话参考尺寸（点）(w,h)。
/// 已有会话时先 `stop()`（单会话，与 Java portal 管理器一致）。
pub fn start_window(window_id: u32) -> Result<(u32, u32), String> {
    #[cfg(target_os = "macos")]
    {
        ffi::start_window(window_id)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window_id;
        Err(ERR_UNSUPPORTED.to_string())
    }
}

/// 捕获第 index 个活动显示器（index 来自 `list_displays`）。
pub fn start_display(index: u32) -> Result<(u32, u32), String> {
    #[cfg(target_os = "macos")]
    {
        ffi::start_display(index)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = index;
        Err(ERR_UNSUPPORTED.to_string())
    }
}

/// 轮询帧字节（线协议见 `frame_wire`；无会话返回空数组）。
pub fn take_frame(since_seq: u64) -> Vec<u8> {
    #[cfg(target_os = "macos")]
    {
        let slot = ffi::current_slot();
        match slot {
            Some(slot) => {
                let (poll, frame) = slot.poll(since_seq);
                frame_wire(poll, frame)
            }
            None => Vec::new(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = since_seq;
        Vec::new()
    }
}

/// 停止会话（幂等，无会话返回 Ok）：置标志 → 唤醒 → join → 清帧槽。
pub fn stop() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        ffi::stop_session()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

/// 当前屏幕录制权限状态（非 macOS 恒 Unknown）。
pub fn permission_state() -> PermState {
    #[cfg(target_os = "macos")]
    {
        ffi::permission_probe(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermState::Unknown
    }
}

// ═══════════════════════════════════════════════════════
// macOS FFI 区
// ═══════════════════════════════════════════════════════
//
// 维护点（绑定升级时重点复核）：
// - objc2-screen-capture-kit 0.3.2 生成绑定：SCScreenshotManager 类方法
//   captureImageWithFilter:configuration:completionHandler:（macOS 14+）。
// - block 回调参数对象只在回调执行期间被 ARC 持有——像素一律在 block 内
//   拷出成 Vec<u8>，绝不跨线程/跨回调持有 CGImage/CFData。
// - ObjC 对象跨线程"转移"（非共享）经 SendRetained：ARC 引用计数原子，
//   Apple 文档允许 creation 之后跨线程使用；本模块只在两个线程间做一次性
//   move（JNI 线程构造 → 捕获线程独占使用 → 捕获线程 drop）。

#[cfg(target_os = "macos")]
mod ffi {
    use super::{
        ChannelOrder, DisplayInfo, FrameSlot, PermState, WindowInfo, classify_channel_order,
        compute_capture_size, convert_to_argb, filter_windows, perm_state,
    };

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
    use objc2::AnyThread;
    use objc2_core_foundation::{
        CFArray, CFArrayGetValueAtIndex, CFDictionary, CFDictionaryGetValue, CFNumber, CFRetained,
        CFString, CFType, Type,
    };
    use objc2_core_graphics as cg;
    use objc2_foundation::{NSArray, NSError};
    use objc2_screen_capture_kit as sck;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Instant;

    /// 一次性等待门（block 回调线程 signal、等待线程带超时 wait）。
    ///
    /// 不用 `std::sync::Semaphore`：rustc stable（含 1.100）尚未稳定该类型，
    /// 跨版本工具链会直接 E0432。Mutex+Condvar 等价实现，且"先 signal 后
    /// wait"不丢事件。
    struct WaitGate {
        done: Mutex<bool>,
        cvar: Condvar,
    }

    impl WaitGate {
        fn new() -> Self {
            WaitGate {
                done: Mutex::new(false),
                cvar: Condvar::new(),
            }
        }

        fn signal(&self) {
            let mut g = self.done.lock().unwrap_or_else(|e| e.into_inner());
            *g = true;
            self.cvar.notify_all();
        }

        /// true = 已 signal；false = 超时且未 signal。
        fn wait(&self, timeout: std::time::Duration) -> bool {
            let mut g = self.done.lock().unwrap_or_else(|e| e.into_inner());
            let deadline = Instant::now() + timeout;
            while !*g {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (next, wait_res) = self
                    .cvar
                    .wait_timeout(g, remaining)
                    .unwrap_or_else(|e| e.into_inner());
                g = next;
                if wait_res.timed_out() {
                    break;
                }
            }
            *g
        }
    }

    // ── 时序常量 ─────────────────────────────────────────────
    /// 抓帧轮询目标帧率（SCScreenshotManager 为同步排队抓帧，10-15fps 为
    /// 官方可接受区间；全速 30/60fps 需 SCStream 路径，见模块头选型说明）。
    const TARGET_FPS: u64 = 12;
    const POLL_INTERVAL: std::time::Duration =
        std::time::Duration::from_millis(1000 / TARGET_FPS);
    /// 单次抓帧回调等待上限（超时计一次失败）
    const CAPTURE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);
    /// SCShareableContent 异步枚举等待上限
    const SHARE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    /// 输出长边上限（Retina 4K/5K 原生帧 ~60MB/帧，12fps 下分配抖动不可接受）
    const MAX_CAPTURE_EDGE: u32 = 1920;
    /// 连续失败达到该数 → 标记 dead 并退出线程（典型：目标窗口被关）
    const MAX_CONSECUTIVE_FAILURES: u32 = 3;

    const ERR_PERMISSION: &str = "permission_denied";
    const ERR_VERSION: &str = "macos14_required";

    /// ObjC 对象跨线程一次性转移的封装。
    ///
    /// SAFETY 论证：
    /// 1. ObjC/ARC 引用计数（retain/release）在 Apple 运行时是原子的；
    /// 2. 被包裹对象在构造完成后只经历"调用线程 → 捕获线程"的一次 move，
    ///    任一时刻只有一个线程持有/访问（无共享可变）；
    /// 3. SCContentFilter 创建后不可变，SCK 内部自身跨队列传递这类对象。
    /// 绑定未来若收紧（如标记 MainThreadOnly）此 unsafe impl 需重审。
    pub struct SendRetained<T: objc2::Message>(pub Retained<T>);
    unsafe impl<T: objc2::Message> Send for SendRetained<T> {}

    struct Shared {
        slot: Arc<FrameSlot>,
        stop: AtomicBool,
        /// condvar 守卫（仅用于唤醒等待，不保护业务状态）
        lock: Mutex<()>,
        wake: Condvar,
    }

    struct Session {
        join: Option<std::thread::JoinHandle<()>>,
        shared: Arc<Shared>,
    }

    static SESSION: Mutex<Option<Session>> = Mutex::new(None);

    fn session_lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// take_frame 用的当前帧槽句柄（无会话 None）。
    pub fn current_slot() -> Option<Arc<FrameSlot>> {
        session_lock(&SESSION)
            .as_ref()
            .map(|s| s.shared.slot.clone())
    }

    // ── 权限 / 版本门 ────────────────────────────────────────

    /// `ask=true` 时经 CGRequestScreenCaptureAccess 引导系统弹窗。
    pub fn permission_probe(ask: bool) -> PermState {
        let preflight1 = unsafe { cg::CGPreflightScreenCaptureAccess() };
        if preflight1 {
            return PermState::Granted;
        }
        if !ask {
            // permission_state() 只读探测：不打扰用户，未授权即 Denied
            return PermState::Denied;
        }
        let requested = Some(unsafe { cg::CGRequestScreenCaptureAccess() });
        let preflight2 = unsafe { cg::CGPreflightScreenCaptureAccess() };
        perm_state(preflight1, requested, preflight2)
    }

    /// SCScreenshotManager（macOS 14+）类与抓帧 selector 存在性。
    fn screenshot_api_available() -> bool {
        let Some(class) = AnyClass::get(c"SCScreenshotManager") else {
            return false;
        };
        // 类存在性已足够（13 上该类根本不会被 dyld 装载）；selector 检查是
        // 对"未来类出现但方法缺席"的防御。
        let sel = objc2::sel!(captureImageWithFilter:configuration:completionHandler:);
        class.responds_to(sel)
    }

    fn ensure_ready(ask: bool) -> Result<(), String> {
        if permission_probe(ask) != PermState::Granted {
            return Err(ERR_PERMISSION.to_string());
        }
        if !screenshot_api_available() {
            return Err(ERR_VERSION.to_string());
        }
        Ok(())
    }

    // ── SCShareableContent 异步→同步桥 ──────────────────────

    /// 在调用线程同步获取 SCShareableContent（block + WaitGate）。
    /// 返回的 filter 由调用线程构造（ObjC 对象不跨线程"共享"，只在下方
    /// SendRetained 论证内一次性转移）。
    fn shareable_blocking() -> Result<Retained<sck::SCShareableContent>, String> {
        let gate = Arc::new(WaitGate::new());
        let out: Arc<Mutex<Option<Result<RetainedHolder, String>>>> =
            Arc::new(Mutex::new(None));
        {
            let gate = gate.clone();
            let out = out.clone();
            let block = RcBlock::new(move |content: *mut sck::SCShareableContent, err: *mut NSError| {
                // objc2 0.6：Retained::retain(*mut) -> Option（null → None）
                let result = match unsafe { Retained::retain(content) } {
                    Some(ret) => Ok(RetainedHolder(ret)),
                    None => {
                        let msg = match unsafe { Retained::retain(err) } {
                            Some(e) => e.localizedDescription().to_string(),
                            None => "unknown".to_string(),
                        };
                        Err(format!("shareable_content_failed: {msg}"))
                    }
                };
                if let Ok(mut g) = out.lock() {
                    if g.is_none() {
                        *g = Some(result);
                    }
                }
                gate.signal();
            });
            unsafe {
                sck::SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                    true,
                    true,
                    &block,
                );
            }
        }
        if !gate.wait(SHARE_TIMEOUT) {
            return Err("shareable_content_timeout".to_string());
        }
        match session_lock(&out).take() {
            Some(Ok(holder)) => Ok(holder.0),
            Some(Err(e)) => Err(e),
            None => Err("shareable_content_lost".to_string()),
        }
    }

    /// content 从 block 线程带回调用线程：同 SendRetained 的一次性转移论证
    /// （block 内 retain 出的所有权随闭包捕获的 Arc<Mutex> 交给等待线程，
    /// 交出后 block 不再触碰它）。
    struct RetainedHolder(Retained<sck::SCShareableContent>);
    unsafe impl Send for RetainedHolder {}

    // ── 窗口枚举（CGWindowList）──────────────────────────────

    /// 从 CFDictionary 取 CFType 值。生成绑定的 `CFDictionary::get` 要求
    /// `K: Type + Hash + PartialEq` 等 trait，而 kCGWindow* 常量是
    /// `&'static CFString`——直接走 C API 最稳（维护点：绑定升级后复核）。
    fn dict_value(dict: &CFDictionary, key: &CFString) -> Option<CFRetained<CFType>> {
        let key_ref: &CFType = key.as_CFTypeRef();
        let raw = unsafe { CFDictionaryGetValue(dict, key_ref as *const CFType as *const c_void) };
        let ptr = NonNull::new(raw.cast_mut().cast::<CFType>())?;
        Some(unsafe { CFRetained::retain(ptr) })
    }

    fn dict_i64(dict: &CFDictionary, key: &CFString) -> Option<i64> {
        let v = dict_value(dict, key)?;
        let num: CFRetained<CFNumber> = v.downcast().ok()?;
        num.as_i64()
    }

    fn dict_str(dict: &CFDictionary, key: &CFString) -> Option<String> {
        let v = dict_value(dict, key)?;
        let s: CFRetained<CFString> = v.downcast().ok()?;
        Some(s.to_string())
    }

    /// kCGWindowBounds 是嵌套 CFDictionary（键 "X"/"Y"/"Width"/"Height"）。
    fn dict_bounds(dict: &CFDictionary) -> Option<(i32, i32, i32, i32)> {
        static BOUNDS_KEYS: [&str; 4] = ["X", "Y", "Width", "Height"];
        let bounds_ref = dict_value(dict, unsafe { cg::kCGWindowBounds })?;
        let bounds: CFRetained<CFDictionary> = bounds_ref.downcast().ok()?;
        let mut vals = [0i32; 4];
        for (i, k) in BOUNDS_KEYS.iter().enumerate() {
            let key = CFString::new(k);
            let v = dict_value(&bounds, &key)?;
            let num: CFRetained<CFNumber> = v.downcast().ok()?;
            vals[i] = num.as_f64()?.round() as i32;
        }
        Some((vals[0], vals[1], vals[2], vals[3]))
    }

    /// 数组下标取元素并 retain（越界指针判空防御）。
    fn array_value(array: &CFArray, idx: usize) -> Option<CFRetained<CFType>> {
        let raw = unsafe { CFArrayGetValueAtIndex(array, idx as isize) };
        let ptr = NonNull::new(raw.cast_mut().cast::<CFType>())?;
        Some(unsafe { CFRetained::retain(ptr) })
    }

    pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
        // 无权限时 kCGWindowName 为空串——透出权限错误而不是返回一堆无名窗口
        if permission_probe(false) != PermState::Granted {
            return Err(ERR_PERMISSION.to_string());
        }
        let option =
            cg::CGWindowListOption::OptionOnScreenOnly | cg::CGWindowListOption::ExcludeDesktopElements;
        let array: CFRetained<CFArray> = cg::CGWindowListCopyWindowInfo(option, cg::kCGNullWindowID)
            .ok_or_else(|| "CGWindowListCopyWindowInfo returned null".to_string())?;
        let count = array.len();
        let mut raw = Vec::with_capacity(count);
        for i in 0..count {
            let Some(item) = array_value(&array, i) else { continue };
            let Ok(dict) = item.downcast::<CFDictionary>() else {
                continue;
            };
            let Some(id) = dict_i64(&dict, unsafe { cg::kCGWindowNumber }) else {
                continue;
            };
            let (x, y, w, h) = dict_bounds(&dict).unwrap_or((0, 0, 0, 0));
            raw.push(WindowInfo {
                id: id as u32,
                owner: dict_str(&dict, unsafe { cg::kCGWindowOwnerName }).unwrap_or_default(),
                title: dict_str(&dict, unsafe { cg::kCGWindowName }).unwrap_or_default(),
                pid: dict_i64(&dict, unsafe { cg::kCGWindowOwnerPID }).unwrap_or(-1) as i32,
                layer: dict_i64(&dict, unsafe { cg::kCGWindowLayer }).unwrap_or(-1),
                x,
                y,
                width: w,
                height: h,
            });
        }
        Ok(filter_windows(raw, std::process::id() as i32))
    }

    pub fn list_displays() -> Result<Vec<DisplayInfo>, String> {
        if permission_probe(false) != PermState::Granted {
            return Err(ERR_PERMISSION.to_string());
        }
        let mut ids = vec![0u32; 16];
        let mut count: u32 = 0;
        // 绑定形态：原始指针版（feature CGError 门后）
        let e = unsafe {
            cg::CGGetActiveDisplayList(
                ids.len() as u32,
                ids.as_mut_ptr(),
                &mut count as *mut u32,
            )
        };
        if e != cg::CGError::Success {
            return Err(format!("CGGetActiveDisplayList: {:?}", e));
        }
        ids.truncate(count as usize);
        let main = cg::CGMainDisplayID();
        Ok(ids
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let bounds = cg::CGDisplayBounds(id);
                DisplayInfo {
                    index: i as u32,
                    id,
                    width: bounds.size.width.max(0.0) as u32,
                    height: bounds.size.height.max(0.0) as u32,
                    builtin: unsafe { CGDisplayIsBuiltin(id) } != 0,
                    main: id == main,
                }
            })
            .collect())
    }

    // 维护点：objc2-core-graphics 0.3.2 未生成 CGDisplayIsBuiltin（在
    // <CoreGraphics/CGDirectDisplay.h> 中为普通函数，但生成清单缺失）。
    // 绑定升级后优先换回 crate 提供的版本。
    unsafe extern "C-unwind" {
        fn CGDisplayIsBuiltin(display: cg::CGDirectDisplayID) -> u32;
    }

    // ── 会话启动 ─────────────────────────────────────────────

    pub fn start_window(window_id: u32) -> Result<(u32, u32), String> {
        stop_session()?;
        ensure_ready(true)?;
        let content = shareable_blocking()?;
        // 找匹配 SCWindow 并当场建 filter（SCWindow 不离开本线程作用域）
        let windows = unsafe { content.windows() };
        let mut filter = None;
        let mut frame_pts = (0u32, 0u32);
        for win in windows.iter() {
            if unsafe { win.windowID() } != window_id {
                continue;
            }
            let f: Retained<sck::SCContentFilter> = unsafe {
                sck::SCContentFilter::initWithDesktopIndependentWindow(
                    sck::SCContentFilter::alloc(),
                    &win,
                )
            };
            let fr = unsafe { win.frame() };
            frame_pts = (
                fr.size.width.max(0.0) as u32,
                fr.size.height.max(0.0) as u32,
            );
            filter = Some(f);
            break;
        }
        let filter = filter.ok_or_else(|| format!("window_not_found:{window_id}"))?;
        start_session(SendRetained(filter), frame_pts, format!("window {window_id}"))
    }

    pub fn start_display(index: u32) -> Result<(u32, u32), String> {
        stop_session()?;
        ensure_ready(true)?;
        let displays = list_displays()?;
        let target = displays
            .into_iter()
            .find(|d| d.index == index)
            .ok_or_else(|| format!("display_index_invalid:{index}"))?;
        let content = shareable_blocking()?;
        let sck_displays = unsafe { content.displays() };
        let no_exclusions = NSArray::<sck::SCWindow>::new();
        let mut filter = None;
        for d in sck_displays.iter() {
            if unsafe { d.displayID() } != target.id {
                continue;
            }
            let f: Retained<sck::SCContentFilter> = unsafe {
                sck::SCContentFilter::initWithDisplay_excludingWindows(
                    sck::SCContentFilter::alloc(),
                    &d,
                    &no_exclusions,
                )
            };
            filter = Some(f);
            break;
        }
        let filter = filter.ok_or("display_not_found".to_string())?;
        start_session(
            SendRetained(filter),
            (target.width, target.height),
            format!("display {index}"),
        )
    }

    fn start_session(
        filter: SendRetained<sck::SCContentFilter>,
        ref_size: (u32, u32),
        label: String,
    ) -> Result<(u32, u32), String> {
        let shared = Arc::new(Shared {
            slot: Arc::new(FrameSlot::new()),
            stop: AtomicBool::new(false),
            lock: Mutex::new(()),
            wake: Condvar::new(),
        });
        let thread_shared = shared.clone();
        let thread_label = label.clone();
        let join = std::thread::Builder::new()
            .name("mac-capture".to_string())
            .spawn(move || capture_loop(filter, thread_shared, &thread_label))
            .map_err(|e| format!("spawn_failed: {e}"))?;
        let mut guard = session_lock(&SESSION);
        // start_* 已先 stop 过；这里仍是 None。若是 None 之外的值，说明
        // 并发 start——按单会话契约拒绝，不覆盖（防 join handle 泄漏）。
        if guard.is_some() {
            drop(guard);
            shared.stop.store(true, Ordering::Release);
            let _ = join.join();
            return Err("session_busy".to_string());
        }
        *guard = Some(Session {
            join: Some(join),
            shared,
        });
        eprintln!("[mac-capture] session started: {label} ref={}x{}", ref_size.0, ref_size.1);
        Ok(ref_size)
    }

    /// stop：置标志 → 唤醒 → join（有界：≤ POLL_INTERVAL + CAPTURE_ACK_TIMEOUT）
    /// → 清帧槽。ObjC 对象在捕获线程内 drop，确定性释放。
    pub fn stop_session() -> Result<(), String> {
        let session = session_lock(&SESSION).take();
        let Some(mut s) = session else {
            return Ok(());
        };
        s.shared.stop.store(true, Ordering::Release);
        {
            let _g = s.shared.lock.lock().unwrap_or_else(|e| e.into_inner());
            s.shared.wake.notify_all();
        }
        if let Some(h) = s.join.take() {
            // panic=abort 下 join 不会返回 Err；thread panics 则进程早已 abort。
            // 这里 join 本身不可能死锁（循环内所有等待都有超时）。
            let _ = h.join();
        }
        s.shared.slot.clear();
        eprintln!("[mac-capture] session stopped (last seq={})", s.shared.slot.seq());
        Ok(())
    }

    // ── 捕获线程 ─────────────────────────────────────────────

    enum CaptureOutcome {
        Ok(SlotFrameParts),
        Err(String),
    }

    struct SlotFrameParts {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    }

    /// block 内执行：CGImage → ARGB8888 Vec（对象只在本回调内引用）。
    /// CGImage 形参是 completion handler 的 __strong 入参，执行期间必活；
    /// CFData 同理——字节指针在本函数返回前有效。
    unsafe fn extract_pixels(
        img: *mut cg::CGImage,
        err: *mut NSError,
    ) -> CaptureOutcome {
        let Some(image_ptr) = NonNull::new(img) else {
            let msg = match NonNull::new(err) {
                Some(e) => unsafe { e.as_ref().localizedDescription().to_string() },
                None => "capture returned no image".to_string(),
            };
            return CaptureOutcome::Err(msg);
        };
        let image = unsafe { image_ptr.as_ref() };
        let w = cg::CGImageGetWidth(Some(image)) as u32;
        let h = cg::CGImageGetHeight(Some(image)) as u32;
        let bpr = cg::CGImageGetBytesPerRow(Some(image));
        let bpp = cg::CGImageGetBitsPerPixel(Some(image)) as u32;
        let alpha = cg::CGImageGetAlphaInfo(Some(image)).0;
        let bminfo = cg::CGImageGetBitmapInfo(Some(image)).0;
        let order: ChannelOrder = match classify_channel_order(bpp, alpha, bminfo) {
            Ok(o) => o,
            Err(e) => return CaptureOutcome::Err(e),
        };
        let Some(provider) = cg::CGImageGetDataProvider(Some(image)) else {
            return CaptureOutcome::Err("no data provider".to_string());
        };
        let Some(data) = cg::CGDataProviderCopyData(Some(&provider)) else {
            return CaptureOutcome::Err("CGDataProviderCopyData failed".to_string());
        };
        // SAFETY: `data` 由本 block 独占（刚拷贝出的独立 CFData），无并发修改
        let src: &[u8] = unsafe { data.as_bytes_unchecked() };
        match convert_to_argb(src, w as usize, h as usize, bpr, order) {
            Ok(pixels) => CaptureOutcome::Ok(SlotFrameParts {
                width: w,
                height: h,
                pixels,
            }),
            Err(e) => CaptureOutcome::Err(e),
        }
    }

    fn make_config() -> Result<Retained<sck::SCStreamConfiguration>, String> {
        // objc2 0.6.5：msg_send_id! 已弃用；msg_send! 原生处理
        // Allocated 接收者 + init 方法族 → Retained 转换。
        let cfg: Retained<sck::SCStreamConfiguration> = unsafe {
            let alloc = sck::SCStreamConfiguration::alloc();
            objc2::msg_send![alloc, init]
        };
        // SDR 显式固定：HDR 输出是半角浮点像素格式（RGBA16F），本模块通道
        // 判定只接受 8bit 形态。
        unsafe {
            cfg.setCaptureDynamicRange(sck::SCCaptureDynamicRange::SDR);
        }
        Ok(cfg)
    }

    fn capture_loop(
        filter: SendRetained<sck::SCContentFilter>,
        shared: Arc<Shared>,
        label: &str,
    ) {
        let config = match make_config() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[mac-capture] {label}: config init failed: {e}");
                shared.slot.mark_dead();
                return;
            }
        };
        let mut failures: u32 = 0;
        let mut size_set = false;

        while !shared.stop.load(Ordering::Acquire) {
            // 可被 stop 打断的轮询间隔
            {
                let guard = shared.lock.lock().unwrap_or_else(|e| e.into_inner());
                if !shared.stop.load(Ordering::Acquire) {
                    let _ = shared.wake.wait_timeout(guard, POLL_INTERVAL);
                }
            }
            if shared.stop.load(Ordering::Acquire) {
                break;
            }

            let gate = Arc::new(WaitGate::new());
            let out: Arc<Mutex<Option<CaptureOutcome>>> = Arc::new(Mutex::new(None));
            {
                let gate = gate.clone();
                let out = out.clone();
                let block = RcBlock::new(move |img: *mut cg::CGImage, err: *mut NSError| {
                    let outcome = unsafe { extract_pixels(img, err) };
                    if let Ok(mut g) = out.lock() {
                        if g.is_none() {
                            *g = Some(outcome);
                        }
                    }
                    gate.signal();
                });
                unsafe {
                    sck::SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
                        &filter.0,
                        &config,
                        Some(&block),
                    );
                }
                // block 在本作用域末 drop；SCK 已在其内部队列持有引用
                //（add_ref on call）——回调超时路径下 block 可能稍后才被
                // 释放，但捕获的仅是 Arc，不触碰 frame slot 以外的状态。
            }

            if gate.wait(CAPTURE_ACK_TIMEOUT) {
                let taken = out
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                match taken {
                    Some(CaptureOutcome::Ok(parts)) => {
                        if !size_set {
                            // 首帧得知真实像素尺寸 → 设 cap 降采样
                            let (cw, ch) =
                                compute_capture_size(parts.width, parts.height, MAX_CAPTURE_EDGE);
                            if cw != parts.width as usize || ch != parts.height as usize {
                                unsafe {
                                    config.setWidth(cw);
                                    config.setHeight(ch);
                                }
                            }
                            size_set = true;
                        }
                        shared
                            .slot
                            .publish(parts.width, parts.height, parts.pixels);
                        failures = 0;
                    }
                    Some(CaptureOutcome::Err(e)) => {
                        failures += 1;
                        eprintln!("[mac-capture] {label}: frame error ({failures}): {e}");
                    }
                    None => {
                        failures += 1;
                        eprintln!("[mac-capture] {label}: callback lost ({failures})");
                    }
                }
            } else {
                failures += 1;
                eprintln!("[mac-capture] {label}: capture timeout ({failures})");
            }

            if failures >= MAX_CONSECUTIVE_FAILURES {
                eprintln!("[mac-capture] {label}: too many consecutive failures, ending");
                shared.slot.mark_dead();
                break;
            }
        }

        // 线程退出：config/filter（Retained）在此 drop——确定性释放。
        eprintln!("[mac-capture] {label}: capture thread exiting");
    }
}

// ═══════════════════════════════════════════════════════
// 单测（cfg 无关区；Linux cargo test --lib 执行）
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn win(id: u32, layer: i64, pid: i32, w: i32, h: i32) -> WindowInfo {
        WindowInfo {
            id,
            owner: "owner".to_string(),
            title: format!("win {id}"),
            pid,
            layer,
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    #[test]
    fn filter_windows_keeps_only_normal_app_windows() {
        let raw = vec![
            win(1, 0, 100, 800, 600),   // ok
            win(2, 1, 100, 100, 100),   // layer != 0 → 滤
            win(3, 0, 999, 0, 600),     // 宽 0 → 滤
            win(4, 0, 999, -5, 600),    // 负宽 → 滤
            win(5, 0, 42, 300, 200),    // 自身 pid → 滤
            win(1, 0, 100, 800, 600),   // 重复 id → 滤（保首个）
        ];
        let got = filter_windows(raw, 42);
        assert_eq!(got.iter().map(|w| w.id).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn perm_state_matrix() {
        // preflight 首探即通过 → Granted（不再 request）
        assert_eq!(perm_state(true, None, false), PermState::Granted);
        // 未授权、request 不可用 → Denied
        assert_eq!(perm_state(false, None, false), PermState::Denied);
        // request 后二次 preflight 通过 → Granted（request 返回值不影响）
        assert_eq!(perm_state(false, Some(false), true), PermState::Granted);
        // request 返回 true 但二次 preflight 仍 false → Denied（不信 request 假成功）
        assert_eq!(perm_state(false, Some(true), false), PermState::Denied);
        // 全 false → Denied
        assert_eq!(perm_state(false, Some(false), false), PermState::Denied);
    }

    #[test]
    fn frame_slot_seq_state_machine() {
        let slot = FrameSlot::new();
        // 初始：无帧、未 dead
        assert_eq!(slot.seq(), 0);
        let (poll, f) = slot.poll(0);
        assert_eq!(poll, FramePoll::Idle);
        assert!(f.is_none());

        let s1 = slot.publish(100, 50, vec![9u8; 400]);
        let s2 = slot.publish(100, 50, vec![8u8; 400]);
        assert!(s2 > s1);

        // since < 当前 seq → 新帧（拿到的是最新内容）
        let (poll, f) = slot.poll(1);
        assert_eq!(poll, FramePoll::Fresh);
        let f = f.unwrap();
        assert_eq!(f.seq, s2);
        assert_eq!(f.pixels[0], 8);

        // since == 当前 seq → Idle
        let (poll, _) = slot.poll(s2);
        assert_eq!(poll, FramePoll::Idle);

        // dead：最后一帧仍可消费，消费完再报 Dead
        slot.mark_dead();
        let (poll, f) = slot.poll(1);
        assert_eq!(poll, FramePoll::Fresh);
        assert_eq!(f.unwrap().seq, s2);
        let (poll, _) = slot.poll(s2);
        assert_eq!(poll, FramePoll::Dead);

        // clear：像素回收、dead 复位，但 seq 单调不回退
        slot.clear();
        assert_eq!(slot.seq(), s2);
        assert!(!slot.is_dead());
        assert!(slot.take(s1).is_none());
        assert!(slot.take(0).is_none()); // clear 后 seq=2 也不得交出已释放帧
        let s3 = slot.publish(200, 20, vec![7u8; 160]);
        assert!(s3 > s2);
    }

    fn px_rgba_order(b: u8, g: u8, r: u8, a: u8) -> [u8; 4] {
        [b, g, r, a]
    }

    #[test]
    fn classify_channel_order_cases() {
        // SCK SDR 实际形态：32bpp + PremultipliedFirst + 32Little → Bgra
        assert_eq!(
            classify_channel_order(32, ALPHA_PREMULTIPLIED_FIRST, BITMAP_INFO_32_LITTLE),
            Ok(ChannelOrder::Bgra)
        );
        assert_eq!(
            classify_channel_order(32, ALPHA_PREMULTIPLIED_LAST, 0),
            Ok(ChannelOrder::Rgba)
        );
        assert_eq!(
            classify_channel_order(32, ALPHA_NONE, BITMAP_INFO_32_LITTLE),
            Ok(ChannelOrder::Bgrx)
        );
        assert_eq!(
            classify_channel_order(24, ALPHA_NONE, 0),
            Ok(ChannelOrder::Rgb)
        );
        // 显式拒绝：Only alpha / 16bpp / 32 无 alpha 大端
        assert!(classify_channel_order(32, ALPHA_ONLY, 0).is_err());
        assert!(classify_channel_order(16, ALPHA_NONE, 0).is_err());
        assert!(classify_channel_order(32, ALPHA_NONE, 0).is_err());
    }

    #[test]
    fn convert_bgra_passthrough_trims_padding() {
        // 2x1 像素，bpr=12（含 4 字节 padding）
        let mut src = Vec::new();
        src.extend_from_slice(&px_rgba_order(1, 2, 3, 4));
        src.extend_from_slice(&px_rgba_order(5, 6, 7, 8));
        src.extend_from_slice(&[0xAA; 4]); // padding
        let out = convert_to_argb(&src, 2, 1, 12, ChannelOrder::Bgra).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn convert_rgba_swaps_channels() {
        let src = vec![10, 20, 30, 40, 50, 60, 70, 80]; // R,G,B,A ×2
        let out = convert_to_argb(&src, 2, 1, 8, ChannelOrder::Rgba).unwrap();
        assert_eq!(out, vec![30, 20, 10, 40, 70, 60, 50, 80]);
    }

    #[test]
    fn convert_bgrx_fills_opaque_alpha() {
        let src = vec![1, 2, 3, 0, 4, 5, 6, 0];
        let out = convert_to_argb(&src, 2, 1, 8, ChannelOrder::Bgrx).unwrap();
        assert_eq!(out, vec![1, 2, 3, 0xFF, 4, 5, 6, 0xFF]);
    }

    #[test]
    fn convert_rgb24_expands_with_alpha() {
        let src = vec![1, 2, 3, 4, 5, 6];
        let out = convert_to_argb(&src, 2, 1, 6, ChannelOrder::Rgb).unwrap();
        assert_eq!(out, vec![3, 2, 1, 0xFF, 6, 5, 4, 0xFF]);
    }

    #[test]
    fn convert_rejects_short_buffer() {
        let src = vec![0u8; 3];
        assert!(convert_to_argb(&src, 2, 1, 8, ChannelOrder::Bgra).is_err());
        assert!(convert_to_argb(&[], 0, 0, 0, ChannelOrder::Bgra).is_err());
    }

    #[test]
    fn compute_capture_size_rules() {
        // 不超限 → 原样
        assert_eq!(compute_capture_size(1920, 1080, 1920), (1920, 1080));
        // cap=0 / 未知 → (0,0) 原生
        assert_eq!(compute_capture_size(3840, 2160, 0), (0, 0));
        assert_eq!(compute_capture_size(0, 0, 1920), (0, 0));
        // 超限等比（Retina 4K → 1920x1080）
        assert_eq!(compute_capture_size(3840, 2160, 1920), (1920, 1080));
        // 竖屏窗口：高是长边
        assert_eq!(compute_capture_size(1000, 4000, 1000), (250, 1000));
        // 极窄：短边至少 1
        assert_eq!(compute_capture_size(1, 10000, 100), (1, 100));
    }

    #[test]
    fn frame_wire_encoding() {
        let f = SlotFrame {
            seq: 7,
            width: 3,
            height: 2,
            pixels: vec![0u8; 24],
        };
        let w = frame_wire(FramePoll::Fresh, Some(f.clone()));
        assert_eq!(w.len(), 16 + 24);
        assert_eq!(&w[0..4], &3u32.to_le_bytes());
        assert_eq!(&w[4..8], &2u32.to_le_bytes());
        assert_eq!(&w[8..16], &7u64.to_le_bytes());
        assert_eq!(frame_wire(FramePoll::Idle, None), Vec::<u8>::new());
        assert_eq!(frame_wire(FramePoll::Dead, None), vec![DEAD_SENTINEL]);
        assert_eq!(frame_wire(FramePoll::Fresh, None), Vec::<u8>::new());
    }

    #[test]
    fn window_lines_escape_pipes_and_match_shape() {
        let mut w = win(9, 0, 5, 100, 50);
        w.title = "a|b".to_string();
        w.owner = "c|d".to_string();
        let lines = window_lines(&[w]);
        assert_eq!(lines.len(), 1);
        let fields: Vec<&str> = lines[0].split('|').collect();
        assert_eq!(fields.len(), 9);
        assert_eq!(fields[0], "9");
        assert_eq!(fields[1], "a/b");
        assert_eq!(fields[2], "c/d");
        assert_eq!(fields[4], "mac9");
        assert_eq!(fields[7], "100");
        assert_eq!(fields[8], "50");
    }

    #[test]
    fn display_lines_shape() {
        let d = DisplayInfo {
            index: 1,
            id: 0x1234,
            width: 2560,
            height: 1440,
            builtin: false,
            main: true,
        };
        let lines = display_lines(&[d]);
        let fields: Vec<&str> = lines[0].split('|').collect();
        assert_eq!(fields.len(), 6);
        assert_eq!(fields[0], "1");
        assert_eq!(fields[2], "2560x1440");
        assert!(fields[5].contains("main"));
    }

    #[test]
    fn slot_publish_race_keeps_monotonic_seq() {
        // 模拟捕获线程多写者语义：seq 严格递增、take 幂等
        let slot = std::sync::Arc::new(FrameSlot::new());
        let mut handles = Vec::new();
        for i in 0..4 {
            let slot = slot.clone();
            handles.push(std::thread::spawn(move || {
                slot.publish(10 + i, 10, vec![i as u8; 400 * (10 + i) as usize]);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(slot.seq(), 4);
        assert!(slot.take(3).is_some());
        assert!(slot.take(4).is_none());
    }
}
