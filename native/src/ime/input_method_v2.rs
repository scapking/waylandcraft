//! `zwp_input_method_v2` / manager / keyboard_grab / popup wire 层。
//!
//! v0.14 嵌套 IM 路线恢复（自 `.deleted_v010/input_method_v2.rs` 还原）：
//! waylandcraft 作为合成器导出 `zwp_input_method_manager_v2`，让在**我们的
//! 嵌套 WAYLAND_DISPLAY 上独立拉起的 fcitx5/ibus 实例**（见
//! `nested_daemon` 模块）作为 IME 客户端 bind 进来。引擎状态归嵌套会话
//! 所有，宿主桌面会话（GNOME/mutter）的 focus 隔离不再影响 consumed 判定。
//!
//! 职责边界：协议对象管理、键盘 grab 的种子化（keymap/修饰键/repeat）、
//! popup 角色与矩形事件；语义判定全部委托 [`Relay`](super::relay::Relay)。
//!
//! 协议要点：
//! - IME 侧的 serial = 该 input_method 对象**已收到的 done 事件数**
//!   （done 事件本身无 serial 参数）。`commit(serial)` 与计数不符时，
//!   合成器照常处理但不改变对象状态——即整批丢弃，本层据此拒绝转发。
//!   计数器 per-instance：IME 重连即从 0 开始。
//! - preedit/delete/commit_string 的到达顺序必须原样保持（应用顺序由
//!   客户端按协议固定次序执行）。
//! - grab 对象没有 enter 事件；取 grab 时必须先发 keymap 再发当前修饰键。
//! - popup 是输入法候选窗 surface；合成器通过 `text_input_rectangle`
//!   事件告知文本框位置。本项目暂不把 popup 合成进游戏画面（v0.14：
//!   嵌套 daemon 的候选窗要么让引擎走 in-app preedit（ti3 高亮格式），
//!   要么后续接 popup 合成），但矩形信息按协议如实上报。

use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_v3::{ChangeCause, ContentHint, ContentPurpose},
};
use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};
use smithay::reexports::wayland_server::{
    backend::ClientId,
    protocol::wl_keyboard::KeymapFormat,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::utils::new_serial;
use std::os::fd::AsFd;
use crate::WLCState;

/// 一个 input_method 对象的完整服务端状态。
pub(crate) struct Im2Instance {
    pub obj: ZwpInputMethodV2,
    /// 已发出的 done 事件数 —— 协议规定的 commit(serial) 校验基准。
    pub done_count: u32,
}

/// im2 wire 层状态。挂在 `ImeState.im2` 上。
#[derive(Default)]
pub struct InputMethodV2State {
    pub(crate) instance: Option<Im2Instance>,
    /// 输入法抓走的键盘 grab（存在期间按键路由给 IME）。
    pub grab: Option<ZwpInputMethodKeyboardGrabV2>,
    /// 候选窗 surface（未合成渲染，仅维护协议对象与矩形上报）。
    pub(crate) popup: Option<ZwpInputPopupSurfaceV2>,
    /// 最近一次已知的 app 光标矩形。
    pub(crate) last_cursor_rect: Option<(i32, i32, i32, i32)>,
}

impl InputMethodV2State {
    /// 向实例发一个 done 并推进 per-instance 计数。
    pub(crate) fn send_done(&mut self) {
        if let Some(inst) = &mut self.instance {
            inst.obj.done();
            inst.done_count += 1;
        }
    }

    /// 把 AppState 的文本相关字段推送到 im2（不含 activate/deactivate/done）。
    pub(crate) fn push_state_events(&mut self, st: &super::relay::AppState) {
        let Some(inst) = &self.instance else { return };
        if !st.surrounding_text.is_empty() {
            inst.obj.surrounding_text(
                st.surrounding_text.clone(),
                st.surrounding_cursor,
                st.surrounding_anchor,
            );
        }
        // 只转发 app 显式设置过的字段；协议默认值不发，避免噪音批次。
        if st.change_cause != 0
            && let Some(cause) = change_cause_from_u32(st.change_cause)
        {
            inst.obj.text_change_cause(cause);
        }
        if (st.content_hint != 0 || st.content_purpose != 0)
            && let Some(purpose) = content_purpose_from_u32(st.content_purpose)
        {
            inst.obj
                .content_type(ContentHint::from_bits_retain(st.content_hint), purpose);
        }
        self.last_cursor_rect = st.cursor_rect;
        if let Some(popup) = &self.popup
            && let Some((x, y, w, h)) = st.cursor_rect
        {
            popup.text_input_rectangle(x, y, w, h);
        }
    }
}

/// 协议原始值 → 服务端枚举（text-input-v3 change_cause：0=input_method 1=other）。
fn change_cause_from_u32(v: u32) -> Option<ChangeCause> {
    match v {
        0 => Some(ChangeCause::InputMethod),
        1 => Some(ChangeCause::Other),
        _ => None,
    }
}

/// 协议原始值 → 服务端枚举（与官方 XML 的 content_purpose 表一致）。
fn content_purpose_from_u32(v: u32) -> Option<ContentPurpose> {
    let p = match v {
        0 => ContentPurpose::Normal,
        1 => ContentPurpose::Alpha,
        2 => ContentPurpose::Digits,
        3 => ContentPurpose::Number,
        4 => ContentPurpose::Phone,
        5 => ContentPurpose::Url,
        6 => ContentPurpose::Email,
        7 => ContentPurpose::Name,
        8 => ContentPurpose::Password,
        9 => ContentPurpose::Pin,
        10 => ContentPurpose::Date,
        11 => ContentPurpose::Time,
        12 => ContentPurpose::Datetime,
        13 => ContentPurpose::Terminal,
        _ => return None,
    };
    Some(p)
}

// ─────────────────────────────────────────────────────────────
// Dispatch 实现（挂在 crate::WLCState 上）
// ─────────────────────────────────────────────────────────────

impl GlobalDispatch<ZwpInputMethodManagerV2, ()> for WLCState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpInputMethodManagerV2>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpInputMethodManagerV2,
        request: zwp_input_method_manager_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_input_method_manager_v2::Request::GetInputMethod { input_method, .. } => {
                let im: ZwpInputMethodV2 = data_init.init(input_method, ());
                state.ime.note_im2_bound(im);
            }
            zwp_input_method_manager_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _im: &ZwpInputMethodV2,
        request: zwp_input_method_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use super::relay::ImeOp;
        match request {
            // 文本操作只缓冲；原子性由 commit(serial) 决定。
            zwp_input_method_v2::Request::CommitString { text } => {
                state.ime.relay.ime_op(ImeOp::CommitString(text));
            }
            zwp_input_method_v2::Request::SetPreeditString { text, cursor_begin, cursor_end } => {
                state
                    .ime
                    .relay
                    .ime_op(ImeOp::Preedit(text, cursor_begin, cursor_end));
            }
            zwp_input_method_v2::Request::DeleteSurroundingText { before_length, after_length } => {
                state
                    .ime
                    .relay
                    .ime_op(ImeOp::DeleteSurrounding(before_length, after_length));
            }
            // 应用点：serial 校验失败 = 整批丢弃（协议强制），不产生任何 app 可见变化。
            zwp_input_method_v2::Request::Commit { serial } => {
                state.ime.ime_commit_from_wire(serial);
            }
            zwp_input_method_v2::Request::GetInputPopupSurface { id, surface } => {
                let popup: ZwpInputPopupSurfaceV2 = data_init.init(id, ());
                // 如实上报最近已知矩形（尚未上报时按协议语义发 0 矩形占位）。
                let (x, y, w, h) = state
                    .ime
                    .im2
                    .last_cursor_rect
                    .unwrap_or((0, 0, 0, 0));
                popup.text_input_rectangle(x, y, w, h);
                state.ime.im2.popup = Some(popup.clone());
                // ── Phase 1 候选窗采集（纯加线，上方既有逻辑不改）──
                // 协议：surface 已有角色 → 二次赋予即协议错误。v2 XML 无
                // error 枚举，沿用 KWin im2 实现的 code=1 "Role" 约定。
                // （new_id 必须先 init 再报错，否则 wayland-server 对
                // 未初始化资源会 panic——报错后连接本就会终止。）
                if state.ime.popup.role_taken(&surface) {
                    popup.post_error(
                        1u32,
                        "surface already assigned an input_popup role",
                    );
                    return;
                }
                state.ime.register_popup_role(&popup, &surface);
            }
            zwp_input_method_v2::Request::GrabKeyboard { keyboard } => {
                let grab: ZwpInputMethodKeyboardGrabV2 = data_init.init(keyboard, ());
                {
                    let keymap = &state.seat.keymap_file;
                    grab.keymap(KeymapFormat::XkbV1, keymap.as_fd(), keymap.size() as u32);
                }
                let mods = state.seat.modifiers_tuple();
                grab.modifiers(new_serial(), mods.0, mods.1, mods.2, mods.3);
                grab.repeat_info(25, 600);
                state.ime.im2.grab = Some(grab);
            }
            zwp_input_method_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, _im: &ZwpInputMethodV2, _data: &()) {
        // Phase 3：input_method 销毁级联销毁 popup（协议 destroy 语义）→
        // popup surface 若被 pointer 聚焦过，先补 leave 再清理采集轨道。
        if let Some(surf) = state.ime.popup_surface() {
            state.seat.popup_pointer_leave(&surf);
        }
        state.ime.note_im2_gone();
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for WLCState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _grab: &ZwpInputMethodKeyboardGrabV2,
        request: zwp_input_method_keyboard_grab_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_input_method_keyboard_grab_v2::Request::Release => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, _grab: &ZwpInputMethodKeyboardGrabV2, _data: &()) {
        state.ime.im2.grab = None;
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for WLCState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _popup: &ZwpInputPopupSurfaceV2,
        request: zwp_input_popup_surface_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_input_popup_surface_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        popup: &ZwpInputPopupSurfaceV2,
        _data: &(),
    ) {
        if state.ime.im2.popup.as_ref().is_some_and(|p| p.id() == popup.id()) {
            state.ime.im2.popup = None;
        }
        // Phase 3：销毁的是当前 popup 且其 surface 尚存活 → 对聚焦过它的
        // pointer 补 leave（fcitx5 WaylandPointer 的 pointerFocus_ 一致性）。
        if let Some(surf) = state.ime.popup_pointer_target(popup.id()) {
            state.seat.popup_pointer_leave(&surf);
        }
        // Phase 1 候选窗采集轨道同步解除（仅当销毁的是当前登记对象）。
        state.ime.clear_popup_role(popup.id());
    }
}
