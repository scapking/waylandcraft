//! `zwp_virtual_keyboard_v1` / manager wire 层 —— 嵌套 IM 路线的引擎直通通道。
//!
//! ## 为什么必须有它
//!
//! v0.14 嵌套 daemon 路线中，fcitx5（实测本机 5.1.12 的 `libwaylandim.so`
//! 含 `zwp_input_method_manager_v2` + `zwp_virtual_keyboard_manager_v1`
//! 符号）bind 我们的 im2 manager 后：
//!
//! 1. 引擎**消费**的键 → 经 im2 `preedit/commit` 回来，走 relay 推给
//!    嵌套应用的 ti3（`input_method_v2.rs`）；
//! 2. 引擎**未消费**的键（方向键、Esc、Ctrl 快捷键、无引擎接管时的字母）
//!    → fcitx5 不丢键，而是通过 `zwp_virtual_keyboard_v1.key` **重新注入**
//!    （源码 `waylandimserverv2.cpp::sendKeyToVK/mayCommitAsText`：可打印键
//!    走 commit_string，其余走 VK）。
//!
//! 即 sync4 的 "consumed 分流" 在 im2 通道上的对偶：分流决策由引擎做出，
//! 我们的职责是把 VK 注入落回 seat 的原始按键路由——**没有 VK server，
//! im2 grab 期间未消费键会被合成器吞掉**（KWin/wlroots 都实现了该协议，
//! 缺了它任何 im2 客户端都不完整）。
//!
//! ## 语义边界
//!
//! - `keymap`：忽略——seat 的 xkb keymap 由 GLFW/宿主布局决定，是权威源；
//!   fd 由 OwnedFd drop 关闭（绝不泄漏）。
//! - `key`：`key` 为 evdev 码（与 grab 线一致），seat 侧统一用 xkb 码
//!   （evdev+8），转换后走 [`crate::seat::WLCSeatState::keyboard_key`]
//!   ——与 raw 直通同一条派发路径，保证应用看到的按键流单一。
//! - `modifiers`：忽略——seat 修饰键状态由 Java/GLFW 真键盘驱动
//!   （keyboard_update_xkb），引擎回报的修饰态若再灌入会造成双写竞争；
//!   首次到达打一条诊断日志即可。

use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::{
    zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};
use smithay::reexports::wayland_server::{
    backend::{ClientId, ObjectId},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

/// VK wire 层状态。挂在 `ImeState.vks` 上。
#[derive(Default)]
pub struct VirtualKeyboardState {
    keyboards: Vec<ZwpVirtualKeyboardV1>,
}

impl VirtualKeyboardState {
    pub(crate) fn push(&mut self, kb: ZwpVirtualKeyboardV1) {
        self.keyboards.push(kb);
    }

    pub(crate) fn remove(&mut self, id: &ObjectId) -> bool {
        let before = self.keyboards.len();
        self.keyboards.retain(|k| k.id() != *id);
        before != self.keyboards.len()
    }

    /// 已建立的虚拟键盘数（status 上报用：非零 = IME 客户端在用 VK 直通）。
    pub fn count(&self) -> usize {
        self.keyboards.len()
    }
}

impl GlobalDispatch<ZwpVirtualKeyboardManagerV1, ()> for crate::WLCState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpVirtualKeyboardManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for crate::WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpVirtualKeyboardManagerV1,
        request: zwp_virtual_keyboard_manager_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_virtual_keyboard_manager_v1::Request::CreateVirtualKeyboard { id, seat: _ } => {
                let kb: ZwpVirtualKeyboardV1 = data_init.init(id, ());
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][vk] IME 客户端创建 zwp_virtual_keyboard_v1"
                ));
                state.ime.vks.push(kb);
            }
            // 该协议 manager 接口无 destroy 请求（destructor 只在 keyboard 上）。
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for crate::WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _kb: &ZwpVirtualKeyboardV1,
        request: zwp_virtual_keyboard_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_virtual_keyboard_v1::Request::Keymap { format: _, fd: _, size: _ } => {
                // 忽略：seat keymap 是权威源（GLFW 布局）；fd 在参数 drop 时关闭。
            }
            zwp_virtual_keyboard_v1::Request::Key { time: _, key, state: ks } => {
                // ks 与 wl_keyboard key state 同编码：0=released 1=pressed。
                // KeyboardAction::Release=0 / Press=1 直接对得上。
                let Some(action) = crate::seat::KeyboardAction::from_i32(ks as i32) else {
                    return;
                };
                // VK 传 evdev 码；seat.keyboard_key 收 xkb 码（evdev+8）。
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][vk] 引擎直通 key={key} state={ks}"
                ));
                state.seat.keyboard_key(key + 8, action);
            }
            zwp_virtual_keyboard_v1::Request::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                // 修饰态不双写（真键盘路径是权威源），仅诊断可见性。
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][vk] modifiers（忽略，seat 为权威源）dep={mods_depressed} lat={mods_latched} lock={mods_locked} grp={group}"
                ));
            }
            zwp_virtual_keyboard_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        kb: &ZwpVirtualKeyboardV1,
        _data: &(),
    ) {
        state.ime.vks.remove(&kb.id());
    }
}
