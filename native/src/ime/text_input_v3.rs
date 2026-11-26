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
//! ## v0.13 简化（v0.14 部分回调）
//!
//! - 删除对 `Relay` 的依赖（v0.10 整体被删），所有裁决直接通过
//!   `ImeState::apply_ti3_outcome` 走 host_bridge；
//! - Commit / Preedit 不再"等 relay 原子应用"——host_bridge 信号线程
//!   自己攒批（`take_up_events_batched`），由 lib.rs update 灌入 ImeState；
//! - 上行 commit / preedit 直接通过对应的 ZwpTextInputV3 obj 发回 client。
//!
//! ## v0.14 嵌套 IM 路线恢复
//!
//! 嵌套 daemon（fcitx5，经 im2 接入）路线要求 relay 拿到完整的 app 侧
//! 状态做反向同步（surrounding text / content_type / change_cause 决定
//! 引擎行为，如 password 域不激活拼音），因此恢复 v0.9.27 的
//! content/change_cause 跟踪与 `active_id` 会话标记；done(serial) 仍按
//! **per-instance commit_count** 发出（协议要求，绝不共享计数）。
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
    backend::{ClientId, ObjectId},
    protocol::wl_surface::WlSurface,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use super::relay::AppState;

/// 单个 text_input 实例的待提交状态（double-buffer，随 commit 应用）。
///
/// content_hint/content_purpose/change_cause 保存协议原始编码（u32 位掩码/
/// 枚举值），由 [`AppState::from_pending`] 直接灌入 relay——嵌套 daemon
/// （im2）路线的反向同步需要完整 app 状态（例如 password 内容域下
/// fcitx5 切换为不组合行为，丢这些字段引擎上下文就不完整）。
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
    /// 当前激活（enable 且已 commit 生效）的实例 id。
    /// relay 的 emit 批次必须打到这个实例（done serial 用它的 commit_count），
    /// 不能按"聚焦 client 的第一个实例"猜——多输入框 client（每 tab 一个
    /// text_input 的 firefox）会打错对象。
    active_id: Option<ObjectId>,
}

/// [`TextInputV3State::commit_instance`] 的裁决结果。
///
/// v0.14 恢复 v0.9.27 形状：裁决携带 [`AppState`]（relay 的反向同步
/// 词汇表），host_bridge 后备路径与 im2 主路径从同一份状态派生。
pub(crate) enum Ti3CommitOutcome {
    /// commit 属于未聚焦/未知/未激活实例，忽略。
    Ignored,
    /// 实例请求启用；附带应推送给 relay/host_bridge 的状态快照。
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

    /// 当前是否有激活的文本输入会话（供 enable 门控与按键路由参考）。
    /// v0.14：以 active_id 为准（旧 v0.13 用"有 focus surface"近似，
    /// 嵌套 daemon 路线下必须区分 enter 与 enable——引擎只该给已 enable
    /// 的实例送 commit/preedit）。
    pub fn has_active(&self) -> bool {
        self.active_id.is_some()
    }

    /// 当前激活实例的对象 id（relay emit 用）。
    pub(crate) fn active_id(&self) -> Option<ObjectId> {
        self.active_id.clone()
    }

    /// v0.13.4：当前活着的 ti3 实例数（firefox 之类客户端的 zwp_text_input_v3 对象数）。
    /// status.rs 用于判断嵌套 IME 是否被启用。
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// 处理一个实例的 commit：应用 double-buffer，返回裁决结果。
    /// commit 计数无条件递增（协议按「该对象发出的请求」计数）。
    ///
    /// v0.14 恢复 v0.9.27 语义：enable 切换 `active_id`；未激活实例的
    /// disable / 普通状态提交按协议忽略（DisabledInactive / Ignored）——
    /// 嵌套 daemon 路线的 done(serial) 基准是**激活实例**的 commit 计数，
    /// 裁决错位会造成整批 commit 被引擎侧误判过期。
    pub(crate) fn commit_instance(&mut self, obj: &ZwpTextInputV3) -> Ti3CommitOutcome {
        let Some(inst) = self.instances.iter_mut().find(|i| i.obj.id() == obj.id()) else {
            return Ti3CommitOutcome::Ignored;
        };
        inst.commit_count += 1;
        let new_state = std::mem::take(&mut inst.pending);

        let snapshot = AppState::from_pending(
            new_state.surrounding_text.as_deref(),
            new_state.surrounding_cursor,
            new_state.surrounding_anchor,
            new_state.content_hint.unwrap_or(0),
            new_state.content_purpose.unwrap_or(0),
            new_state.change_cause.unwrap_or(0),
            new_state.cursor_rectangle,
        );
        match new_state.enable {
            Some(true) => {
                self.active_id = Some(obj.id());
                Ti3CommitOutcome::Enabled(snapshot)
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
                    Ti3CommitOutcome::State(snapshot)
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
                // v0.13 P1：客户端创建 ti3 实例——记录到 ImeState.ti3。
                // 创建后**不**主动 send enter：enter 由 keyboard_focus 触发，
                // 避免 surface 还没聚焦就发 enter 让 client 提前 enable。
                state.ime.ti3.push_instance(Ti3Instance {
                    obj: ti,
                    pending: Ti3Pending::default(),
                    commit_count: 0,
                });
                // 分级=里程碑（客户端接入生命周期事件，低频）：保留。
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
            // 分级=细节（ti3 逐事件：commit 每敲一键一行）→ verbose 档。
            crate::bridge::ime_log_v!(
                "[waylandcraft][ime][ti3] commit_instance obj={} -> apply_ti3_outcome",
                obj_id
            );
            let outcome = state.ime.ti3.commit_instance(ti);
            super::ImeState::apply_ti3_outcome(state, outcome);
            return;
        }

        let Some(inst) = state.ime.ti3.instance_mut(&obj_id) else {
            return;
        };
        match request {
            // 分级=细节（ti3 逐事件：enable/disable/周边文本，含用户输入
            // 内容摘录，隐私上也应只在 verbose 档落盘）→ verbose 档。
            zwp_text_input_v3::Request::Enable => {
                crate::bridge::ime_log_v!("[waylandcraft][ime][ti3] Enable obj={obj_id}");
                inst.pending.enable = Some(true);
            }
            zwp_text_input_v3::Request::Disable => {
                crate::bridge::ime_log_v!("[waylandcraft][ime][ti3] Disable obj={obj_id}");
                inst.pending.enable = Some(false);
            }
            zwp_text_input_v3::Request::SetSurroundingText { text, cursor, anchor } => {
                crate::bridge::ime_log_v!(
                    "[waylandcraft][ime][ti3] SetSurroundingText obj={obj_id} text=\"{}\" cursor={cursor} anchor={anchor}",
                    text.chars().take(16).collect::<String>()
                );
                inst.pending.surrounding_text = Some(text);
                inst.pending.surrounding_cursor = cursor as u32;
                inst.pending.surrounding_anchor = anchor as u32;
            }
            zwp_text_input_v3::Request::SetTextChangeCause { cause } => {
                // v0.14：恢复跟踪——relay 反向同步给 im2 端点时区分
                // “输入法改的文本”与“应用自己改的文本”。
                if let Ok(c) = cause.into_result() {
                    inst.pending.change_cause = Some(c as u32);
                }
            }
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                // v0.14：恢复跟踪——password/PIN 内容域下引擎行为不同，
                // 宿主/嵌套输入法端点都需要该信息。
                if let Ok(h) = hint.into_result() {
                    inst.pending.content_hint = Some(h.bits());
                }
                if let Ok(p) = purpose.into_result() {
                    inst.pending.content_purpose = Some(p as u32);
                }
            }
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                // 客户端通过 ti3 上报光标矩形——验证候选窗锚点能否取真实应用光标。
                // 分级=细节（逐光标移动一行）→ verbose 档。
                crate::bridge::ime_log_v!(
                    "[waylandcraft][ime][ti3] SetCursorRectangle obj={obj_id} rect=({x},{y},{width},{height})"
                );
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
            // 激活实例销毁 ≈ 会话终结：停用 relay（im2 端点收 deactivate）
            // + host_bridge FocusOut（v0.13 语义保留）。
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][ti3] destroyed active obj={} -> relay.focus_lost",
                ti.id()
            ));
            state.ime.note_ti3_session_gone();
        }
    }
}