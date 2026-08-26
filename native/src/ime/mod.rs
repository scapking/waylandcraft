//! 输入法子系统门面 —— 协议全局对象注册、端点路由与统一执行层。
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
//!              ┌─────────────────────────┴────────────────────┐
//!              ▼                                              ▼
//!     InputMethodV2State（im2 wire 层，           Passthrough outbox →
//!     游戏内直连 fcitx5 等输入法客户端）            system_ime/passthrough.rs
//!                                                   （宿主桌面输入法穿透）
//! ```
//!
//! **端点策略**：同一时刻至多一个 IME 端点生效。
//! - 游戏内 im2 客户端（如直接跑在游戏合成器上的 fcitx5）拥有最高优先级；
//! - 无 im2 实例且宿主穿透就绪时，走穿透端点；
//! - 端点切换时 Relay 负责在新端点上重新激活会话（重发 Activate），
//!   旧端点的 serial 计数随下线复位。
//!
//! **数据流总览**（协议正确路径）：
//!
//! ```text
//! keyboard(Java/GLFW) → 合成器 ─(grab 时)→ 输入法端点 → preedit/commit → text-input → App
//! App 文本状态(surrounding/cursor/content) → text-input commit → Relay → 输入法端点（反向同步）
//! ```

mod input_method_v2;
mod relay;
mod text_input_v3;

#[cfg(test)]
mod tests;

pub use input_method_v2::InputMethodV2State;
pub use relay::{AppState, ImeCommand, ImeOp, Relay, TiCommand};
pub use text_input_v3::TextInputV3State;

use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::
    zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::{
    zwp_input_method_v2::ZwpInputMethodV2, zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
};
use smithay::reexports::wayland_server::{DisplayHandle, Resource};

use crate::seat::KeyboardAction;
use crate::utils::{get_time, new_serial};
use crate::WLCState;

/// 当前生效的输入法端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Endpoint {
    #[default]
    None,
    /// 游戏内 im2 客户端。
    InProcess,
    /// 宿主桌面输入法穿透。
    Passthrough,
}

/// 输入法全局状态。挂在 `WLCState.ime` 上。
#[derive(Default)]
pub struct ImeState {
    pub ti3: TextInputV3State,
    pub im2: InputMethodV2State,
    pub relay: Relay,

    /// im2 客户端是否在位（无论是否为当前端点）。
    im2_bound: bool,
    /// 宿主穿透是否就绪（由 lib.rs 在 SystemIme 初始化成功后调用 note_passthrough_ready）。
    passthrough_ready: bool,

    endpoint: Endpoint,

    /// 发往穿透端点的命令出站队列；lib.rs 每帧取走交给 SystemIme 执行。
    passthrough_outbox: Vec<ImeCommand>,
}

impl ImeState {
    pub fn create_globals(&self, disp: &DisplayHandle) {
        disp.create_global::<WLCState, ZwpTextInputManagerV3, ()>(1, ());
        disp.create_global::<WLCState, ZwpInputMethodManagerV2, ()>(1, ());
        // 注：刻意不注册 text-input-v1 / input-method-v1 —— 现代协议栈
        // （ti3 + im2）是唯一受支持路径，避免 ibus 退回 v1 造成行为分叉。
    }

    // ── 端点生命周期 ──────────────────────────────────────────

    fn recompute_endpoint(&mut self) -> Endpoint {
        if self.im2_bound {
            Endpoint::InProcess
        } else if self.passthrough_ready {
            Endpoint::Passthrough
        } else {
            Endpoint::None
        }
    }

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
            self.switch_endpoint();
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
        self.switch_endpoint();
    }

    /// 宿主穿透就绪状态变化。由 lib.rs 驱动。
    pub fn note_passthrough_ready(&mut self, ready: bool) {
        if self.passthrough_ready == ready {
            return;
        }
        self.passthrough_ready = ready;
        self.switch_endpoint();
    }

    /// 端点切换：通知 Relay 重置计数并按需向新端点补发 Activate；
    /// 旧端点若是穿透则先发 Deactivate 清场。
    fn switch_endpoint(&mut self) {
        let new_ep = self.recompute_endpoint();
        if new_ep == self.endpoint {
            // 同端点内的实例更替（如 fcitx5 重启）：让 Relay 复位计数。
            if new_ep == Endpoint::InProcess {
                let cmds = self.relay.set_ime_present(false);
                debug_assert!(cmds.is_empty());
                let cmds = self.relay.set_ime_present(true);
                self.execute_ime_commands(cmds);
            }
            return;
        }
        match self.endpoint {
            Endpoint::InProcess => {
                // 旧 im2 即将不再是端点：无需 deactivate（对象即将 unavailable/已亡）。
                let _ = self.relay.set_ime_present(false);
            }
            Endpoint::Passthrough => {
                let cmds = self.relay.set_ime_present(false);
                self.passthrough_outbox.extend(cmds);
            }
            Endpoint::None => {}
        }
        self.endpoint = new_ep;
        let present = new_ep != Endpoint::None;
        let cmds = self.relay.set_ime_present(present);
        match new_ep {
            Endpoint::Passthrough => self.passthrough_outbox.extend(cmds),
            _ => self.execute_ime_commands(cmds),
        }
    }

    // ── 焦点入口（bridge.rs keyboard_focus / keyboard_unfocus 调用）──

    /// 键盘焦点切到某 surface：更新 ti3 焦点并同步 Relay 会话状态。
    pub fn set_focus(&mut self, surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) {
        let had_focus = self.ti3.focus_surface().is_some();
        let switched = self.ti3.enter(surface);
        // 焦点在两个 surface 间直接切换（A→B 不经过空焦点）时，
        // 旧会话必须显式终结：否则 B 的 enable 会被误判为会话延续，
        // IME 收不到重新激活（对应测试场景「输入框 A → 输入框 B」）。
        if switched && had_focus && self.relay.app_active() {
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
                // 先把首批状态灌入 relay 缓存（未激活时只更新不产出命令），
                // 使随后的 Activate 单周期携带最新状态，避免多余的 done 往返。
                let _ = ime.relay.push_app_state(st);
                ime.relay.set_app_enabled(true)
            }
            O::Disabled => ime.relay.set_app_enabled(false),
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
        let Some(active_id) = self.ti3.active_id() else { return };
        let Some(inst) = self.ti3.instance_mut(&active_id) else { return };
        for cmd in commands {
            match cmd {
                TiCommand::Preedit(t, b, e) => inst.obj.preedit_string(Some(t), b, e),
                TiCommand::DeleteSurrounding(b, a) => inst.obj.delete_surrounding_text(b, a),
                TiCommand::CommitString(t) => inst.obj.commit_string(Some(t)),
                TiCommand::Done { .. } => unreachable!("relay 不产出 Done"),
            }
        }
        // done 必须跟在整批事件之后一次性发出；serial = 该实例收到的 commit 请求数。
        let count = inst.commit_count;
        inst.obj.done(count);
    }

    // ── ImeCommand 执行（relay 输出的抽象命令 → 具体端点）──

    /// 执行 relay 产出的命令序列：按当前端点分流到 im2 直发或穿透出站队列。
    pub(crate) fn execute_ime_commands(&mut self, commands: Vec<ImeCommand>) {
        for cmd in commands {
            match self.endpoint {
                Endpoint::InProcess => self.exec_on_im2(cmd),
                Endpoint::Passthrough => self.passthrough_outbox.push(cmd),
                Endpoint::None => {}
            }
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

    // ── 穿透数据通道 ──────────────────────────────────────────

    /// 宿主穿透事件入站（lib.rs 每帧从 SystemIme 取出后灌入）。
    ///
    /// 保序处理：文本操作进 Relay 缓冲，Done 触发原子应用。
    pub fn passthrough_events(&mut self, events: Vec<crate::system_ime::HostEvent>) {
        use crate::system_ime::HostEvent;
        for ev in events {
            match ev {
                HostEvent::Enter | HostEvent::Leave => {
                    // 焦点路由由 SystemIme 内部状态机消费（enable 门控），
                    // 对游戏内会话无语义影响。
                }
                HostEvent::CommitString(t) => self.relay.ime_op(ImeOp::CommitString(t)),
                HostEvent::PreeditString(t, b, e) => self.relay.ime_op(ImeOp::Preedit(t, b, e)),
                HostEvent::DeleteSurroundingText(b, a) => {
                    self.relay.ime_op(ImeOp::DeleteSurrounding(b, a))
                }
                HostEvent::Done(_) => {
                    // 宿主批次完成。serial 校验已由宿主合成器对它的客户端
                    // （即我们）完成，这里无条件应用缓冲。
                    let fr = self.relay.ime_flush();
                    if fr.applied {
                        self.emit_ti_batch(fr.commands);
                    }
                }
            }
        }
    }

    /// 取走发往穿透端点的命令（lib.rs 每帧转交 SystemIme 执行）。
    pub fn take_passthrough_outbox(&mut self) -> Vec<ImeCommand> {
        std::mem::take(&mut self.passthrough_outbox)
    }

    /// 是否有激活的文本输入会话（Java 侧驱动宿主 enable 门控）。
    pub fn app_active(&self) -> bool {
        self.relay.app_active()
    }

    /// 穿透端点是否需要接管原始按键（dbus 类宿主后端的 ProcessKeyEvent 路由）。
    /// 条件：当前端点为穿透 && 游戏内有激活文本会话。
    /// 后端自身是否就绪由驱动层结合 `system_ime` 实例状态判断（见 bridge.keyboard_input）。
    pub fn passthrough_wants_keys(&self) -> bool {
        self.endpoint == Endpoint::Passthrough && self.relay.app_active()
    }

    // ── 键盘路由（bridge.rs keyboard_input 调用）──────────────

    /// 输入法是否抓走了键盘（抓走期间原始按键只发给 IME）。
    pub fn keyboard_grabbed(&self) -> bool {
        self.im2.grab.is_some()
    }

    /// 按键转发给输入法 grab（当其存在时）。返回 true 表示已被 IME 消费。
    /// `key` 为 xkb keycode（evdev+8）；wire 侧还原为 evdev。
    pub fn handle_key(&mut self, key: u32, action: KeyboardAction, mods: (u32, u32, u32, u32)) -> bool {
        let Some(grab) = &self.im2.grab else { return false };
        let serial = new_serial();
        let wire = key.saturating_sub(8);
        grab.key(serial, get_time(), wire, action.key_state());
        grab.modifiers(serial, mods.0, mods.1, mods.2, mods.3);
        true
    }
}
