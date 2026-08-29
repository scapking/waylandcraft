//! 输入法子系统门面 —— 协议全局对象注册与执行层。
//!
//! ## 模块结构与职责边界
//!
//! ```text
//! seat（键盘焦点） ──enter/leave──> TextInputV3State（ti3 wire 层）
//!                                        │ commit/enable/disable
//!                                        ▼
//!                                   Relay（纯逻辑状态机：serial 记账、
//!                                         原子缓冲、丢弃判定）   ← 本模块组装
//!                                        │ ImeCommand / TiCommand
//!                                        ▼
//!                              InputMethodV2State（im2 wire 层）
//!                              游戏内直连 fcitx5 等输入法客户端
//! ```
//!
//! **架构演进**（C 方案）：
//! - 旧 v0.9.38 路径有 host_ime 穿透（已被证明架构错误）
//! - v0.9.39 又用 hybrid async 重新做穿透（仍是 dbus 客户端模式）
//! - **当前重构** 删除所有 host_ime / system_ime / 穿透代码
//! - 嵌套应用通过自己的 GdkIMContext（GTK/Qt）直接连宿主 IME daemon
//! - mod 未来要实现 XIM server（X11 native 应用）+ im1 global（ibus-wayland）
//! - **永不模拟** IME 引擎——永远转发给宿主 daemon
//!
//! **数据流总览**（重构后协议正确路径）：
//!
//! ```text
//! keyboard(Java/GLFW) → 合成器 ─(grab 时)→ im2 客户端 → preedit/commit → text-input → App
//! App 文本状态 → text-input commit → Relay → im2 客户端（反向同步）
//! [未来] X11 应用 → XIM server → 内部 ImeEvent → 宿主 dbus-ibus
//! [未来] ibus-wayland → im1 global → 内部 ImeEvent → 宿主 dbus-ibus
//! ```

mod ime_event;
mod input_method_v2;
mod relay;
mod text_input_v3;

#[cfg(test)]
mod tests;

pub use ime_event::{
    Commit, CursorRect, DeleteSurrounding, Done, DownEvent, FocusChange, KeyEvent,
    LookupTable, PreeditUpdate, SurroundingText, UpEvent,
};
pub use input_method_v2::InputMethodV2State;
pub use relay::{AppState, ImeCommand, ImeOp, Relay, TiCommand};
pub use text_input_v3::TextInputV3State;

use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::
    zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::{
    zwp_input_method_v2::ZwpInputMethodV2,
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
};
use smithay::reexports::wayland_server::{DisplayHandle, Resource};

use crate::WLCState;
use crate::seat::KeyboardAction;
use crate::utils::{get_time, new_serial};

/// 输入法全局状态。挂在 `WLCState.ime` 上。
#[derive(Default)]
pub struct ImeState {
    pub ti3: TextInputV3State,
    pub im2: InputMethodV2State,
    pub relay: Relay,

    /// im2 客户端是否在位。
    im2_bound: bool,

    /// 最近一次候选窗快照（Java 每帧轮询，自绘候选窗用；mod 自绘是过渡方案，
    /// 未来改用桌面 IME 框架 kimpanel / ibus panel 渲染）。
    lookup_table: Option<LookupTableSnapshot>,
}

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

impl ImeState {
    /// 取走候选窗快照（Java 侧 JNI 每帧调用；无更新时返回 None）。
    pub fn take_lookup_table(&mut self) -> Option<LookupTableSnapshot> {
        self.lookup_table.take()
    }
    pub fn create_globals(&self, disp: &DisplayHandle) {
        disp.create_global::<WLCState, ZwpTextInputManagerV3, ()>(1, ());
        disp.create_global::<WLCState, ZwpInputMethodManagerV2, ()>(1, ());
        // 注：刻意不注册 text-input-v1 / input-method-v1 —— 现代协议栈
        // （ti3 + im2）是唯一受支持路径，避免 ibus 退回 v1 造成行为分叉。
        // C 方案下一阶段：实现 im1 global（ibus-wayland 兼容）+ XIM server。
    }

    // ── im2 客户端生命周期 ────────────────────────────────────
    // （旧：im2 客户端 vs 宿主穿透端点，由 recompute_endpoint 仲裁 —— 已删）

    /// im2 客户端绑定（manager.get_input_method）。顶替旧实例。
    pub(crate) fn note_im2_bound(&mut self, im: ZwpInputMethodV2) {
        if let Some(old) = self.im2.instance.as_mut() {
            if old.obj.id() == im.id() {
                return;
            }
            old.obj.unavailable();
        }
        self.im2.instance = Some(input_method_v2::Im2Instance {
            obj: im,
            done_count: 0,
        });
        let was = std::mem::replace(&mut self.im2_bound, true);
        if !was {
            // 首次绑定 im2：通知 Relay ime_present 切换
            let _ = self.relay.set_ime_present(false);
            let cmds = self.relay.set_ime_present(true);
            self.execute_ime_commands(cmds);
        }
    }

    /// im2 客户端消失（断连/销毁）。
    pub(crate) fn note_im2_gone(&mut self) {
        if !self.im2_bound && self.im2.instance.is_none() {
            return;
        }
        self.im2_bound = false;
        self.im2.instance = None;
        self.im2.grab = None;
        self.im2.popup = None;
        // 通知 Relay ime_present 离开
        let cmds = self.relay.set_ime_present(false);
        self.execute_ime_commands(cmds);
    }

    // ── 焦点入口（bridge.rs keyboard_focus / keyboard_unfocus 调用）──

    /// 键盘焦点切到某 surface：更新 ti3 焦点并同步 Relay 会话状态。
    pub fn set_focus(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        let had_focus = self.ti3.focus_surface().is_some();
        let switched = self.ti3.enter(surface);
        // 焦点在两个 surface 间直接切换（A→B 不经过空焦点）时，
        // 旧会话必须显式终结：否则 B 的 enable 会被误判为会话延续，
        // IME 收不到重新激活（对应测试场景「输入框 A → 输入框 B」）。
        if switched && had_focus && self.relay.app_active() {
            // P3 来源日志：surface 直接切换（A→B）触发 focus_lost。
            crate::bridge::ime_log_write(
                "[waylandcraft][ime][ti3] focus switched A->B -> focus_lost",
            );
            let cmds = self.relay.focus_lost();
            self.execute_ime_commands(cmds);
        }
    }

    /// 键盘焦点整体离开：leave 全部实例、停用会话、失活端点会话。
    pub fn clear_focus(&mut self) {
        if self.ti3.leave() {
            let cmds = self.relay.focus_lost();
            self.execute_ime_commands(cmds);
        }
    }

    // ── ti3 commit 裁决落地（text_input_v3.rs 的 Dispatch 调用）──

    pub(crate) fn apply_ti3_outcome(
        state: &mut WLCState,
        outcome: text_input_v3::Ti3CommitOutcome,
    ) {
        use text_input_v3::Ti3CommitOutcome as O;
        let ime = &mut state.ime;
        let cmds = match outcome {
            O::Ignored | O::DisabledInactive => Vec::new(),
            O::Enabled(st) => {
                // P3 来源日志：app_active 变 true 的驱动源。
                crate::bridge::ime_log_write(
                    "[waylandcraft][ime][ti3] outcome=Enabled -> set_app_enabled(true, ti3.enable)",
                );
                // 先把首批状态灌入 relay 缓存（未激活时只更新不产出命令），
                // 使随后的 Activate 单周期携带最新状态，避免多余的 done 往返。
                let _ = ime.relay.push_app_state(st);
                // v0.9.45 修法：同时通知 host_bridge FocusIn。
                // 没有 FocusIn，ibus 引擎收到 ProcessKeyEvent 但不处理
                // （InputContext 状态 unfocused）→ 永远不发回 commit/preedit。
                if let Some(hb) = state.host_bridge.as_mut() {
                    if hb.is_ready() {
                        hb.submit(crate::ime::DownEvent::State(
                            crate::ime::FocusChange::Activate,
                        ));
                    }
                }
                ime.relay.set_app_enabled(true, "ti3.enable")
            }
            O::Disabled => {
                // P3 来源日志：app_active 变 false 的驱动源。
                crate::bridge::ime_log_write(
                    "[waylandcraft][ime][ti3] outcome=Disabled -> set_app_enabled(false, ti3.disable)",
                );
                // v0.9.45 修法：同时通知 host_bridge FocusOut。
                if let Some(hb) = state.host_bridge.as_mut() {
                    if hb.is_ready() {
                        hb.submit(crate::ime::DownEvent::State(
                            crate::ime::FocusChange::Deactivate,
                        ));
                    }
                }
                ime.relay.set_app_enabled(false, "ti3.disable")
            }
            O::State(st) => ime.relay.push_app_state(st),
        };
        ime.execute_ime_commands(cmds);
    }

    // ── im2 commit(serial) 落地（input_method_v2.rs 的 Dispatch 调用）──

    pub(crate) fn ime_commit_from_wire(&mut self, serial: u32) {
        let fr = self.relay.ime_commit(serial);
        if fr.applied {
            self.emit_ti_batch(fr.commands);
        }
    }

    /// 把一批 TiCommand 发给当前激活的 text_input 实例，并按协议补发
    /// `done(<该实例的 commit 计数>)`。
    fn emit_ti_batch(&mut self, commands: Vec<TiCommand>) {
        if commands.is_empty() {
            return;
        }
        let Some(active_id) = self.ti3.active_id() else {
            crate::bridge::ime_log_write(
                "[waylandcraft][ime] ti3 batch DROPPED：无 active text_input 实例（App 未 enable 或未聚焦）",
            );
            return;
        };
        let Some(inst) = self.ti3.instance_mut(&active_id) else {
            return;
        };
        for cmd in commands {
            match cmd {
                TiCommand::Preedit(t, b, e) => {
                    inst.obj.preedit_string(Some(t), b, e)
                }
                TiCommand::DeleteSurrounding(b, a) => {
                    inst.obj.delete_surrounding_text(b, a)
                }
                TiCommand::CommitString(t) => inst.obj.commit_string(Some(t)),
                TiCommand::Done { .. } => unreachable!("relay 不产出 Done"),
            }
        }
        // done 必须跟在整批事件之后一次性发出；serial = 该实例收到的 commit 请求数。
        let count = inst.commit_count;
        inst.obj.done(count);
    }

    // ── ImeCommand 执行（relay 输出的抽象命令 → im2 端点）────

    /// 执行 relay 产出的命令序列：当前唯一端点是 im2 直连
    /// （C 方案成熟时还会加上 XIM server / im1 global / 宿主 dbus 桥接）。
    pub(crate) fn execute_ime_commands(&mut self, commands: Vec<ImeCommand>) {
        for cmd in commands {
            self.exec_on_im2(cmd);
        }
    }

    fn exec_on_im2(&mut self, cmd: ImeCommand) {
        match cmd {
            ImeCommand::Activate(st) => {
                if let Some(inst) = &self.im2.instance {
                    inst.obj.activate();
                }
                self.im2.push_state_events(&st);
                self.im2.send_done();
            }
            ImeCommand::Deactivate => {
                if let Some(inst) = &self.im2.instance {
                    inst.obj.deactivate();
                }
                self.im2.send_done();
            }
            ImeCommand::PushState(st) => {
                self.im2.push_state_events(&st);
                self.im2.send_done();
            }
        }
    }

    // ── 内部 IME 事件流（协议无关）──
    // C 方案：所有 IME 事件流（XIM / im2 / im1 / host_bridge）都翻译成 ImeEvent
    // 内部流（ime/ime_event.rs），由 lib.rs 每帧灌入这里。

    /// 接收 host_bridge / XIM / im1 的 UpEvent 批次，灌入 relay 并原子应用。
    ///
    /// 行为：
    /// - Preedit / Commit / DeleteSurrounding → 进 relay.ime_op 缓冲
    /// - Done → 触发 relay.ime_flush，原子推到 ti3 wire
    /// - LookupTable → 跳过（mod 不自绘候选窗；用宿主 IME 框架 kimpanel；
    ///   光标位置由 host_bridge.update_cursor_rect 在 im2 grab 缺席时
    ///   单独发给宿主 SetCursorLocationRelative）
    pub fn apply_up_events(
        &mut self,
        events: Vec<crate::ime::UpEvent>,
    ) {
        use crate::ime::{ImeOp, UpEvent};
        for ev in events {
            match ev {
                UpEvent::Preedit(p) => {
                    self.relay.ime_op(ImeOp::Preedit(p.text, p.cursor_begin, p.cursor_end));
                }
                UpEvent::Commit(c) => {
                    self.relay.ime_op(ImeOp::CommitString(c.text));
                }
                UpEvent::DeleteSurrounding(d) => {
                    self.relay.ime_op(ImeOp::DeleteSurrounding(
                        d.before_length,
                        d.after_length,
                    ));
                }
                UpEvent::LookupTable(_) => {
                    // mod 不自绘候选窗（v0.9.40+ 决策：交给宿主 IME 框架）。
                    // XIM / im1 / host_bridge 上行 LookupTable 忽略；
                    // firefox / gnome-terminal 等 GTK 应用自己处理候选窗 UI。
                }
                UpEvent::Done(_) => {
                    // 原子应用缓冲到 ti3 wire
                    let fr = self.relay.ime_flush();
                    if fr.applied {
                        crate::bridge::ime_log_write(&format!(
                            "[waylandcraft][ime] host_bridge flush applied -> ti3 batch ({} cmds)",
                            fr.commands.len()
                        ));
                        self.emit_ti_batch(fr.commands);
                    } else {
                        crate::bridge::ime_log_write(
                            "[waylandcraft][ime] host_bridge flush NOT applied（无激活会话）",
                        );
                    }
                }
            }
        }
    }

    /// 是否有激活的文本输入会话（Java 侧驱动宿主 enable 门控）。
    pub fn app_active(&self) -> bool {
        self.relay.app_active()
    }

    // ── 键盘路由（bridge.rs keyboard_input 调用）──────────────

    /// 输入法是否抓走了键盘（抓走期间原始按键只发给 IME）。
    pub fn keyboard_grabbed(&self) -> bool {
        self.im2.grab.is_some()
    }

    /// 按键转发给输入法 grab（当其存在时）。返回 true 表示已被 IME 消费。
    /// `key` 为 xkb keycode（evdev+8）；wire 侧还原为 evdev。
    pub fn handle_key(
        &mut self,
        key: u32,
        action: KeyboardAction,
        mods: (u32, u32, u32, u32),
    ) -> bool {
        let Some(grab) = &self.im2.grab else {
            return false;
        };
        let serial = new_serial();
        let wire = key.saturating_sub(8);
        grab.key(serial, get_time(), wire, action.key_state());
        grab.modifiers(serial, mods.0, mods.1, mods.2, mods.3);
        true
    }
}
