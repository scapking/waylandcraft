//! smithay SeatState 接入层（IME 重写 Step 1）。
//!
//! ## 目的
//!
//! waylandcraft IME 子系统当前完全基于自造 `WLCSeatState`（1671 行
//! seat.rs），与 smithay 框架脱节。要用 smithay 完整 im2 + ti3 manager
//! 框架，必须让 `WLCState` 实现 `SeatHandler` 并持有 `SeatState<WLCState>`。
//!
//! ## 约束（任务硬性要求）
//!
//! - **不删** `WLCSeatState`：现有键盘 pipeline（`bridge::keyboard_input` →
//!   `WLCSeatState::keyboard_key` → 各 `wl_keyboard.key` 客户端）已编译过
//!   且 48/48 测试通过，重构 seat.rs 风险高。
//! - **不动** `WlSeat` / `WlKeyboard` / `WlPointer` 的 dispatch：当前由
//!   `seat.rs` 内手工 `GlobalDispatch` + `Dispatch` 实现，smithay
//!   `delegate_seat!` 宏会接管这三者的全部 dispatch——和现有 dispatch
//!   必然冲突。Step 1 **不调** `delegate_seat!`，仅添加 `SeatState` 字段
//!   与 `SeatHandler` impl，让 smithay 编译过即可。
//! - **不动** `bridge::keyboard_input`：键盘事件仍走 `WLCSeatState`，新增
//!   smithay 路径作 Step 3 才需要。
//!
//! ## 与 smithay 框架的关系
//!
//! - 持有 `SeatState<WLCState>` 字段是 smithay im2 / ti3 manager 编译要求。
//! - smithay `InputMethodManagerState::new::<D, _>(disp, |c| true)` 要求 `D:
//!   SeatHandler`。Step 2 加 im2 / ti3 manager global 时需要本文件提供这个 impl。
//! - `Seat::from_resource(&wl_seat)` 当前会返回 `None`（因为我们没有调用
//!   `SeatState::new_seat`，没有给 WlSeat 设 `SeatUserData`）——这意味着
//!   Step 2 加入的 smithay im2 / ti3 manager 在客户端调用 `get_input_method`
//!   / `get_text_input` 时**拿不到** `Seat` 实例，整体不可用。
//!
//!   解决路径只有两条：
//!   (A) 调用 `SeatState::new_seat` 让 smithay 创建 WlSeat global，并把
//!       `WLCSeatState::create_globals` 的 WlSeat 干掉——`delegate_seat!`
//!       接管 dispatch。
//!   (B) 放弃 smithay im2 / ti3 manager，继续走自造 `input_method_v2.rs` /
//!       `text_input_v3.rs`（即保持 v0.9.45 现状，仅用 Step 3 把 host_bridge
//!       接到 im2 grab）。
//!
//!   路径 (A) 要求重构 seat.rs——任务明令禁止。
//!   路径 (B) 是本任务允许的最小可行路径。
//!
//!   本文件保留 `SeatState` 字段 + `SeatHandler` impl，是为了**未来可平滑升级到
//!   路径 (A)**：当用户决策放弃"不重构 seat.rs"约束时，只需要把
//!   `WLCSeatState::create_globals` 中的 `WlSeat` global 创建移走即可，
//!   业务代码（`SeatHandler::focus_changed` 等）已经准备好。
//!
//! ## Step 1 改动小结
//!
//! - 新增 `seat_smithay.rs`（本文件）—— 仅放 `SeatHandler` impl
//! - `lib.rs` 加 `smithay_seat_state: SeatState<WLCState>` 字段
//! - `WLCState::new` 初始化 `smithay_seat_state`
//! - 不动 `WLCSeatState`、不动 `bridge::keyboard_input`、不动任何 delegate

use smithay::{
    input::{
        pointer::CursorImageStatus,
        keyboard::LedState,
        SeatHandler, SeatState,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
};

use crate::WLCState;

/// `SeatHandler` impl for `WLCState`。
///
/// **仅满足 smithay trait 约束**：smithay im2 / ti3 manager 编译时要求
/// `WLCState: SeatHandler`。本 impl 不参与运行时键盘派发——所有键盘事件
/// 仍由 `WLCSeatState` 处理（见 `bridge::keyboard_input`）。
///
/// `KeyboardFocus = WlSurface`：smithay im2 / ti3 默认把 keyboard focus
/// surface 当作 text input focus surface，这是协议惯例。实际 focus 切换
/// 仍由 `WLCSeatState::keyboard_focus` 控制。
///
/// `focus_changed` / `cursor_image` / `led_state_changed` 都是默认空实现——
/// waylandcraft 不需要 smithay 在这些事件上做事（自造路径已经处理）。
impl SeatHandler for WLCState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.smithay_seat_state
    }

    fn focus_changed(
        &mut self,
        _seat: &smithay::input::Seat<Self>,
        _focused: Option<&Self::KeyboardFocus>,
    ) {
        // 不做事——WLCSeatState 在 bridge::keyboard_focus_surface 时手动 enter/leave。
        // 留作未来 smithay Seat 接管 WlSeat 后的事件钩子。
    }

    fn cursor_image(
        &mut self,
        _seat: &smithay::input::Seat<Self>,
        _image: CursorImageStatus,
    ) {
        // 不做事——cursor 由 wp_cursor_shape_device_v1 路径管理（seat.rs）。
    }

    fn led_state_changed(
        &mut self,
        _seat: &smithay::input::Seat<Self>,
        _led_state: LedState,
    ) {
        // 不做事——LED state 在 WLCSeatState::send_modifiers 序列化时由 xkb_state 提供。
    }
}