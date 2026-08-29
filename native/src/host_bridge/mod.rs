//! 宿主 IME 桥接（C 方案 Layer 3：mod ↔ 宿主 dbus-ibus/dbus-fcitx5）。
//!
//! ## 架构
//!
//! mod 在嵌套合成器里**对外是完整桌面 IME 服务**（XIM / im2 / im1 三个协议
//! 适配器），对内**通过 dbus 客户端连宿主 IME daemon**（ibus / fcitx5）。
//! host_bridge 就是这个 dbus 客户端的抽象。
//!
//! ```
//! [Layer 1: XIM / im2 / im1 适配器]  ─↓  DownEvent (Key/Surrounding/...)─
//!                                       ↓
//!                                ImeEvent 流（ime/ime_event.rs）
//!                                       ↓
//!                                HostBridge（host_bridge/mod.rs）
//!                                       ↓
//!                          ┌────────────┼────────────┐
//!                          ↓            ↓            ↓
//!                  [dbus_ibus]   [dbus_fcitx5]   (未来更多)
//!                          ↓            ↓
//!                       ibus         fcitx5
//!                          ↓            ↓
//!                       CommitText / UpdatePreeditText / HidePreeditText / etc
//!                          ↓            ↓
//!                          ←─ UpEvent (Commit/Preedit/LookupTable/Done) ─
//! ```
//!
//! ## 关键设计：commit 驱动模式
//!
//! **不依赖 reply 决定按键命运**——这是 v0.9.39 hybrid async 失败的核心原因：
//! 100% 超时（IBUS_ENABLE_SYNC_MODE 默认 0），reply 永不回，sync 路径无效。
//!
//! 改用**commit 驱动**：
//! 1. 应用按 `DownEvent::Key` → host_bridge **立即**通过 ProcessKeyEvent 发给
//!    宿主 daemon（fire-and-forget，不等 reply）
//! 2. 应用**同时**继续处理按键（不被吞）——因为 reply 不可靠
//! 3. 宿主 daemon 在内部处理拼音→preedit，异步发回信号
//! 4. host_bridge 收到信号 → 翻译为 `UpEvent` → 应用通过 ti3 preedit/commit
//!    看到 preedit 和 commit 文本
//!
//! **应用字母键 + 数字键都直通 firefox 文本框**——firefox 的 GdkIMContext
//! 已经独立连了宿主 ibus（你机器实测确认），所以 firefox 文本框会同时显示：
//!   - 字母（用户按的）
//!   - preedit 汉字候选（ibus kimpanel 画的）
//!   - 选字后的 commit 汉字（commit 推到 firefox 文本框）
//!
//! **这正是 v0.9.38 看到的"firefox 文本框隐约出现汉字"——但之前是 race 引起，
//! 现在是 firefox GdkIMContext + mod ti3 两条独立路径共存**。
//!
//! ## 三个状态
//!
//! - **InProcess**：游戏内有 im2 客户端（嵌套应用通过自己的 GdkIMContext /
//!   im2 grab 直接连宿主 daemon）—— mod 不参与，host_bridge 是 no-op
//! - **OutOfProcess**：嵌套应用走 XIM 协议（xterm 等纯 X11 应用）—— mod 当
//!   XIM server，接 XIM 消息 → 转 DownEvent 给 host_bridge
//! - **None**：无 IME 端点激活
//!
//! 当前**只实现 OutOfProcess + 协议适配**（Layer 1 + Layer 3 联动），
//! XIM server 本身在独立模块 `ime/xim_server.rs`（未来实现）。

use crate::ime::{
    Commit, CursorRect, DeleteSurrounding, DownEvent, KeyEvent, LookupTable, PreeditUpdate,
    SurroundingText, UpEvent,
};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

/// 宿主 IME 后端能力探测结果。
pub enum BridgeInit {
    /// 初始化成功，后端就绪。
    Ready(Box<dyn HostBridge>),
    /// 暂时性失败（bus 不存在等），稍后可重试。
    Transient(String),
    /// 结构性不支持（无 dbus daemon、协议不匹配等）。
    Unsupported(String),
}

/// 宿主 IME 后端 trait（所有 dbus 客户端共享）。
///
/// **实现原则**：本 trait 是**纯 ImeEvent 接口**——不暴露 zbus、dbus、
/// ibus、fcitx5 等任何后端细节。**未来要加 scim/uim 等后端时，只需
/// 写一个新后端实现本 trait，业务逻辑零修改**。
pub trait HostBridge: Send {
    /// 后端名（诊断日志用）。
    fn name(&self) -> &'static str;

    /// 是否就绪（异步初始化完成后才 true）。
    fn is_ready(&self) -> bool;

    /// 后端是否已死（连接断开，需重建）。
    fn is_dead(&self) -> bool;

    /// 下行事件：mod → 宿主 IME daemon。
    ///
    /// 调用方**不应假设**事件被同步处理——后端可以立即返回（非阻塞），
    /// 由后台 worker 线程异步发送到 dbus。
    fn submit(&mut self, ev: DownEvent);

    /// 上行事件出站：从宿主 daemon 接收的 commit/preedit/delete/lookup。
    /// 调用方**必须**每帧调用一次以 drain 队列。
    fn take_up_events(&mut self) -> Vec<UpEvent>;

    /// 反向同步光标矩形（候选窗锚点）。
    /// 仅在 im2 grab 不在场时（即 OutOfProcess XIM 路径）需要。
    fn update_cursor_rect(&mut self, rect: CursorRect);
}

/// 主线程侧句柄（持有 worker 通信通道 + 上行事件缓冲）。
pub struct HostBridgeHandle {
    backend: Box<dyn HostBridge>,
    /// 上行事件缓冲（每帧由 lib.rs drain 取走）。
    pending: Vec<UpEvent>,
}

impl HostBridgeHandle {
    /// 包装一个已就绪的后端。
    pub fn new(backend: Box<dyn HostBridge>) -> Self {
        Self {
            backend,
            pending: Vec::new(),
        }
    }

    /// 后端名（诊断用）。
    pub fn name(&self) -> &str {
        self.backend.name()
    }

    /// 是否就绪。
    pub fn is_ready(&self) -> bool {
        self.backend.is_ready()
    }

    /// 提交下行事件。
    pub fn submit(&mut self, ev: DownEvent) {
        self.backend.submit(ev);
    }

    /// 取走所有上行事件（每帧由 lib.rs 调用）。
    pub fn take_up_events(&mut self) -> Vec<UpEvent> {
        // 后端先 drain 自己的内部队列
        let mut events = self.backend.take_up_events();
        // 与本地缓冲合并
        events.append(&mut std::mem::take(&mut self.pending));
        events
    }

    /// 反向同步光标矩形。
    pub fn update_cursor_rect(&mut self, rect: CursorRect) {
        self.backend.update_cursor_rect(rect);
    }

    /// 后端是否已死。
    pub fn is_dead(&self) -> bool {
        self.backend.is_dead()
    }

    /// 取走所有上行事件并按 `Done` 边界分组。
    ///
    /// **关键**：UpEvent 是流式事件，必须按 Done 边界原子应用——
    /// 一次 Done 内所有 preedit/commit/delete 必须被应用层一起处理，
    /// 否则会出现"commit 之前没清 preedit"等竞态。
    pub fn take_up_events_batched(&mut self) -> Vec<Vec<UpEvent>> {
        let events = self.take_up_events();
        if events.is_empty() {
            return Vec::new();
        }
        let mut batches = Vec::new();
        let mut current = Vec::new();
        for ev in events {
            current.push(ev);
            if matches!(current.last(), Some(UpEvent::Done(_))) {
                batches.push(std::mem::take(&mut current));
            }
        }
        // 收尾：如果最后一批没 Done，仍返回（不丢事件）
        if !current.is_empty() {
            batches.push(current);
        }
        batches
    }
}

/// 按探测顺序尝试所有宿主 IME 后端。
///
/// 失败原因都写 IME 日志；全部失败时汇总成探测报告。
pub fn probe() -> BridgeInit {
    // 1. dbus-ibus（最常见，GNOME 默认）
    match dbus_ibus::DbusIbusBridge::connect() {
        BridgeInit::Ready(b) => {
            ime_log!("[waylandcraft][host_bridge] backend selected: dbus-ibus");
            return BridgeInit::Ready(b);
        }
        BridgeInit::Transient(msg) => {
            ime_log!("[waylandcraft][host_bridge] dbus-ibus: TRANSIENT {msg}");
        }
        BridgeInit::Unsupported(msg) => {
            ime_log!("[waylandcraft][host_bridge] dbus-ibus: {msg}");
        }
    }

    // 2. dbus-fcitx5（KDE / fcitx5 用户）
    match dbus_fcitx5::DbusFcitx5Bridge::connect() {
        BridgeInit::Ready(b) => {
            ime_log!("[waylandcraft][host_bridge] backend selected: dbus-fcitx5");
            return BridgeInit::Ready(b);
        }
        BridgeInit::Transient(msg) => {
            ime_log!("[waylandcraft][host_bridge] dbus-fcitx5: TRANSIENT {msg}");
        }
        BridgeInit::Unsupported(msg) => {
            ime_log!("[waylandcraft][host_bridge] dbus-fcitx5: {msg}");
        }
    }

    BridgeInit::Unsupported("no host IME backend available (need ibus or fcitx5)".into())
}

/// 协议无关的日志宏（与 system_ime 旧路径同名；写 stderr + ime.log）。
macro_rules! ime_log {
    ($($arg:tt)*) => {
        crate::bridge::ime_log_write(&format!($($arg)*))
    };
}

pub(crate) use ime_log;

pub mod dbus_ibus;
pub mod dbus_fcitx5;

#[cfg(test)]
mod tests;
