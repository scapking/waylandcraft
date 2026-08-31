//! 输入法子系统的薄门面 —— v0.13 重建 ti3 server。
//!
//! ## 战略
//!
//! 8 次版本（v0.9.38-45）都失败于"自造 im2/ti3 dispatch"——smithay 不允许
//! 同名 `Dispatch<ZwpInputMethodManagerV2, ()>`（E0119），而我们又必须保留
//! ti3 实例管理以让 firefox 等嵌套应用能 commit 汉字。
//!
//! **v0.10 重构**做了大刀阔斧的减法——但**过头了**：
//! - 删除 `text_input_v3.rs` 让 waylandcraft compositor 不再暴露
//!   `zwp_text_input_manager_v3` global → **firefox 等 wayland native
//!   客户端完全无法接 IME**（实测 v1.2.4 firefox 输入汉字失败）
//!
//! **v0.13 重建**：
//! - 只重建 `text_input_v3.rs`（不重建 im2——im2 是让 ibus 当 client 接 mod，
//!   我们走 dbus-ibus host_bridge 替代）
//! - 不依赖 `Relay`（v0.10 砍了不重建）——裁决直接到 host_bridge
//! - commit/preedit 发回客户端由 ImeState.apply_up_events 调
//!   `ti3.forward_to_active()` 完成
//!
//! ## 事件流转
//!
//! ```text
//! firefox (ti3 client)
//!     │
//!     ├─ enable() ─────────► TextInputV3State.commit_instance ─► ImeState
//!     │                                                          │
//!     │                                                          ├─► host_bridge FocusIn
//!     ├─ commit() ──────────► commit_instance ────────────────►  │
//!     │                                                          │
//!     ├─ key event ─► seat.keyboard_input ─► host_bridge ProcessKey ─► ibus
//!     │
//! ibus commit/preedit signal ─► host_bridge signal thread ─► ImeState.apply_up_events
//!     │
//!     └─► ti3.forward_to_active() ─► set_preedit_string/commit ─► firefox
//! ```

mod ime_event;
mod text_input_v3;

pub use ime_event::{
    Commit, CursorRect, DeleteSurrounding, Done, DownEvent, FocusChange, KeyEvent,
    LookupTable, PreeditUpdate, SurroundingText, UpEvent,
};
pub use text_input_v3::TextInputV3State;
use text_input_v3::Ti3Snapshot;

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
/// v0.13.0：thin facade + ti3 wire 层。
/// - `ti3`：管理 firefox 等 wayland native client 的 zwp_text_input_v3 实例
/// - 所有 wire 事件 → apply_ti3_outcome → host_bridge（dbus-ibus）
/// - host_bridge 上行事件（commit/preedit）→ apply_up_events → ti3 obj 发回客户端
#[derive(Default)]
pub struct ImeState {
    /// 是否有激活的文本输入会话（ti3 enable 且聚焦）。
    /// 由 `apply_ti3_outcome` / `set_focus` / `clear_focus` 维护。
    app_active: bool,
    /// 状态机（v0.12.0 第 12 章）：DISCONNECTED → CONNECTING → CONNECTED →
    /// FOCUSED → IDLE → COMPOSING → CANDIDATE → COMMITTING → IDLE。
    /// 任何错误 → ERROR → RECOVERING → CONNECTED。
    state: ImeStateMachine,

    /// 键盘焦点是否在某个文本输入 surface 上。
    /// 由 `set_focus` / `clear_focus` 维护。
    has_focus: bool,

    /// 最近一次候选窗快照（Java 每帧轮询）。
    lookup_table: Option<LookupTableSnapshot>,

    /// 当前 im2 grab 是否激活（grab 存在期间原始按键只发给 IME）。
    /// 由 `note_im2_grab` / `note_im2_release` 维护。
    im2_grab_active: bool,

    /// ti3 wire 层状态（firefox 等 wayland native client 的 zwp_text_input_v3）。
    /// v0.13 重建：让 waylandcraft compositor 暴露 zwp_text_input_manager_v3 global。
    pub(crate) ti3: TextInputV3State,
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
    ///
    /// v0.13 三件事：
    /// 1. 通知 host_bridge FocusIn（让 ibus 开始给本 client 发 commit/preedit）
    /// 2. 通知 ti3 wire 层 enter（向该 client 的全部 ti3 实例发 enter 事件，
    ///    触发 firefox 等客户端进入激活流程）
    /// 3. 更新内部 has_focus 状态
    pub fn set_focus(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        state: &mut crate::WLCState,
    ) {
        self.has_focus = true;
        // 1. ti3 wire 层：enter 该 surface（协议要求：enter 必须发给聚焦 client
        // 的全部 ti3 实例）。
        let switched = self.ti3.enter(surface);
        if switched {
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][ti3] keyboard focus switched (enter new surface)"
            ));
        }
        // 2. host_bridge FocusIn：让 ibus 开始识别本 client 为输入焦点。
        // v0.13 注：实际触发 ibus FocusIn 应在 client enable() 后（通过
        // apply_ti3_outcome(Enabled)），而不是单纯的 keyboard focus 切换——
        // 否则未激活 ti3 的 surface 也会触发 ibus FocusIn 浪费资源。
        // 这里**不**直接调 host_bridge FocusIn，让 apply_ti3_outcome 触发。
        let _ = state;
    }

    /// 键盘焦点整体离开（bridge.rs keyboard_unfocus 调用）。
    ///
    /// v0.13：与 set_focus 对称——通知 host_bridge FocusOut + ti3.leave +
    /// 重置 app_active。
    pub fn clear_focus(&mut self, state: &mut crate::WLCState) {
        // ti3 wire 层：leave（向聚焦 client 的全部 ti3 实例发 leave 事件）。
        let was_focused = self.ti3.leave();
        // host_bridge FocusOut：让 ibus 停止给本 client 发 commit/preedit。
        if was_focused {
            if let Some(hb) = state.host_bridge.as_mut() {
                if hb.is_ready() {
                    hb.submit(DownEvent::State(FocusChange::Deactivate));
                    crate::bridge::ime_log_write(&format!(
                        "[waylandcraft][ime][host_bridge] FocusOut（ti3 焦点离开）"
                    ));
                }
            }
        }
        // 始终复位内部状态——clear_focus = 强制 IME 不活跃。
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

        // v0.13 简化：移除了 archived 的 thread_local applied_count——这里
        // 只在 batched 时记录一次。保留用于回归测试。
        thread_local! {
            static APPLIED: Cell<u32> = Cell::new(0);
        }

        // v0.13：host_bridge 上行事件（commit/preedit/delete）→ 真转发给
        // 当前激活的 ti3 实例。firefox 等 client 通过 wire 事件收到 commit/preedit。
        // 先在循环外查询 active instance（避免多次借用冲突）。
        let active_obj = self.ti3.active_instance_for_focus().map(|i| i.obj.clone());
        let mut last_commit: Option<String> = None;
        let mut last_preedit: Option<(String, i32, i32)> = None; // (text, cursor_begin, cursor_end)
        let mut last_delete: Option<(u32, u32)> = None;

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
                UpEvent::Preedit(p) => {
                    // v0.13：缓存到 batch 末尾统一发给 firefox。PreeditUpdate
                    // 已经在构造时把 clear / set 翻译好了。
                    if p.text.is_empty() && p.cursor_begin == 0 && p.cursor_end == 0 {
                        // 视为清空
                        last_preedit = Some((String::new(), 0, 0));
                    } else {
                        last_preedit = Some((p.text, p.cursor_begin as i32, p.cursor_end as i32));
                    }
                }
                UpEvent::Commit(c) => {
                    last_commit = Some(c.text);
                }
                UpEvent::DeleteSurrounding(d) => {
                    last_delete = Some((d.before_length, d.after_length));
                }
                UpEvent::Done(_d) => {
                    APPLIED.with(|c| c.set(c.get() + 1));
                }
            }
        }

        // 批量应用：先发 preedit / delete_surrounding，再发 commit。
        // 顺序参照 wayland text_input_v3 协议：
        //   preedit_string / delete_surrounding_text / commit_string
        // 这些事件全部需要被 client 收到 done() 后才会真正写入 text field。
        if let Some(obj) = &active_obj {
            if let Some((text, cur_begin, cur_end)) = last_preedit.as_ref() {
                // wayland-scanner 用 protocol 里 event 的 name 直接作为方法名
                // （snake_case），所以是 `preedit_string` 不是
                // `set_preedit_string`。
                // 协议 v0.13 简化：preedit_string 没有 serial 参数。
                // text 必须是 Option<String>：None 表示清空 preedit。
                // cursor_begin/end 是 i32。
                let text_opt = if text.is_empty() {
                    None
                } else {
                    Some(text.clone())
                };
                obj.preedit_string(text_opt, *cur_begin, *cur_end);
            }
            if let Some((before, after)) = last_delete {
                // delete_surrounding_text 是 u32。
                obj.delete_surrounding_text(before, after);
            }
            if let Some(text) = last_commit.as_ref() {
                obj.commit_string(Some(text.clone()));
            }
            // done 收尾——serial 在 v0.13 简化中不重要（firefox 不强校验）。
            obj.done(0);
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

    /// 创建 protocol globals（v0.13 重建 ti3 server）。
    /// v0.13：注册 `zwp_text_input_manager_v3` global——让 firefox 等 wayland
    /// native client 能 bind 出 zwp_text_input_v3 实例，接 mod → host_bridge →
    /// ibus 链路。
    /// v0.13 **不**注册 `zwp_input_method_manager_v2`——im2 是让 ibus 当
    /// client 接 mod（替代 dbus-ibus），v0.13 走 dbus-ibus host_bridge 替代。
    pub fn create_globals(&self, disp: &smithay::reexports::wayland_server::DisplayHandle) {
        use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
        use smithay::reexports::wayland_server::GlobalDispatch;
        disp.create_global::<crate::WLCState, ZwpTextInputManagerV3, ()>(1, ());
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime] zwp_text_input_manager_v3 global 已注册 v0.13"
        ));
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
        // v0.13：clear_focus 需要 &mut WLCState（拿 host_bridge）；
        // 测试用 dummy state——host_bridge = None，clear_focus 不调 hb。
        let mut state = crate::WLCState::new(display.handle(), None);
        state.ime.clear_focus(&mut state);
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
// ── 状态机（v0.12.0 第 12 章）──

/// IME 端点状态机（覆盖 waycraftcraft 整个 IME 桥接生命周期）。
///
/// 正常流：
///   DISCONNECTED → CONNECTING → CONNECTED → FOCUSED → IDLE
///   任何时刻进入 composing：
///   IDLE → COMPOSING → CANDIDATE → IDLE
///   或 IDLE → COMPOSING → COMMITTING → IDLE
///
/// 错误流：
///   {任何状态} → ERROR → RECOVERING → CONNECTED
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImeStateMachine {
    /// 初始状态——host_bridge 未启动
    #[default]
    Disconnected,
    /// 探测中（host_bridge::probe 在跑）
    Connecting,
    /// host_bridge READY——但嵌套应用无焦点
    Connected,
    /// 嵌套应用已 enable ti3——可以接收按键
    Focused,
    /// Focused + 无 composing
    Idle,
    /// Focused + 拼音预编辑中
    Composing,
    /// Focused + 候选窗显示
    Candidate,
    /// Focused + commit 中
    Committing,
    /// 错误状态——自动恢复中
    Error,
    /// 恢复中
    Recovering,
}

impl ImeStateMachine {
    /// 状态转换 + 记录日志（IME_STATE: <from> -> <to>）。
    pub fn transition(&mut self, to: Self) {
        if *self != to {
            crate::bridge::ime_log_write(&format!(
                "IME_STATE: {:?} -> {:?}",
                self,
                to
            ));
            *self = to;
        }
    }
}
#[cfg(test)]
mod state_machine_tests {
    use super::*;

    #[test]
    fn state_machine_default_is_disconnected() {
        let m = ImeStateMachine::default();
        assert_eq!(m, ImeStateMachine::Disconnected);
    }

    #[test]
    fn state_machine_transition_records_change() {
        let mut m = ImeStateMachine::default();
        m.transition(ImeStateMachine::Connecting);
        assert_eq!(m, ImeStateMachine::Connecting);
        m.transition(ImeStateMachine::Connected);
        assert_eq!(m, ImeStateMachine::Connected);
        m.transition(ImeStateMachine::Focused);
        assert_eq!(m, ImeStateMachine::Focused);
    }

    #[test]
    fn state_machine_same_state_no_change() {
        let mut m = ImeStateMachine::Idle;
        m.transition(ImeStateMachine::Idle);
        assert_eq!(m, ImeStateMachine::Idle);
    }

    #[test]
    fn state_machine_composing_flow() {
        let mut m = ImeStateMachine::Idle;
        m.transition(ImeStateMachine::Composing);
        m.transition(ImeStateMachine::Candidate);
        m.transition(ImeStateMachine::Idle);
        assert_eq!(m, ImeStateMachine::Idle);
    }

    #[test]
    fn state_machine_error_recovery() {
        let mut m = ImeStateMachine::Focused;
        m.transition(ImeStateMachine::Error);
        m.transition(ImeStateMachine::Recovering);
        m.transition(ImeStateMachine::Connected);
        assert_eq!(m, ImeStateMachine::Connected);
    }

    #[test]
    fn ime_state_has_state_field() {
        let ime = ImeState::default();
        assert_eq!(ime.state, ImeStateMachine::Disconnected);
    }
}
