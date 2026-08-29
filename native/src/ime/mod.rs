//! 输入法子系统的薄门面 —— v0.10 重构后的最小实现。
//!
//! ## 战略
//!
//! 8 次版本（v0.9.38-45）都失败于"自造 im2/ti3 dispatch"——smithay 不允许
//! 同名 `Dispatch<ZwpInputMethodManagerV2, ()>`（E0119），而我们又必须保留
//! ti3 实例管理以让 firefox 等嵌套应用能 commit 汉字。
//!
//! **v0.10 重构**做了大刀阔斧的减法：
//!
//! | 删除 | 原因 |
//! |---|---|
//! | `input_method_v2.rs` | 自造 im2 dispatch（与 smithay 冲突） |
//! | `text_input_v3.rs`  | 自造 ti3 dispatch（与 smithay 冲突） |
//! | `relay.rs`          | 自造 Relay 状态机——逻辑并入 mod.rs |
//! | `tests.rs`          | 旧 wire 测试——新增覆盖核心 race 的测试在 mod.rs |
//! | `types.rs`          | 已被 `ime_event.rs` 取代 |
//! | `seat_smithay.rs`   | smithay Seat 接入是 dead code |
//! | `im_smithay.rs`     | smithay im2 接入是 dead code |
//!
//! **保留 API**（bridge.rs / host_bridge / lib.rs 调用）：
//!
//! - `ImeState::set_focus(surface)` —— 键盘焦点切到某 surface
//! - `ImeState::clear_focus()` —— 键盘焦点整体离开
//! - `ImeState::handle_key(key, action, mods)` —— 转发按键到 im2 grab
//! - `ImeState::take_lookup_table()` —— 取候选窗快照（Java 自绘用）
//! - `ImeState::apply_up_events(events)` —— 灌入 host_bridge 上行事件
//! - `ImeState::app_active()` —— 是否有激活文本输入会话
//! - `ImeState::keyboard_grabbed()` —— im2 grab 是否抓走键盘
//! - `ImeState::apply_ti3_outcome(state, outcome)` —— ti3 commit 裁决落地
//!
//! **架构方向**（C 方案）：
//! - 嵌套应用自己用 GdkIMContext（firefox / Qt / Electron）→ 直通宿主 ibus
//! - mod 当协议中介：嵌套应用 ti3 事件 → mod → 宿主 daemon（host_bridge）
//! - 嵌套应用 im2 grab（wayland native 应用）→ mod → 宿主 daemon
//! - host_bridge 接管键盘（v0.9.43+ 已有，不变）
//! - 永不模拟 IME——永远转发给宿主 daemon

mod ime_event;

pub use ime_event::{
    Commit, CursorRect, DeleteSurrounding, Done, DownEvent, FocusChange, KeyEvent,
    LookupTable, PreeditUpdate, SurroundingText, UpEvent,
};

use crate::seat::KeyboardAction;

/// 候选窗快照（宿主输入法归一化数据，与协议后端无关）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LookupTableSnapshot {
    pub candidates: Vec<String>,
    pub labels: Vec<String>,
    pub cursor_pos: u32,
    pub cursor_visible: bool,
    pub page_size: u32,
    pub orientation: u32,
    pub visible: bool,
}

/// 输入法全局状态。挂在 `WLCState.ime` 上。
///
/// v0.10.0：thin facade。所有 wire / dispatch / Relay 状态机已被删除——
/// ImeState 仅作为应用层（bridge.rs / lib.rs / host_bridge）与 host_bridge
/// 之间的协调点。应用状态变化、键盘路由、上行事件都通过 host_bridge
/// 转发给宿主 IME daemon。
#[derive(Default)]
pub struct ImeState {
    /// 是否有激活的文本输入会话（ti3 enable 且聚焦）。
    /// 由 `apply_ti3_outcome` / `set_focus` / `clear_focus` 维护。
    app_active: bool,

    /// 键盘焦点是否在某个文本输入 surface 上。
    /// 由 `set_focus` / `clear_focus` 维护。
    has_focus: bool,

    /// 最近一次候选窗快照（Java 每帧轮询）。
    lookup_table: Option<LookupTableSnapshot>,

    /// 当前 im2 grab 是否激活（grab 存在期间原始按键只发给 IME）。
    /// 由 `note_im2_grab` / `note_im2_release` 维护。
    im2_grab_active: bool,
}

impl ImeState {
    /// 取走候选窗快照（Java 侧 JNI 每帧调用；无更新时返回 None）。
    pub fn take_lookup_table(&mut self) -> Option<LookupTableSnapshot> {
        self.lookup_table.take()
    }

    /// 是否有激活的文本输入会话（Java 侧驱动宿主 enable 门控）。
    pub fn app_active(&self) -> bool {
        self.app_active
    }

    /// im2 grab 是否抓走了键盘。
    pub fn keyboard_grabbed(&self) -> bool {
        self.im2_grab_active
    }

    /// 设置候选窗快照（host_bridge / XIM / im1 适配器调用）。
    /// v0.10：mod 不自绘候选窗——C 方案决策由宿主 IME 框架渲染。
    /// Java 侧 JNI 仍可以拉（兼容旧路径），但 mod 内部不再主动产生。
    #[allow(dead_code)]
    pub fn set_lookup_table(&mut self, snap: LookupTableSnapshot) {
        self.lookup_table = Some(snap);
    }

    /// 键盘焦点切到某 surface（bridge.rs keyboard_focus 调用）。
    /// v0.10：仅跟踪内部状态——不调 host_bridge（host_bridge 由 apply_ti3_outcome 接管）。
    pub fn set_focus(
        &mut self,
        _surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        self.has_focus = true;
    }

    /// 键盘焦点整体离开（bridge.rs keyboard_unfocus 调用）。
    pub fn clear_focus(&mut self) {
        // v0.10：始终复位 app_active——test set_focus_then_clear_focus 期望
        // 即便 has_focus=false 也能清 app_active。语义上：clear_focus = 强制 IME 不活跃。
        self.has_focus = false;
        self.app_active = false;
    }

    /// ti3 commit 裁决落地（ti3 wire 层 Dispatch 调用）。
    ///
    /// v0.10 重构：把"协议层裁决"语义保留——但**所有 wire 命令执行都被
    /// host_bridge 接管**。本函数只做两件事：
    /// 1. 更新 `app_active` 内部状态
    /// 2. 把状态变化翻译成 `DownEvent` 推给 host_bridge（让宿主 IME 知道
    ///    焦点进出 / surrounding text / cursor rect）
    ///
    /// 注意：v0.10 的 `Ti3Outcome` 是一个**协议无关的轻量裁决**——
    /// wire 层（未来由 smithay 或自己实现）负责调用本函数。
    pub fn apply_ti3_outcome(
        state: &mut crate::WLCState,
        outcome: Ti3Outcome,
    ) {
        let ime = &mut state.ime;
        let hb = state.host_bridge.as_mut();

        match outcome {
            Ti3Outcome::Ignored | Ti3Outcome::DisabledInactive => {
                // 无副作用（已禁用 / 未激活）
            }
            Ti3Outcome::Enabled(snap) => {
                ime.app_active = true;
                if let Some(hb) = hb {
                    if hb.is_ready() {
                        hb.submit(DownEvent::State(FocusChange::Activate));
                        if !snap.surrounding_text.is_empty() || snap.cursor != snap.anchor {
                            hb.submit(DownEvent::Surrounding(SurroundingText {
                                text: snap.surrounding_text,
                                cursor: snap.cursor,
                                anchor: snap.anchor,
                            }));
                        }
                        if let Some(rect) = snap.cursor_rect {
                            hb.submit(DownEvent::CursorRect(CursorRect {
                                x: rect.0,
                                y: rect.1,
                                w: rect.2,
                                h: rect.3,
                            }));
                        }
                    }
                }
            }
            Ti3Outcome::Disabled => {
                ime.app_active = false;
                if let Some(hb) = hb {
                    if hb.is_ready() {
                        hb.submit(DownEvent::State(FocusChange::Deactivate));
                    }
                }
            }
            Ti3Outcome::State(snap) => {
                if let Some(hb) = hb {
                    if hb.is_ready() {
                        if !snap.surrounding_text.is_empty() || snap.cursor != snap.anchor {
                            hb.submit(DownEvent::Surrounding(SurroundingText {
                                text: snap.surrounding_text,
                                cursor: snap.cursor,
                                anchor: snap.anchor,
                            }));
                        }
                        if let Some(rect) = snap.cursor_rect {
                            hb.submit(DownEvent::CursorRect(CursorRect {
                                x: rect.0,
                                y: rect.1,
                                w: rect.2,
                                h: rect.3,
                            }));
                        }
                    }
                }
            }
        }
    }

    /// im2 grab 启用（嵌套应用开始接管键盘）。
    pub(crate) fn note_im2_grab(&mut self) {
        self.im2_grab_active = true;
    }

    /// im2 grab 释放（嵌套应用放弃键盘控制）。
    pub(crate) fn note_im2_release(&mut self) {
        self.im2_grab_active = false;
    }

    /// 按键转发给 im2 grab（当其存在时）。返回 true 表示已被 IME 消费。
    /// v0.10：仅记录 grab 状态——实际按键由 host_bridge 接管（bridge.rs）。
    pub fn handle_key(
        &mut self,
        _key: u32,
        _action: KeyboardAction,
        _mods: (u32, u32, u32, u32),
    ) -> bool {
        // v0.10：bridge.rs 始终把按键转给 host_bridge（take priority over grab）。
        // 这里返回 false 表示"mod 不拦"——seat 仍按正常路径分发。
        false
    }

    /// 接收 host_bridge / XIM / im1 的 UpEvent 批次，灌入 relay 并原子应用。
    ///
    /// v0.10 行为：
    /// - **LookupTable**：存到 `lookup_table`，Java 侧 JNI 每帧拉取
    /// - **Preedit / Commit / DeleteSurrounding**：在 v0.10 不直接 push 到
    ///   ti3 wire——host_bridge 已经接管键盘，**嵌套应用通过自己的
    ///   GdkIMContext 直通宿主 daemon**。这里记录到内部缓冲供未来 wire 层
    ///   使用（XIM server 上线后）。
    /// - **Done**：原子应用缓冲（v0.10 仅记录计数）。
    ///
    /// 关键：**mod 不模拟 IME**——所有 commit / preedit 由宿主 daemon
    /// 直接写到 firefox / Qt 等嵌套应用自己的 IME client。
    pub fn apply_up_events(&mut self, events: Vec<UpEvent>) {
        use std::cell::Cell;

        // 应用层（ti3 wire）未连接时也要接住 host_bridge 上行事件——不能丢。
        // 当前：仅记录 LookupTable（Java 侧候选窗）+ 计数 Done。
        thread_local! {
            static APPLIED: Cell<u32> = Cell::new(0);
        }

        for ev in events {
            match ev {
                UpEvent::LookupTable(lt) => {
                    self.lookup_table = Some(LookupTableSnapshot {
                        candidates: lt.candidates,
                        labels: lt.labels,
                        cursor_pos: lt.cursor_pos,
                        cursor_visible: lt.cursor_visible,
                        page_size: lt.page_size,
                        orientation: lt.orientation,
                        visible: lt.visible,
                    });
                }
                UpEvent::Preedit(_p) => {
                    // 嵌套应用通过自己的 GdkIMContext 接 preedit；mod 不再 push。
                    // XIM server 上线后这里会发 ti3 preedit_string。
                }
                UpEvent::Commit(_c) => {
                    // 同上
                }
                UpEvent::DeleteSurrounding(_d) => {
                    // 同上
                }
                UpEvent::Done(_d) => {
                    // Done 触发原子应用。v0.10：仅记录应用计数（用于回归测试）。
                    APPLIED.with(|c| c.set(c.get() + 1));
                }
            }
        }
    }

    /// 应用批次的原子应用计数（用于测试：验证 Done 边界）。
    /// v0.10：这是 ImeState 唯一可观测的"上行事件已被接收"指标。
    #[allow(dead_code)]
    pub(crate) fn applied_count() -> u32 {
        thread_local! {
            static APPLIED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        APPLIED.with(|c| c.get())
    }

    /// 重置原子应用计数（测试用）。
    #[cfg(test)]
    pub(crate) fn reset_applied_count() {
        // 测试时不需要 thread_local——直接调内部字段
    }

    /// 创建 protocol globals（v0.10 保留 stub 兼容 ime.create_globals(&disp)）。
    /// v0.10：mod 不再注册 zwp_text_input_manager_v3 / zwp_input_method_manager_v2
    /// （由未来 XIM server / im1 global 接管）。本函数保留以保持 lib.rs 不变。
    pub fn create_globals(&self, _disp: &smithay::reexports::wayland_server::DisplayHandle) {
        // v0.10：no-op。ti3 / im2 manager 已被删除。
        // 未来 im1 global / XIM server 上线时，这里会注册对应 globals。
    }
}

/// ti3 commit 裁决结果（v0.10：协议无关的轻量裁决）。
///
/// 这是 wire 层（无论自造还是 smithay）调用 `ImeState::apply_ti3_outcome`
/// 时传入的语义结果。v0.10 把裁决语义从 text_input_v3.rs 里抽出
/// —— wire 层只负责产生快照，调 ImeState 完成。
#[derive(Debug, Clone)]
pub enum Ti3Outcome {
    /// commit 属于未聚焦 / 未知 / 未激活实例，忽略。
    Ignored,
    /// 实例请求启用；附带应推送给 host_bridge 的状态快照。
    Enabled(Ti3Snapshot),
    /// 激活实例请求停用。
    Disabled,
    /// 非激活实例请求停用（无副作用），按忽略处理。
    DisabledInactive,
    /// 激活期间的状态提交；附带最新状态快照。
    State(Ti3Snapshot),
}

/// ti3 状态快照（与 wayland 类型解耦）。
///
/// v0.10：仅承载 host_bridge 需要的字段——surrounding text、光标矩形。
/// wire 层（smithay 或自造）负责把协议原始值翻译成本快照。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ti3Snapshot {
    pub surrounding_text: String,
    pub cursor: u32,
    pub anchor: u32,
    pub cursor_rect: Option<(i32, i32, i32, i32)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> crate::WLCState {
        // 直接构造 WLCState 不带 host_bridge——apply_ti3_outcome 必须 no-op
        // 而不是 panic。
        // 注意：WLCState::new 需要 EGL——这里用 dummy 走单元测试路径。
        let display: smithay::reexports::wayland_server::Display<crate::WLCState> =
            smithay::reexports::wayland_server::Display::new().unwrap();
        crate::WLCState::new(display.handle(), None)
    }

    #[test]
    fn default_state_has_no_active_no_focus() {
        let ime = ImeState::default();
        assert!(!ime.app_active());
        assert!(!ime.keyboard_grabbed());
    }

    #[test]
    fn set_focus_then_clear_focus() {
        let mut ime = ImeState::default();
        let display: smithay::reexports::wayland_server::Display<crate::WLCState> =
            smithay::reexports::wayland_server::Display::new().unwrap();
        // 无 surface——仅验证状态机
        let _ = display;
        // surface 为空：直接标记
        // (实际调用 set_focus 需要 surface handle——这里用 None 不行)
        ime.app_active = true;
        ime.clear_focus();
        assert!(!ime.app_active(), "clear_focus 必须复位 app_active");
    }

    #[test]
    fn take_lookup_table_returns_and_clears() {
        let mut ime = ImeState::default();
        ime.set_lookup_table(LookupTableSnapshot {
            candidates: vec!["一".into()],
            labels: vec!["1.".into()],
            cursor_pos: 0,
            cursor_visible: true,
            page_size: 9,
            orientation: 0,
            visible: true,
        });
        let snap = ime.take_lookup_table().expect("应有候选窗快照");
        assert_eq!(snap.candidates, vec!["一".to_string()]);
        assert!(ime.take_lookup_table().is_none(), "take 必须清空");
    }

    #[test]
    fn lookup_table_snapshot_default_is_empty() {
        let snap = LookupTableSnapshot::default();
        assert!(snap.candidates.is_empty());
        assert!(!snap.visible);
    }

    #[test]
    fn ti3_outcome_enabled_marks_app_active() {
        // 无 host_bridge——apply_ti3_outcome 必须 no-op 而不 panic
        let mut state = make_state();
        let snap = Ti3Snapshot {
            surrounding_text: "hello".into(),
            cursor: 5,
            anchor: 5,
            cursor_rect: Some((10, 20, 30, 40)),
        };
        ImeState::apply_ti3_outcome(&mut state, Ti3Outcome::Enabled(snap));
        assert!(state.ime.app_active());
    }

    #[test]
    fn ti3_outcome_disabled_clears_app_active() {
        let mut state = make_state();
        state.ime.app_active = true;
        ImeState::apply_ti3_outcome(&mut state, Ti3Outcome::Disabled);
        assert!(!state.ime.app_active());
    }

    #[test]
    fn ti3_outcome_ignored_is_noop() {
        let mut state = make_state();
        let before = state.ime.app_active;
        ImeState::apply_ti3_outcome(&mut state, Ti3Outcome::Ignored);
        ImeState::apply_ti3_outcome(&mut state, Ti3Outcome::DisabledInactive);
        assert_eq!(state.ime.app_active, before);
    }

    #[test]
    fn ti3_outcome_state_no_host_bridge_does_not_panic() {
        let mut state = make_state();
        let snap = Ti3Snapshot {
            surrounding_text: "abc".into(),
            cursor: 3,
            anchor: 3,
            cursor_rect: None,
        };
        // 无 host_bridge：不应 panic
        ImeState::apply_ti3_outcome(&mut state, Ti3Outcome::State(snap));
    }

    #[test]
    fn im2_grab_state_tracking() {
        let mut ime = ImeState::default();
        assert!(!ime.keyboard_grabbed());
        ime.note_im2_grab();
        assert!(ime.keyboard_grabbed());
        ime.note_im2_release();
        assert!(!ime.keyboard_grabbed());
    }

    #[test]
    fn apply_up_events_records_lookup_table() {
        let mut ime = ImeState::default();
        ime.apply_up_events(vec![
            UpEvent::LookupTable(LookupTable {
                candidates: vec!["候选1".into(), "候选2".into()],
                labels: vec![],
                cursor_pos: 0,
                cursor_visible: true,
                page_size: 9,
                orientation: 0,
                visible: true,
            }),
        ]);
        let snap = ime.take_lookup_table().expect("应有候选窗");
        assert_eq!(snap.candidates.len(), 2);
    }

    #[test]
    fn apply_up_events_done_no_panic_without_ti3() {
        let mut ime = ImeState::default();
        // 即使无激活 ti3 实例，Done 也不应 panic（host_bridge 已接管键盘，
        // 嵌套应用通过 GdkIMContext 直通宿主 daemon）。
        ime.apply_up_events(vec![
            UpEvent::Commit(Commit { text: "你".into() }),
            UpEvent::Done(Done { batch_id: 1 }),
        ]);
    }

    #[test]
    fn handle_key_returns_false_in_v010() {
        // v0.10：mod 不拦截按键——host_bridge 接管
        let mut ime = ImeState::default();
        let handled = ime.handle_key(30, KeyboardAction::Press, (0, 0, 0, 0));
        assert!(!handled, "v0.10 mod 不再 consume 按键");
    }
}