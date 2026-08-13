//! 输入法协议 —— 手写实现，零改动现有键盘路径。
//!
//! 支持的协议（全部服务端绑定来自 smithay 的 reexport）：
//!   - `zwp_text_input_v3` / `zwp_text_input_manager_v3`（现代文本输入，GTK/Qt 用）
//!   - `zwp_input_method_v2` / `zwp_input_method_manager_v2`（现代输入法，ibus 用）
//!   - `zwp_text_input_v1` / `zwp_text_input_manager_v1`（旧版文本输入，已废弃）
//!   - `zwp_input_method_v1`（旧版输入法，已废弃）
//!
//! 数据流（v3 + v2，ibus 实际走这条路）：
//!   App(text-input-v3) ──enable/set_*──> 合成器 ──activate/surrounding_text/...──> ibus(v2)
//!   App(text-input-v3) <─commit_string/preedit/delete/ done─ 合成器 <──commit_string/...── ibus(v2)
//!   Java 按键 ──> seat ──(IME 已 grab 键盘)──> ibus(v2) 的 keyboard_grab ──> ibus 翻译 ──> commit 回 App
//!
//! 焦点跟随 seat 的 keyboard focus：`WLCSeatState::keyboard_focus` 变化时，
//! bridge 会调用 [`ImeState::set_focus`] / [`ImeState::clear_focus`]。

use crate::seat::KeyboardAction;
use crate::utils::{get_time, new_serial};
use crate::WLCState;
use smithay::reexports::wayland_protocols::wp::{
    input_method::zv1::server::{
        zwp_input_method_context_v1::{self, ZwpInputMethodContextV1},
        zwp_input_method_v1::{self, ZwpInputMethodV1},
    },
    text_input::zv1::server::{
        zwp_text_input_manager_v1::{self, ZwpTextInputManagerV1},
        zwp_text_input_v1::{self, ZwpTextInputV1},
    },
    text_input::zv3::server::{
        zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
        zwp_text_input_v3::{self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3},
    },
};
use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};
use smithay::reexports::wayland_server::{
    backend::{ClientId, ObjectId},
    protocol::{wl_keyboard::KeymapFormat, wl_surface::WlSurface},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use std::collections::HashMap;
use std::os::fd::AsFd;

/// v3 文本输入在 commit 前累积的待提交状态（double-buffer 语义）。
#[derive(Default)]
struct Ti3Pending {
    enable: Option<bool>,
    surrounding_text: Option<(String, u32, u32)>,
    content_type: Option<(ContentHint, ContentPurpose)>,
    cursor_rectangle: Option<(i32, i32, i32, i32)>,
    text_change_cause: Option<ChangeCause>,
}

/// 输入法全局状态。所有协议实例都挂在 `WLCState.ime` 上。
#[derive(Default)]
pub struct ImeState {
    // ── text-input-v3 ──
    ti3_instances: Vec<ZwpTextInputV3>,
    ti3_pending: HashMap<ObjectId, Ti3Pending>,
    ti3_focus: Option<WlSurface>,
    ti3_active: Option<ZwpTextInputV3>,
    ti3_serial: u32,

    // ── input-method-v2 ──
    im2: Option<ZwpInputMethodV2>,
    im2_grab: Option<ZwpInputMethodKeyboardGrabV2>,
    im2_serial: u32,

    // ── text-input-v1（旧版，功能性子集） ──
    ti1_instances: Vec<ZwpTextInputV1>,
    ti1_active: Option<ZwpTextInputV1>,
    ti1_serial: u32,

    // ── input-method-v1（旧版，功能性子集） ──
    im1: Option<ZwpInputMethodV1>,
    im1_context: Option<ZwpInputMethodContextV1>,
}

impl ImeState {
    pub fn create_globals(&self, disp: &DisplayHandle) {
        disp.create_global::<WLCState, ZwpTextInputManagerV3, ()>(1, ());
        disp.create_global::<WLCState, ZwpInputMethodManagerV2, ()>(1, ());
        disp.create_global::<WLCState, ZwpTextInputManagerV1, ()>(1, ());
        disp.create_global::<WLCState, ZwpInputMethodV1, ()>(1, ());
    }

    /// 焦点切到某个 surface：向该 client 的所有 v3/v1 文本输入发 enter。
    /// 由 bridge 在 seat.keyboard_focus 之后调用。
    pub fn set_focus(&mut self, surface: &WlSurface) {
        let client = match surface.client() {
            Some(c) => c,
            None => return,
        };

        // v3
        let old = self.ti3_focus.replace(surface.clone());
        let mut changed = false;
        if old.as_ref() != Some(surface) {
            self.ti3_focus = Some(surface.clone());
            changed = true;
        }
        if changed {
            let enter_list: Vec<ZwpTextInputV3> = self
                .ti3_instances
                .iter()
                .filter(|ti| ti.client().as_ref() == Some(&client))
                .cloned()
                .collect();
            for ti in enter_list {
                ti.enter(surface);
            }
        }

        // v1
        let enter_list: Vec<ZwpTextInputV1> = self
            .ti1_instances
            .iter()
            .filter(|ti| ti.client().as_ref() == Some(&client))
            .cloned()
            .collect();
        for ti in enter_list {
            ti.enter(surface);
        }
    }

    /// 焦点离开（全部 surface 都失焦）：发 leave，失活输入法。
    /// 由 bridge 在 seat.keyboard_unfocus 之后调用。
    pub fn clear_focus(&mut self) {
        if let Some(focus) = self.ti3_focus.take() {
            let client = focus.client();
            let leave_list: Vec<ZwpTextInputV3> = self
                .ti3_instances
                .iter()
                .filter(|ti| ti.client() == client)
                .cloned()
                .collect();
            for ti in leave_list {
                ti.leave(&focus);
            }
        }
        self.ti3_active = None;
        if let Some(im) = &self.im2 {
            im.deactivate();
            im.done();
        }

        // v1
        if let Some(ti) = self.ti1_active.take() {
            ti.leave();
        }
        if let Some(ctx) = &self.im1_context {
            self.im1.as_ref().map(|im| im.deactivate(ctx));
        }
    }

    /// 输入法是否已 grab 键盘。bridge 用它决定按键是发给 IME 还是普通客户端。
    pub fn keyboard_grabbed(&self) -> bool {
        self.im2_grab.is_some()
    }

    /// 按键转发给输入法（当其已 grab 键盘时）。返回 true 表示已由 IME 消费。
    /// `key` 是 xkb keycode（evdev+8，与 seat.keyboard_key 入参一致）。
    pub fn handle_key(
        &mut self,
        key: u32,
        action: KeyboardAction,
        mods: (u32, u32, u32, u32),
    ) -> bool {
        let grab = match &self.im2_grab {
            Some(g) => g.clone(),
            None => return false,
        };
        let serial = new_serial();
        let wire = key.saturating_sub(8);
        grab.key(serial, get_time(), wire, action.key_state());
        grab.modifiers(serial, mods.0, mods.1, mods.2, mods.3);
        true
    }

    // ── text-input-v3 内部逻辑 ──

    fn ti3_commit(&mut self, ti: &ZwpTextInputV3) {
        // 每次 commit 都递增 text-input serial（与 smithay 一致，丢弃也递增）
        self.ti3_serial += 1;
        let pending = self.ti3_pending.remove(&ti.id()).unwrap_or_default();

        // 只接受当前聚焦 client 的 text-input
        let focused_client = match self.ti3_focus.as_ref().and_then(|f| f.client()) {
            Some(c) => c,
            None => return,
        };
        if ti.client().as_ref() != Some(&focused_client) {
            return;
        }

        match pending.enable {
            Some(true) => {
                self.ti3_active = Some(ti.clone());
                if let Some(im) = &self.im2 {
                    im.activate();
                }
            }
            Some(false) => {
                if self.ti3_active.as_ref() == Some(ti) {
                    self.ti3_active = None;
                    if let Some(im) = &self.im2 {
                        im.deactivate();
                        im.done();
                    }
                }
                return;
            }
            None => {
                // 未显式 enable 的 commit 必须发生在已 active 的 text-input 上
                if self.ti3_active.as_ref() != Some(ti) {
                    return;
                }
            }
        }

        let im = match &self.im2 {
            Some(im) => im,
            None => return,
        };
        if let Some((text, cursor, anchor)) = pending.surrounding_text {
            im.surrounding_text(text, cursor, anchor);
        }
        if let Some(cause) = pending.text_change_cause {
            im.text_change_cause(cause);
        }
        if let Some((hint, purpose)) = pending.content_type {
            im.content_type(hint, purpose);
        }
        // cursor_rectangle 用于定位输入法候选窗，ibus 桌面版可忽略；后续接 popup 再处理。
        im.done();
        self.im2_serial += 1;
    }
}

// ─────────────────────────────────────────────────────────────
// text-input-v3：manager + text_input
// ─────────────────────────────────────────────────────────────

impl GlobalDispatch<ZwpTextInputManagerV3, ()> for WLCState {
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

impl Dispatch<ZwpTextInputManagerV3, ()> for WLCState {
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
                state
                    .ime
                    .ti3_pending
                    .insert(ti.id(), Ti3Pending::default());
                state.ime.ti3_instances.push(ti);
            }
            zwp_text_input_manager_v3::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        ti: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let pending = state
            .ime
            .ti3_pending
            .entry(ti.id())
            .or_insert_with(Ti3Pending::default);

        match request {
            zwp_text_input_v3::Request::Enable => pending.enable = Some(true),
            zwp_text_input_v3::Request::Disable => pending.enable = Some(false),
            zwp_text_input_v3::Request::SetSurroundingText { text, cursor, anchor } => {
                pending.surrounding_text = Some((text, cursor as u32, anchor as u32));
            }
            zwp_text_input_v3::Request::SetTextChangeCause { cause } => {
                if let Ok(c) = cause.into_result() {
                    pending.text_change_cause = Some(c);
                }
            }
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                if let (Ok(h), Ok(p)) = (hint.into_result(), purpose.into_result()) {
                    pending.content_type = Some((h, p));
                }
            }
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                pending.cursor_rectangle = Some((x, y, width, height));
            }
            zwp_text_input_v3::Request::Commit => {
                state.ime.ti3_commit(ti);
            }
            zwp_text_input_v3::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, ti: &ZwpTextInputV3, _data: &()) {
        let id = ti.id();
        state.ime.ti3_instances.retain(|t| t.id() != id);
        state.ime.ti3_pending.remove(&id);
        if state.ime.ti3_active.as_ref().map(|t| t.id()) == Some(id) {
            state.ime.ti3_active = None;
        }
    }
}

// ─────────────────────────────────────────────────────────────
// input-method-v2：manager + input_method + keyboard_grab + popup
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
                // 新的 IME 实例顶替旧的
                if let Some(old) = state.ime.im2.replace(im) {
                    old.unavailable();
                }
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
        match request {
            zwp_input_method_v2::Request::CommitString { text } => {
                if let Some(ti) = &state.ime.ti3_active {
                    ti.commit_string(Some(text));
                }
            }
            zwp_input_method_v2::Request::SetPreeditString { text, cursor_begin, cursor_end } => {
                if let Some(ti) = &state.ime.ti3_active {
                    ti.preedit_string(Some(text), cursor_begin, cursor_end);
                }
            }
            zwp_input_method_v2::Request::DeleteSurroundingText { before_length, after_length } => {
                if let Some(ti) = &state.ime.ti3_active {
                    ti.delete_surrounding_text(before_length, after_length);
                }
            }
            zwp_input_method_v2::Request::Commit { serial } => {
                // serial 对不上说明 text-input 状态过期，discard；否则回 done。
                let discard = state.ime.im2_serial != serial;
                if let Some(ti) = &state.ime.ti3_active {
                    if discard {
                        ti.done(0);
                    } else {
                        ti.done(state.ime.ti3_serial);
                    }
                }
            }
            zwp_input_method_v2::Request::GetInputPopupSurface { id, surface } => {
                // 输入法候选窗 / 屏上键盘。先只给 surface 一个角色并回默认矩形，
                // 后续需要时再接 compositor 的 popup 定位。
                let popup: ZwpInputPopupSurfaceV2 = data_init.init(id, ());
                let _ = surface;
                popup.text_input_rectangle(0, 0, 0, 0);
            }
            zwp_input_method_v2::Request::GrabKeyboard { keyboard } => {
                let grab: ZwpInputMethodKeyboardGrabV2 = data_init.init(keyboard, ());
                // 发送 keymap
                {
                    let keymap = &state.seat.keymap_file;
                    grab.keymap(KeymapFormat::XkbV1, keymap.as_fd(), keymap.size() as u32);
                }
                // 发送当前修饰键
                let mods = state.seat.modifiers_tuple();
                let serial = new_serial();
                grab.modifiers(serial, mods.0, mods.1, mods.2, mods.3);
                grab.repeat_info(0, 0);
                state.ime.im2_grab = Some(grab);
            }
            zwp_input_method_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, _im: &ZwpInputMethodV2, _data: &()) {
        state.ime.im2 = None;
        state.ime.im2_grab = None;
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

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _grab: &ZwpInputMethodKeyboardGrabV2,
        _data: &(),
    ) {
        state.ime.im2_grab = None;
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
}

// ─────────────────────────────────────────────────────────────
// text-input-v1（旧版，功能性子集：enter/leave + commit/preedit 转发）
// ─────────────────────────────────────────────────────────────

impl GlobalDispatch<ZwpTextInputManagerV1, ()> for WLCState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpTextInputManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpTextInputManagerV1, ()> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpTextInputManagerV1,
        request: zwp_text_input_manager_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_text_input_manager_v1::Request::CreateTextInput { id } => {
                let ti: ZwpTextInputV1 = data_init.init(id, ());
                state.ime.ti1_instances.push(ti);
            }
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpTextInputV1, ()> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        ti: &ZwpTextInputV1,
        request: zwp_text_input_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_text_input_v1::Request::Activate { surface, .. } => {
                state.ime.ti1_active = Some(ti.clone());
                ti.enter(&surface);
                // 有 v1 输入法就激活它
                if let (Some(im), None) = (&state.ime.im1, &state.ime.im1_context) {
                    // 创建 context 由 IME 在 activate 事件里拿 new_id 完成
                    let _ = im;
                }
            }
            zwp_text_input_v1::Request::Deactivate { .. } => {
                if state.ime.ti1_active.as_ref() == Some(ti) {
                    state.ime.ti1_active = None;
                }
                ti.leave();
            }
            zwp_text_input_v1::Request::Reset => {}
            zwp_text_input_v1::Request::SetSurroundingText { .. } => {}
            zwp_text_input_v1::Request::SetContentType { .. } => {}
            zwp_text_input_v1::Request::SetCursorRectangle { .. } => {}
            zwp_text_input_v1::Request::SetPreferredLanguage { .. } => {}
            zwp_text_input_v1::Request::CommitState { serial } => {
                state.ime.ti1_serial = serial;
                // 通知 v1 输入法状态已提交
                if let Some(ctx) = &state.ime.im1_context {
                    ctx.commit_state(serial);
                }
            }
            zwp_text_input_v1::Request::InvokeAction { .. } => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, ti: &ZwpTextInputV1, _data: &()) {
        let id = ti.id();
        state.ime.ti1_instances.retain(|t| t.id() != id);
        if state.ime.ti1_active.as_ref().map(|t| t.id()) == Some(id) {
            state.ime.ti1_active = None;
        }
    }
}

// ─────────────────────────────────────────────────────────────
// input-method-v1（旧版，功能性子集）
// ─────────────────────────────────────────────────────────────

impl GlobalDispatch<ZwpInputMethodV1, ()> for WLCState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpInputMethodV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpInputMethodV1, ()> for WLCState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _im: &ZwpInputMethodV1,
        request: zwp_input_method_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // zwp_input_method_v1 只有事件（activate/deactivate），无请求
        match request {
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpInputMethodContextV1, ()> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _ctx: &ZwpInputMethodContextV1,
        request: zwp_input_method_context_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_input_method_context_v1::Request::CommitString { serial, text } => {
                if let Some(ti) = &state.ime.ti1_active {
                    ti.commit_string(serial, text);
                }
            }
            zwp_input_method_context_v1::Request::PreeditString { serial, text, commit } => {
                if let Some(ti) = &state.ime.ti1_active {
                    ti.preedit_string(serial, text, commit);
                }
            }
            zwp_input_method_context_v1::Request::PreeditStyling { .. } => {}
            zwp_input_method_context_v1::Request::PreeditCursor { .. } => {}
            zwp_input_method_context_v1::Request::DeleteSurroundingText { .. } => {}
            zwp_input_method_context_v1::Request::CursorPosition { .. } => {}
            zwp_input_method_context_v1::Request::ModifiersMap { .. } => {}
            zwp_input_method_context_v1::Request::Keysym { .. } => {}
            zwp_input_method_context_v1::Request::GrabKeyboard { .. } => {}
            zwp_input_method_context_v1::Request::Key { .. } => {}
            zwp_input_method_context_v1::Request::Modifiers { .. } => {}
            zwp_input_method_context_v1::Request::Language { .. } => {}
            zwp_input_method_context_v1::Request::TextDirection { .. } => {}
            zwp_input_method_context_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _ctx: &ZwpInputMethodContextV1,
        _data: &(),
    ) {
        state.ime.im1_context = None;
    }
}
