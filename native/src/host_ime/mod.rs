//! 宿主桌面输入法后端抽象层。
//!
//! ## 为什么需要这一层
//!
//! 「把宿主输入法接进游戏内嵌合成器」在不同窗口系统下有不同的标准协议栈：
//!
//! | 后端 | 协议 | 适用环境 | 状态 |
//! |---|---|---|---|
//! | `wayland_ti3` | zwp_text_input_v3 客户端 | 游戏本体跑原生 Wayland（Mutter/KWin/wlroots 桥接桌面 IME） | v0.9.27 起 |
//! | `dbus_ibus` | org.freedesktop.IBus DBus API | **与窗口系统无关**，ibus 在跑即可（GNOME 默认） | 本版新增 |
//! | `dbus_fcitx5` | fcitx5 DBus 前端 | fcitx5 用户 | 规划中 |
//! | `x11_xim` | XIM | 传统 X11 会话 | 规划中 |
//!
//! 没有任何单一协议能覆盖所有环境 —— 正确形态是**统一中继内核 +
//! 可插拔后端**：所有后端产出同一套 [`crate::system_ime::HostEvent`]
//! （Enter/Leave/Commit/Preedit/Delete/Done），消费同一套
//! [`crate::ime::ImeCommand`] 出站命令。中继 Relay 与游戏内 ti3/im2
//! wire 层完全不感知后端差异。
//!
//! ## 探测顺序（[`probe`]）
//!
//! ```text
//! wayland-ti3（需要 GLFW wl_display）
//!   └─ 不可用 → dbus-ibus（需要 session bus 上有 org.freedesktop.IBus）
//!        └─ 不可用 → dbus-fcitx5 → x11-xim → Unsupported（附完整探测报告）
//! ```
//!
//! 探测失败原因全部如实写入 IME 日志 —— 能力边界必须可诊断。
//!
//! ## 键盘路由约定（零阻塞）
//!
//! 需要原始按键的后端（如 dbus-ibus 的 ProcessKeyEvent 往返）通过
//! [`HostImBackend::submit_key`] 接管按键：调用方**立即吞下**该键，
//! 后端在内部完成异步往返后在 [`Self::poll`] 里裁决 —— 消费则丢弃，
//! 放行则进入 [`Self::take_forwarded_keys`]，由驱动层按原顺序补投递给
//! 焦点应用。代价是按键最多晚一帧到达应用（≤16ms），换来渲染线程
//! 零阻塞、零竞态。

use crate::ime::ImeCommand;
use crate::seat::KeyboardAction;
use crate::system_ime::HostEvent;

/// 与 system_ime.rs 同款日志宏（写 stderr + waylandcraft-ime.log）。
/// 本模块独立复制一份以避免跨模块 macro 作用域耦合。
macro_rules! ime_log {
    ($($arg:tt)*) => {
        crate::bridge::ime_log_write(&format!($($arg)*))
    };
}

/// 一条被后端接管的原始按键（xkb keycode = evdev + 8，与 Java scancode 一致）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubmittedKey {
    pub seq: u64,
    /// xkb keycode（evdev + 8）。
    pub key: u32,
    /// 已按当前修饰态/layout 解析出的 keysym（ibus keyval 语义）。
    pub keysym: u32,
    /// evdev keycode（ibus keycode 语义）。
    pub evdev: u32,
    /// ibus state 位掩码（bit30 = release）。
    pub state: u32,
    pub action: KeyboardAction,
    pub mods: (u32, u32, u32, u32),
}

/// ibus ProcessKeyEvent 的 state 参数：release 事件掩码（IBUS_RELEASE_MASK）。
pub(crate) const IBUS_RELEASE_MASK: u32 = 1 << 30;

/// 后端裁决为「放行」的按键：驱动层按原顺序补投递给焦点应用。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForwardedKey {
    pub key: u32,
    pub action: KeyboardAction,
}

/// 候选窗用户操作（Java 候选窗点击/翻页 → 宿主输入法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateNav {
    /// 按【当前页内】下标选字（fcitx5 `SelectCandidate` 语义，跳过 placeholder）。
    SelectCandidate(u32),
    PrevPage,
    NextPage,
}

pub(crate) trait HostImBackend {
    /// 后端名（诊断日志用）。
    fn name(&self) -> &'static str;

    /// 初始化是否已完成（dbus 类后端异步就绪；ti3 同步初始化恒 true）。
    /// 驱动层每帧据此刷新端点就绪状态。
    fn is_ready(&self) -> bool;

    /// 游戏内是否有激活文本会话（enable 门控）。
    fn set_active(&mut self, active: bool);

    /// 执行来自 Relay 的抽象命令（缓存语义，wire 写入在 poll 内调和）。
    fn execute_commands(&mut self, commands: Vec<ImeCommand>);

    /// 候选窗导航（Java UI 触发；默认忽略，dbus-fcitx5 转发专用方法）。
    fn candidate_nav(&mut self, _nav: CandidateNav) {}

    /// Minecraft 窗口重新获得 OS 键盘焦点（事件驱动焦点重协商钩子）。
    fn notify_host_focus_gained(&mut self) {}

    /// 每帧驱动：收事件、调和状态、裁决按键。
    fn poll(&mut self);

    /// 取走保序的宿主事件（灌入 Relay）。
    fn take_events(&mut self) -> Vec<HostEvent>;

    /// 取走已裁决放行的按键（按提交顺序）。
    fn take_forwarded_keys(&mut self) -> Vec<ForwardedKey> {
        Vec::new()
    }

    /// 提交一条原始按键；返回 true 表示已接管（调用方不得立即投递）。
    /// 默认不接管（wayland-ti3 由宿主合成器自行处理按键）。
    fn submit_key(&mut self, _key: SubmittedKey) -> bool {
        false
    }

    /// 连接是否已失效（驱动层据此丢弃实例并按 TRANSIENT 重试）。
    fn is_dead(&self) -> bool;

    /// 反向同步光标矩形（候选窗定位）。默认无操作。
    fn update_cursor_rect(&mut self, _rect: (i32, i32, i32, i32)) {}
}

pub(crate) mod dbus_ibus;
pub(crate) mod dbus_fcitx5;

use crate::system_ime::ImeInit;

/// 按探测顺序尝试所有宿主后端，返回第一个就绪者。
///
/// 每一步的失败原因都写 IME 日志；全部失败时汇总成一份探测报告，
/// 让「为什么没有输入法」永远有答案。
pub(crate) fn probe(wl_display_ptr: usize) -> ImeInit {
    let mut report: Vec<String> = Vec::new();

    // ── 1. wayland-ti3：唯一能吃到宿主合成器原生焦点路由的路径 ──
    if wl_display_ptr != 0 {
        match crate::system_ime::SystemIme::connect(wl_display_ptr) {
            ImeInit::Ready(si) => {
                ime_log!("[waylandcraft][host_ime] backend selected: wayland-ti3");
                return ImeInit::Ready(si);
            }
            ImeInit::Transient(msg) => report.push(format!("wayland-ti3: TRANSIENT {msg}")),
            ImeInit::Unsupported(msg) => report.push(format!("wayland-ti3: {msg}")),
        }
    } else {
        report.push(
            "wayland-ti3: 跳过（Minecraft 非 native Wayland 后端，无 GLFW wl_display）"
                .to_string(),
        );
    }

    // ── 2. dbus-ibus：与窗口系统无关 ──
    match dbus_ibus::DbusIbusBackend::connect() {
        ImeInit::Ready(b) => {
            ime_log!("[waylandcraft][host_ime] backend selected: dbus-ibus");
            return ImeInit::Ready(b);
        }
        ImeInit::Transient(msg) => report.push(format!("dbus-ibus: TRANSIENT {msg}")),
        ImeInit::Unsupported(msg) => report.push(format!("dbus-ibus: {msg}")),
    }

    // ── 3. dbus-fcitx5：fcitx5 的 dbus frontend（portal 名）──
    match dbus_fcitx5::DbusFcitx5Backend::connect() {
        ImeInit::Ready(b) => {
            ime_log!("[waylandcraft][host_ime] backend selected: dbus-fcitx5");
            return ImeInit::Ready(b);
        }
        ImeInit::Transient(msg) => report.push(format!("dbus-fcitx5: TRANSIENT {msg}")),
        ImeInit::Unsupported(msg) => report.push(format!("dbus-fcitx5: {msg}")),
    }

    // ── 4. x11-xim（P4，规划中）──
    report.push("x11-xim: 未实现（规划中）".to_string());

    let summary = report.join("; ");
    ime_log!("[waylandcraft][host_ime] 全部后端不可用: {summary}");
    ImeInit::Unsupported(summary)
}
