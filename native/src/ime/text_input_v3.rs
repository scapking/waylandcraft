//! `zwp_text_input_v3` / `zwp_text_input_manager_v3` wire 层。
//!
//! v0.13 重建（从 .deleted_v010/ 还原 + 适配）：让 waylandcraft 作为 wayland
//! compositor 暴露 text-input 协议，让 firefox 等 wayland native 客户端能
//! 接宿主 IME（ibus / fcitx5）。
//!
//! ## 协议要点
//!
//! - `done(serial)` 的 serial 必须等于**该实例**收到的 commit 请求数
//!   —— 因此计数器是 per-instance 的，绝不共享；
//! - enter 必须发给聚焦 client 的全部实例；leave 之后必须忽略一切请求。
//!
//! ## v0.13 简化
//!
//! - 删除对 `Relay` 的依赖（v0.10 整体被删），所有裁决直接通过
//!   `ImeState::apply_ti3_outcome` 走 host_bridge；
//! - Commit / Preedit 不再"等 relay 原子应用"——host_bridge 信号线程
//!   自己攒批（`take_up_events_batched`），由 lib.rs update 灌入 ImeState；
//! - 上行 commit / preedit 直接通过对应的 ZwpTextInputV3 obj 发回 client。
//!
//! ## 事件流转
//!
//! ```text
//! firefox (ti3 client)
//!     │
//!     ├─ enable() ─────────────► TextInputV3State.commit_instance() ──► ImeState
//!     │                                                                  │
//!     │                                                                  ├─► host_bridge FocusIn
//!     ├─ set_surrounding_text() ─► TextInputV3State.pending ──────────►  │
//!     ├─ set_cursor_rectangle() ─► TextInputV3State.pending ──────────►  │
//!     │                                                                  │
//!     ├─ commit() ─────────────► TextInputV3State.commit_instance() ◄───┘
//!     │
//!     ├─ key event（通过 seat 转发）──► host_bridge ProcessKeyEvent ──► ibus
//!     │
//! ibus commit/preedit signal ─► host_bridge signal thread ─► ImeState.apply_up_events
//!     │
//!     └─► ti3.set_preedit_string() / commit() ◄── firefox 收 commit/preedit
//! ```

use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
    zwp_text_input_v3::{self, ZwpTextInputV3},
};
use smithay::reexports::wayland_server::{
    backend::ClientId,
    protocol::wl_surface::WlSurface,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

/// ti3 状态快照（与 wayland 类型解耦，给 ImeState 内部用）。
///
/// v0.13 简化：只承载 host_bridge 需要的字段——surrounding text、光标矩形。
/// wire 层负责把协议原始值翻译成本快照。
#[derive(Debug, Clone, Default)]
pub(crate) struct Ti3Snapshot {
    pub surrounding_text: String,
    pub cursor: u32,
    pub anchor: u32,
    pub cursor_rect: Option<(i32, i32, i32, i32)>,
}

/// 单个 text_input 实例的待提交状态（double-buffer，随 commit 应用）。
#[derive(Default)]
pub(crate) struct Ti3Pending {
    pub enable: Option<bool>,
    pub surrounding_text: Option<String>,
    pub surrounding_cursor: u32,
    pub surrounding_anchor: u32,
    pub cursor_rectangle: Option<(i32, i32, i32, i32)>,
}

/// 一个 text_input 对象的完整服务端状态。
pub(crate) struct Ti3Instance {
    pub obj: ZwpTextInputV3,
    pub pending: Ti3Pending,
    /// 本实例收到的 commit 请求总数 —— 协议规定的 done(serial) 基准。
    pub commit_count: u32,
}

/// ti3 wire 层状态。挂在 `ImeState.ti3` 上。
#[derive(Default)]
pub struct TextInputV3State {
    instances: Vec<Ti3Instance>,
    /// 当前持有 text-input 焦点的 surface。
    focus_surface: Option<WlSurface>,
}

/// [`TextInputV3State::commit_instance`] 的裁决结果。
///
/// 与 `super::Ti3Outcome` 等价——wire 层不重新定义同名枚举，直接用 super。
/// v0.13 简化：移除了 archived 的 `Ignored` 因为我们的实现里 commit 总是已
/// 找到实例（否则进 dispatch 早就 return 了）；移除了 `DisabledInactive`
/// 因为新实现不区分激活/非激活（只看在 ti3.focus_surface 范围内）。
pub(crate) type Ti3CommitOutcome = super::Ti3Outcome;

impl TextInputV3State {
    /// 焦点进入 surface：向该 client 的全部实例发 enter（协议强制），
    /// 并记录焦点归属。由 seat 键盘焦点变化驱动。
    ///
    /// 返回 true 表示焦点确实发生了切换（含从旧 surface 直接切到新
    /// surface 的场景）；调用方需据此终结旧会话（ImeState 层），
    /// 否则新 surface 的 enable 会被误判为「会话仍在进行」。
    pub fn enter(&mut self, surface: &WlSurface) -> bool {
        if self.focus_surface.as_ref().is_some_and(|s| s == surface) {
            return false;
        }
        // 切换前先离开旧焦点（无旧焦点时为空操作）。
        self.leave();
        let Some(client) = surface.client() else { return true };
        self.focus_surface = Some(surface.clone());
        for inst in &mut self.instances {
            if inst.obj.client().is_some_and(|c| c == client) {
                inst.obj.enter(surface);
            }
        }
        true
    }

    /// 当前焦点 surface（供焦点切换判定）。
    #[allow(dead_code)]
    pub fn focus_surface(&self) -> Option<WlSurface> {
        self.focus_surface.clone()
    }

    /// 焦点离开：向聚焦 client 的全部实例发 leave、清除焦点与激活态。
    /// 返回 true 表示此前确实有焦点（调用方据此通知 ImeState focus_lost）。
    pub fn leave(&mut self) -> bool {
        let Some(focus) = self.focus_surface.take() else {
            return false;
        };
        if let Some(client) = focus.client() {
            for inst in &mut self.instances {
                if inst.obj.client().is_some_and(|c| c == client) {
                    inst.obj.leave(&focus);
                }
            }
        }
        true
    }

    /// 当前是否有激活的文本输入会话（供穿透 enable 门控与按键路由参考）。
    pub fn has_active(&self) -> bool {
        // v0.13：激活态由 ImeState.app_active 持有——这里只看是否有 focus
        // surface（与 v0.10 archived 版本行为等价于"has_focus"）。
        self.focus_surface.is_some()
    }

    /// v0.13.4：当前活着的 ti3 实例数（firefox 之类客户端的 zwp_text_input_v3 对象数）。
    /// status.rs 用于判断嵌套 IME 是否被启用。
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// 处理一个实例的 commit：应用 double-buffer，返回裁决结果。
    /// commit 计数无条件递增（协议按「该对象发出的请求」计数）。
    pub(crate) fn commit_instance(&mut self, obj: &ZwpTextInputV3) -> Ti3CommitOutcome {
        let Some(inst) = self.instances.iter_mut().find(|i| i.obj.id() == obj.id()) else {
            return Ti3CommitOutcome::Ignored;
        };
        inst.commit_count += 1;
        let new_state = std::mem::take(&mut inst.pending);

        match new_state.enable {
            Some(true) => Ti3CommitOutcome::Enabled(snapshot_from_pending(&new_state)),
            Some(false) => Ti3CommitOutcome::Disabled,
            None => Ti3CommitOutcome::State(snapshot_from_pending(&new_state)),
        }
    }

    /// 实例销毁清理。
    pub(crate) fn remove_instance(&mut self, id: &smithay::reexports::wayland_server::backend::ObjectId) -> bool {
        let before = self.instances.len();
        self.instances.retain(|i| i.obj.id() != *id);
        before != self.instances.len()
    }

    pub(crate) fn push_instance(&mut self, inst: Ti3Instance) {
        self.instances.push(inst);
    }

    pub(crate) fn instance_mut(&mut self, id: &smithay::reexports::wayland_server::backend::ObjectId) -> Option<&mut Ti3Instance> {
        self.instances.iter_mut().find(|i| i.obj.id() == *id)
    }

    /// 拿当前聚焦 client 的激活实例（用于把 commit/preedit 发回去）。
    /// v0.13 简化：返回聚焦 client 的第一个 instance（firefox 通常只有一个 ti3）。
    #[allow(dead_code)]
    pub fn active_instance_for_focus(&self) -> Option<&Ti3Instance> {
        let focus = self.focus_surface.as_ref()?;
        let client = focus.client()?;
        self.instances.iter().find(|i| i.obj.client().is_some_and(|c| c == client))
    }

    /// 当前聚焦 client（供事件路由校验使用）。
    #[allow(dead_code)]
    pub fn focused_client(&self) -> Option<Client> {
        self.focus_surface.as_ref().and_then(|s| s.client())
    }
}

fn snapshot_from_pending(p: &Ti3Pending) -> Ti3Snapshot {
    Ti3Snapshot {
        surrounding_text: p.surrounding_text.clone().unwrap_or_default(),
        cursor: p.surrounding_cursor,
        anchor: p.surrounding_anchor,
        cursor_rect: p.cursor_rectangle,
    }
}

// ─────────────────────────────────────────────────────────────
// Dispatch 实现（挂在 crate::WLCState 上）
// ─────────────────────────────────────────────────────────────

impl GlobalDispatch<ZwpTextInputManagerV3, ()> for crate::WLCState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpTextInputManagerV3>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for crate::WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_text_input_manager_v3::Request::GetTextInput { id, .. } => {
                let ti: ZwpTextInputV3 = data_init.init(id, ());
                // v0.13 P1：客户端创建 ti3 实例——记录到 ImeState.ti3。
                // 创建后**不**主动 send enter：enter 由 keyboard_focus 触发，
                // 避免 surface 还没聚焦就发 enter 让 client 提前 enable。
                state.ime.ti3.push_instance(Ti3Instance {
                    obj: ti,
                    pending: Ti3Pending::default(),
                    commit_count: 0,
                });
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][ti3] client created zwp_text_input_v3 instance"
                ));
            }
            zwp_text_input_manager_v3::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for crate::WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        ti: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let obj_id = ti.id();

        // commit 是唯一触发裁决流程的请求。
        if matches!(request, zwp_text_input_v3::Request::Commit) {
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][ti3] commit_instance obj={} -> apply_ti3_outcome",
                obj_id
            ));
            let outcome = state.ime.ti3.commit_instance(ti);
            super::ImeState::apply_ti3_outcome(state, outcome);
            return;
        }

        let Some(inst) = state.ime.ti3.instance_mut(&obj_id) else {
            return;
        };
        match request {
            zwp_text_input_v3::Request::Enable => {
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][ti3] Enable obj={obj_id}"
                ));
                inst.pending.enable = Some(true);
            }
            zwp_text_input_v3::Request::Disable => {
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][ti3] Disable obj={obj_id}"
                ));
                inst.pending.enable = Some(false);
            }
            zwp_text_input_v3::Request::SetSurroundingText { text, cursor, anchor } => {
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][ti3] SetSurroundingText obj={obj_id} text=\"{}\" cursor={cursor} anchor={anchor}",
                    text.chars().take(16).collect::<String>()
                ));
                inst.pending.surrounding_text = Some(text);
                inst.pending.surrounding_cursor = cursor as u32;
                inst.pending.surrounding_anchor = anchor as u32;
            }
            zwp_text_input_v3::Request::SetTextChangeCause { cause } => {
                if let Ok(_c) = cause.into_result() {
                    // v0.13 简化：不跟踪 change_cause（host_bridge 暂不需要）
                }
            }
            zwp_text_input_v3::Request::SetContentType { hint: _hint, purpose: _purpose } => {
                // v0.13 简化：不跟踪 content_hint/content_purpose（firefox 默认填
                // normal/terminal，应用层语义对 host_bridge 路由无影响）
            }
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                // 客户端通过 ti3 上报光标矩形——验证候选窗锚点能否取真实应用光标。
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][ti3] SetCursorRectangle obj={obj_id} rect=({x},{y},{width},{height})"
                ));
                inst.pending.cursor_rectangle = Some((x, y, width, height));
            }
            zwp_text_input_v3::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        ti: &ZwpTextInputV3,
        _data: &(),
    ) {
        let removed = state.ime.ti3.remove_instance(&ti.id());
        if removed {
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][ti3] destroyed obj={}",
                ti.id()
            ));
        }
    }
}