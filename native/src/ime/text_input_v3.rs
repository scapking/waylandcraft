//! `zwp_text_input_v3` / `zwp_text_input_manager_v3` wire 层。
//!
//! 职责边界：本模块只做协议对象管理、double-buffer pending 维护与
//! 类型转换；全部语义判定（激活、serial、IME 路由）委托给
//! [`Relay`](super::relay::Relay)，由 `mod.rs` 的 `ImeState` 统一执行。
//!
//! 协议要点（官方 XML）：
//! - `done(serial)` 的 serial 必须等于**该实例**收到的 commit 请求数
//!   —— 因此计数器是 per-instance 的，绝不共享；
//! - enter 必须发给聚焦 client 的全部实例；leave 之后必须忽略一切请求。

use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
    zwp_text_input_v3::{self, ZwpTextInputV3},
};
use smithay::reexports::wayland_server::{
    backend::{ClientId, ObjectId},
    protocol::wl_surface::WlSurface,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use super::relay::AppState;

/// 单个 text_input 实例的待提交状态（double-buffer，随 commit 应用）。
#[derive(Default)]
pub(crate) struct Ti3Pending {
    pub enable: Option<bool>,
    pub surrounding_text: Option<String>,
    pub surrounding_cursor: u32,
    pub surrounding_anchor: u32,
    pub content_hint: Option<u32>,
    pub content_purpose: Option<u32>,
    pub change_cause: Option<u32>,
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
    /// 当前激活（enable 且聚焦）的实例 id。
    active_id: Option<ObjectId>,
}

/// [`TextInputV3State::commit_instance`] 的裁决结果。
pub(crate) enum Ti3CommitOutcome {
    /// commit 属于未聚焦/未知/未激活实例，忽略。
    Ignored,
    /// 实例请求启用；附带应推送给 relay 的状态快照。
    Enabled(AppState),
    /// 激活实例请求停用。
    Disabled,
    /// 非激活实例请求停用（无副作用），按忽略处理。
    DisabledInactive,
    /// 激活期间的状态提交；附带最新状态快照。
    State(AppState),
}

impl TextInputV3State {
    /// 焦点进入 surface：向该 client 的全部实例发 enter（协议强制），
    /// 并记录焦点归属。由 seat 键盘焦点变化驱动。
    ///
    /// 返回 true 表示焦点确实发生了切换（含从旧 surface 直接切到新
    /// surface 的场景）；调用方需据此终结旧会话（Relay 层），
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

    /// 焦点离开：向聚焦 client 的全部实例发 leave、清除焦点与激活态。
    /// 返回 true 表示此前确实有焦点（调用方据此通知 relay.focus_lost）。
    /// 当前焦点 surface（供焦点切换判定）。
    pub fn focus_surface(&self) -> Option<WlSurface> {
        self.focus_surface.clone()
    }

    pub fn leave(&mut self) -> bool {
        self.active_id = None;
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
        self.active_id.is_some()
    }

    /// 取当前激活实例的对象 id。
    pub(crate) fn active_id(&self) -> Option<ObjectId> {
        self.active_id.clone()
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
            Some(true) => {
                self.active_id = Some(obj.id());
                Ti3CommitOutcome::Enabled(super::relay::AppState::from_pending(
                    new_state.surrounding_text.as_deref(),
                    new_state.surrounding_cursor,
                    new_state.surrounding_anchor,
                    new_state.content_hint.unwrap_or(0),
                    new_state.content_purpose.unwrap_or(0),
                    new_state.change_cause.unwrap_or(0),
                    new_state.cursor_rectangle,
                ))
            }
            Some(false) => {
                if self.active_id.as_ref() == Some(&obj.id()) {
                    self.active_id = None;
                    Ti3CommitOutcome::Disabled
                } else {
                    Ti3CommitOutcome::DisabledInactive
                }
            }
            None => {
                // 未显式 enable 的 commit 只有激活实例才有意义；
                // 其余实例（leave 后）按协议直接忽略。
                if self.active_id.as_ref() == Some(&obj.id()) {
                    Ti3CommitOutcome::State(AppState::from_pending(
                        new_state.surrounding_text.as_deref(),
                        new_state.surrounding_cursor,
                        new_state.surrounding_anchor,
                        new_state.content_hint.unwrap_or(0),
                        new_state.content_purpose.unwrap_or(0),
                        new_state.change_cause.unwrap_or(0),
                        new_state.cursor_rectangle,
                    ))
                } else {
                    Ti3CommitOutcome::Ignored
                }
            }
        }
    }

    /// 实例销毁清理；若销毁的是激活实例返回 true（调用方需停用会话）。
    pub(crate) fn remove_instance(&mut self, id: &ObjectId) -> bool {
        self.instances.retain(|i| i.obj.id() != *id);
        if self.active_id.as_ref() == Some(id) {
            self.active_id = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn push_instance(&mut self, inst: Ti3Instance) {
        self.instances.push(inst);
    }

    pub(crate) fn instance_mut(&mut self, id: &ObjectId) -> Option<&mut Ti3Instance> {
        self.instances.iter_mut().find(|i| i.obj.id() == *id)
    }

    /// 当前聚焦 client（供事件路由校验使用）。
    #[allow(dead_code)]
    pub fn focused_client(&self) -> Option<Client> {
        self.focus_surface.as_ref().and_then(|s| s.client())
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
                state.ime.ti3.push_instance(Ti3Instance {
                    obj: ti,
                    pending: Ti3Pending::default(),
                    commit_count: 0,
                });
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
            let outcome = state.ime.ti3.commit_instance(ti);
            super::ImeState::apply_ti3_outcome(state, outcome);
            return;
        }

        let Some(inst) = state.ime.ti3.instance_mut(&obj_id) else {
            return;
        };
        match request {
            zwp_text_input_v3::Request::Enable => inst.pending.enable = Some(true),
            zwp_text_input_v3::Request::Disable => inst.pending.enable = Some(false),
            zwp_text_input_v3::Request::SetSurroundingText { text, cursor, anchor } => {
                inst.pending.surrounding_text = Some(text);
                inst.pending.surrounding_cursor = cursor as u32;
                inst.pending.surrounding_anchor = anchor as u32;
            }
            zwp_text_input_v3::Request::SetTextChangeCause { cause } => {
                if let Ok(c) = cause.into_result() {
                    inst.pending.change_cause = Some(c as u32);
                }
            }
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                if let Ok(h) = hint.into_result() {
                    inst.pending.content_hint = Some(h.bits());
                }
                if let Ok(p) = purpose.into_result() {
                    inst.pending.content_purpose = Some(p as u32);
                }
            }
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
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
        let was_active = state.ime.ti3.remove_instance(&ti.id());
        if was_active {
            // 激活实例销毁 ≈ 失焦：整会话停用。
            let cmds = state.ime.relay.focus_lost();
            state.ime.execute_ime_commands(cmds);
        }
    }
}
