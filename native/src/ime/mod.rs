//! 输入法子系统门面 —— v0.14 嵌套 IM 守护进程路线重建。
//!
//! ## 战略（路线演进全史）
//!
//! - v0.9.27 家族：自造 im2 + ti3 + Relay，wire 级集成测试 22 个全绿。
//! - v0.10：**全删**（与 smithay 完整 im2 框架 E0119 冲突 + 转向
//!   dbus-ibus host_bridge 穿透），只留薄 facade + ImeEvent。
//! - v0.13：重建 ti3 server（不重建 im2——让嵌套应用至少能收 commit）。
//! - **实测天花板（IME_RESEARCH_CONCLUSIONS.md）**：host dbus-ibus 路线
//!   每键 ProcessKeyEvent 恒 consumed=false——宿主引擎 focus 归宿主会话
//!   管理（GNOME/mutter 隔离），中文永远打不出。
//! - v0.14（本文件）：恢复 im2 + relay 路线，但端点换成**嵌套 daemon**：
//!   在**我们自己的 WAYLAND_DISPLAY** 上拉起独立 fcitx5/ibus 实例
//!   （`nested_daemon`），引擎状态完全归嵌套会话所有 → 宿主 focus 隔离
//!   问题从根上消失。host_bridge 保留为**可选后备路径**（consumed 分流
//!   形状不变），daemon 不可用时行为回退到 v0.13。
//!
//! ## 模块结构
//!
//! ```text
//! seat（键盘焦点） ──enter/leave──> TextInputV3State（ti3 wire 层，嵌套应用）
//!                                        │ commit/enable/disable → AppState
//!                                        ▼
//!                                   Relay（纯逻辑状态机：serial 记账、
//!                                        │  原子缓冲、丢弃判定）   ← 本模块组装
//!                                        │ ImeCommand / TiCommand
//!                     ┌──────────────────┼───────────────────┐
//!                     ▼                  ▼                   ▼
//!        InputMethodV2State      VirtualKeyboardState   host_bridge（dbus，
//!        （im2 wire 层：嵌套     （引擎未消费键的        后备路径，可选）
//!          fcitx5 bind 进来，    直通回注通道）
//!          grab→forward、
//!          commit/preedit 上行）
//! ```
//!
//! ## 按键路由（sync4 consumed 分流形状，后端换成 im2 通道）
//!
//! ```text
//! Java keyboardInput
//!   ├─ im2 grab 在场（引擎已激活）→ 键只送 grab；引擎决定命运：
//!   │     消费 → preedit/commit 经 relay 原子推给激活 ti3 实例
//!   │     未消费 → fcitx5 自己经 zwp_virtual_keyboard_v1 重新注入
//!   │               （可打印键走 commit_string，其余走 VK）→ 本模块
//!   │               Dispatch 落回 seat 原始路由
//!   ├─ 无 grab 且 nested 主路径在线 → seat raw 直通（英文/符号零开销）
//!   └─ nested 未激活（daemon 没起来/mode=off）→ v0.13 原路：
//!         host_bridge submit_key_sync consumed 分流 → raw 兜底
//! ```

mod ime_event;
mod input_method_v2;
mod relay;
mod text_input_v3;
mod virtual_keyboard;

pub use ime_event::{
    Commit, CursorRect, DeleteSurrounding, Done, DownEvent, FocusChange, KeyEvent,
    LookupTable, PreeditUpdate, SurroundingText, UpEvent,
};
pub use input_method_v2::InputMethodV2State;
pub use relay::{AppState, ImeCommand, ImeOp, Relay, TiCommand};
pub use text_input_v3::TextInputV3State;
use text_input_v3::Ti3CommitOutcome;
pub use virtual_keyboard::VirtualKeyboardState;

use crate::seat::KeyboardAction;
use crate::utils::{get_time, new_serial};
use smithay::reexports::wayland_server::Resource;

/// 候选窗快照（输入法归一化数据，与协议后端无关）。
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
/// v0.14：relay + im2 + vk wire 层重建；ti3 wire 层保留；host_bridge
/// （dbus 穿透）降级为 apply_up_events / set_focus 里的后备通路，
/// 不再是唯一裁决出口。
#[derive(Default)]
pub struct ImeState {
    /// 状态机（v0.12.0 第 12 章）：DISCONNECTED → CONNECTED → FOCUSED →
    /// IDLE → COMPOSING → CANDIDATE → COMMITTING → IDLE。
    /// 任何错误 → ERROR → RECOVERING → CONNECTED。
    state: ImeStateMachine,

    /// 键盘焦点是否在某个文本输入 surface 上。
    /// 由 `set_focus` / `clear_focus` 维护。
    /// v0.13.4 改为 pub(crate) 让 status.rs 读取。
    pub(crate) has_focus: bool,

    /// ⑧ press 时被 raw 直通（引擎未消费）的按键集合（scancode）。
    /// release 必须与 press 同路。由 bridge::keyboard_input 维护。
    pub(crate) raw_keys: std::collections::HashSet<u32>,

    /// 最近一次候选窗快照（Java 每帧轮询）。
    lookup_table: Option<LookupTableSnapshot>,

    /// ti3 wire 层状态（嵌套应用的 zwp_text_input_v3 实例）。
    pub(crate) ti3: TextInputV3State,

    /// im2 wire 层状态（嵌套 fcitx5 daemon 的 input_method 对象 + grab）。
    pub(crate) im2: InputMethodV2State,

    /// zwp_virtual_keyboard_v1 实例（引擎未消费键的直通回注通道）。
    pub(crate) vks: VirtualKeyboardState,

    /// 协议中继状态机：serial 记账、原子缓冲、丢弃判定。
    pub(crate) relay: Relay,

    /// im2 客户端对象是否在位（bind 过 get_input_method 且未销毁）。
    im2_bound: bool,
}

impl ImeState {
    /// 取走候选窗快照（Java 侧 JNI 每帧调用；无更新时返回 None）。
    pub fn take_lookup_table(&mut self) -> Option<LookupTableSnapshot> {
        self.lookup_table.take()
    }

    /// 设置候选窗快照（host_bridge 上行 LookupTable 时缓存，Java 拉取）。
    pub(crate) fn set_lookup_table(&mut self, snap: LookupTableSnapshot) {
        self.lookup_table = Some(snap);
    }

    /// 是否有激活的文本输入会话（relay 是唯一事实源，ti3 enable 驱动）。
    pub fn app_active(&self) -> bool {
        self.relay.app_active()
    }

    /// v0.13.4：当前是否有 ti3 / keyboard 焦点（status.rs 用）。
    pub fn has_focus(&self) -> bool {
        self.has_focus
    }

    /// im2 grab 是否抓走了键盘（抓走期间原始按键只发给 IME）。
    pub fn keyboard_grabbed(&self) -> bool {
        self.im2.grab.is_some()
    }

    /// 嵌套 IM 主路径是否在线：im2 客户端 bind 且在位。
    /// bridge.rs keyboard_input 用它决定「键送 im2 通道」还是「回退
    /// host_bridge/raw」。grab 是否在场由 handle_key 内部再判。
    pub fn nested_route_live(&self) -> bool {
        self.im2_bound && self.relay.ime_present()
    }

    /// 创建 protocol globals。
    ///
    /// v0.14 注册三件套：
    /// - `zwp_text_input_manager_v3`（v0.13 起，嵌套应用接入面）
    /// - `zwp_input_method_manager_v2`（**恢复**：嵌套 daemon 的 IME 接入面）
    /// - `zwp_virtual_keyboard_manager_v1`（**新增**：引擎未消费键的直通
    ///   回注——fcitx5 waylandim v2 实测依赖，缺它 grab 期间方向键/快捷键全丢）
    ///
    /// 注意：global 恒导出（bind 本身无副作用）；路由开关在
    /// `nested_daemon::NestedIme` 的 mode/phase 上裁决——mode=off 时
    /// relay 永不见 im2 端点上线，按键走 v0.13 原路。
    pub fn create_globals(&self, disp: &smithay::reexports::wayland_server::DisplayHandle) {
        use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
        use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
        use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
        use smithay::reexports::wayland_server::GlobalDispatch;
        disp.create_global::<crate::WLCState, ZwpTextInputManagerV3, ()>(1, ());
        disp.create_global::<crate::WLCState, ZwpInputMethodManagerV2, ()>(1, ());
        disp.create_global::<crate::WLCState, ZwpVirtualKeyboardManagerV1, ()>(1, ());
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime] globals 已注册: ti3 + im2 manager + virtual_keyboard manager（v0.14 嵌套 IM 路线）"
        ));
    }

    // ── 焦点入口（bridge.rs keyboard_focus / keyboard_unfocus 调用）──

    /// 键盘焦点切到某 surface：ti3 enter + relay 会话同步 + host_bridge
    /// FocusIn（后备路径保持热）。
    pub fn set_focus(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        state: &mut crate::WLCState,
    ) {
        self.has_focus = true;
        let had_focus = self.ti3.focus_surface().is_some();
        // 1. ti3 wire 层：enter 该 surface（协议要求：enter 必须发给聚焦
        // client 的全部 ti3 实例）。
        let switched = self.ti3.enter(surface);
        if switched {
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][ti3] keyboard focus switched (enter new surface)"
            ));
        }
        // 2. 焦点在两个 surface 间直接切换（A→B 不经过空焦点）时，旧会话
        // 必须显式终结：否则 B 的 enable 被误判为「会话仍在进行」，
        // 嵌套 daemon 收不到重新激活（对应 wire 测试 A→B 场景）。
        if switched && had_focus && self.relay.app_active() {
            crate::bridge::ime_log_write(
                "[waylandcraft][ime][relay] set_focus A->B 切换 -> focus_lost",
            );
            let cmds = self.relay.focus_lost();
            self.execute_ime_commands(cmds);
        }
        // 3. host_bridge FocusIn（后备路径）：v0.13 语义原样保留——
        // daemon 中途挂掉回退时宿主 ibus 侧焦点状态是热的。
        // v0.13 修正：firefox enable 后不调 commit，所以 focus 切换即发
        // FocusIn（apply_ti3_outcome 仍会再发一次，submit 幂等）。
        if let Some(hb) = state.host_bridge.as_mut() {
            if hb.is_ready() {
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][host_bridge] FocusIn（set_focus 路径，switched={}）",
                    switched
                ));
                hb.submit(DownEvent::State(FocusChange::Activate));
            }
        }
    }

    /// 键盘焦点整体离开（bridge.rs keyboard_unfocus 调用）。
    pub fn clear_focus(&mut self, state: &mut crate::WLCState) {
        // ti3 wire 层：leave（向聚焦 client 的全部 ti3 实例发 leave 事件）。
        // leave 同时清 active_id（TextInputV3State::leave 内部维护）。
        let was_focused = self.ti3.leave();
        if was_focused {
            // relay：真失焦 → 清缓冲 + Deactivate（v0.9.44 语义：只有
            // focus_lost 才清 pending_ops，ti3.disable 抖动保留）。
            let cmds = self.relay.focus_lost();
            self.execute_ime_commands(cmds);
            // host_bridge FocusOut（后备路径）。
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
        self.relay.focus_lost();
    }

    /// 激活的 ti3 实例被销毁（text_input_v3.rs Dispatch 调用）：
    /// 等价失焦——relay 停用会话、host_bridge FocusOut。
    pub(crate) fn note_ti3_session_gone(&mut self) {
        let cmds = self.relay.focus_lost();
        self.execute_ime_commands(cmds);
    }

    // ── im2 客户端生命周期（input_method_v2.rs Dispatch 调用）──

    /// im2 客户端绑定（manager.get_input_method）。顶替旧实例。
    pub(crate) fn note_im2_bound(
        &mut self,
        im: smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
    ) {
        if let Some(old) = self.im2.instance.as_ref() {
            if old.obj.id() == im.id() {
                return;
            }
            // 协议：同时只应有一个 input_method 对象；顶替旧的并通知不可用。
            old.obj.unavailable();
        }
        self.im2.instance = Some(input_method_v2::Im2Instance { obj: im, done_count: 0 });
        let was = std::mem::replace(&mut self.im2_bound, true);
        crate::bridge::ime_log_write(
            "[waylandcraft][ime][im2] 输入法客户端 bind（嵌套 daemon 已连上）",
        );
        if !was {
            // 首次绑定 im2：通知 Relay ime_present 切换。
            let _ = self.relay.set_ime_present(false);
            let cmds = self.relay.set_ime_present(true);
            self.execute_ime_commands(cmds);
            self.state.transition(ImeStateMachine::Connected);
        }
    }

    /// im2 客户端消失（断连/销毁）。嵌套主路径下线，回退由 bridge.rs 的
    /// `nested_route_live()` 自动完成（下一次按键即走 host_bridge/raw）。
    pub(crate) fn note_im2_gone(&mut self) {
        if !self.im2_bound && self.im2.instance.is_none() {
            return;
        }
        self.im2_bound = false;
        self.im2.instance = None;
        self.im2.grab = None;
        self.im2.popup = None;
        crate::bridge::ime_log_write(
            "[waylandcraft][ime][im2] 输入法客户端断开 → 回退 host_bridge/raw",
        );
        crate::status::note_error(
            "warn",
            "[ime][nested] im2 客户端断开（daemon 掉线），回退 host_bridge/raw",
        );
        let cmds = self.relay.set_ime_present(false);
        self.execute_ime_commands(cmds);
    }

    // ── ti3 commit 裁决落地（text_input_v3.rs 的 Dispatch 调用）──

    /// ti3 commit 的两条出口都喂：relay（嵌套 im2 主路径）与 host_bridge
    /// （dbus 后备路径）。host 侧 submit 是幂等的，双发无副作用。
    pub(crate) fn apply_ti3_outcome(state: &mut crate::WLCState, outcome: Ti3CommitOutcome) {
        let ime = &mut state.ime;
        let hb = state.host_bridge.as_mut();

        let cmds = match outcome {
            Ti3CommitOutcome::Ignored | Ti3CommitOutcome::DisabledInactive => {
                // 无副作用（未知实例 / 非激活实例请求停用）
                Vec::new()
            }
            Ti3CommitOutcome::Enabled(st) => {
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime] apply_ti3_outcome Enabled (surrounding='{}' cursor={} anchor={} rect={:?}) -> relay + host_bridge FocusIn",
                    &st.surrounding_text.chars().take(8).collect::<String>(),
                    st.surrounding_cursor, st.surrounding_anchor, st.cursor_rect
                ));
                // 先把首批状态灌入 relay 缓存（未激活时只更新不产出命令），
                // 使随后的 Activate 单周期携带最新状态，避免多余的 done 往返。
                let _ = ime.relay.push_app_state(st.clone());
                // host_bridge（后备路径）：FocusIn + Surrounding + CursorRect。
                // v0.13.2 修复保留：**始终**发 Surrounding（即使 text 为空）——
                // 引擎需要 surrounding 上下文决定 consumed，否则拒收所有按键。
                if let Some(hb) = hb {
                    if hb.is_ready() {
                        hb.submit(DownEvent::State(FocusChange::Activate));
                        hb.submit(DownEvent::Surrounding(SurroundingText {
                            text: st.surrounding_text.clone(),
                            cursor: st.surrounding_cursor,
                            anchor: st.surrounding_anchor,
                        }));
                        if let Some((x, y, w, h)) = st.cursor_rect {
                            hb.submit(DownEvent::CursorRect(CursorRect { x, y, w, h }));
                        }
                    } else {
                        crate::bridge::ime_log_write(
                            "[waylandcraft][ime] apply_ti3_outcome Enabled: hb NOT ready（无法 FocusIn）"
                        );
                    }
                } else {
                    crate::bridge::ime_log_write(
                        "[waylandcraft][ime] apply_ti3_outcome Enabled: host_bridge=None（probe 失败）"
                    );
                }
                ime.relay.set_app_enabled(true, "ti3.enable")
            }
            Ti3CommitOutcome::Disabled => {
                crate::bridge::ime_log_write(
                    "[waylandcraft][ime] apply_ti3_outcome Disabled -> relay/host FocusOut"
                );
                if let Some(hb) = hb {
                    if hb.is_ready() {
                        hb.submit(DownEvent::State(FocusChange::Deactivate));
                    }
                }
                ime.relay.set_app_enabled(false, "ti3.disable")
            }
            Ti3CommitOutcome::State(st) => {
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime] apply_ti3_outcome State (surrounding='{}' cursor={} anchor={} rect={:?})",
                    &st.surrounding_text.chars().take(8).collect::<String>(),
                    st.surrounding_cursor, st.surrounding_anchor, st.cursor_rect
                ));
                let cmds = ime.relay.push_app_state(st.clone());
                if let Some(hb) = hb {
                    if hb.is_ready() {
                        hb.submit(DownEvent::Surrounding(SurroundingText {
                            text: st.surrounding_text.clone(),
                            cursor: st.surrounding_cursor,
                            anchor: st.surrounding_anchor,
                        }));
                        if let Some((x, y, w, h)) = st.cursor_rect {
                            hb.submit(DownEvent::CursorRect(CursorRect { x, y, w, h }));
                        }
                    }
                }
                cmds
            }
        };
        ime.execute_ime_commands(cmds);
    }

    // ── im2 commit(serial) 落地（input_method_v2.rs 的 Dispatch 调用）──

    /// IME 端点经 im2 wire 发来 commit(serial)：serial 校验 → 原子落地。
    pub(crate) fn ime_commit_from_wire(&mut self, serial: u32) {
        let fr = self.relay.ime_commit(serial);
        if fr.applied {
            self.state.transition(ImeStateMachine::Committing);
            self.emit_ti_batch(fr.commands);
        } else {
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][relay] im2 commit(serial={serial}) DROPPED（serial 与 done 计数不符或无内容）"
            ));
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
                    // text 为空 = 清空 preedit（None）。
                    let text_opt = if t.is_empty() { None } else { Some(t) };
                    inst.obj.preedit_string(text_opt, b, e)
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
        if !matches!(self.state, ImeStateMachine::Composing | ImeStateMachine::Committing) {
            self.state.transition(ImeStateMachine::Composing);
        }
    }

    // ── ImeCommand 执行（relay 输出的抽象命令 → IME 端点）──

    /// 执行 relay 产出的命令序列。v0.14 端点：im2（嵌套 daemon 主路径）。
    /// host_bridge 的等价命令在 apply_ti3_outcome / apply_up_events 里
    /// 就近提交（它按 DownEvent 流语义组织，不吃 ImeCommand）。
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

    /// 接收 host_bridge（dbus 后备路径）的 UpEvent 批次，灌入 relay 并原子应用。
    ///
    /// - Preedit / Commit / DeleteSurrounding → 进 relay.ime_op 缓冲
    ///   （未激活会话直接丢弃——IME 不该给没 enable 的应用送字）
    /// - Done → relay.ime_flush（无条件应用：serial 防线由宿主把守），
    ///   原子推到激活的 ti3 实例
    /// - LookupTable → 缓存给 Java 侧每帧拉取（宿主自绘候选窗时的替代显示源）
    pub fn apply_up_events(&mut self, events: Vec<UpEvent>) {
        for ev in events {
            match ev {
                UpEvent::Preedit(p) => {
                    self.relay.ime_op(ImeOp::Preedit(p.text, p.cursor_begin, p.cursor_end));
                }
                UpEvent::Commit(c) => {
                    self.relay.ime_op(ImeOp::CommitString(c.text));
                }
                UpEvent::DeleteSurrounding(d) => {
                    self.relay.ime_op(ImeOp::DeleteSurrounding(d.before_length, d.after_length));
                }
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
                UpEvent::Done(_d) => {
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

    // ── 键盘路由（bridge.rs keyboard_input 调用）──────────────

    /// 按键转发给输入法 grab（当其存在时）。返回 true 表示已交给引擎
    /// （consumed 与否由引擎裁决：消费 → im2 commit/preedit；未消费 →
    /// fcitx5 经 zwp_virtual_keyboard_v1 重新注入，见 virtual_keyboard.rs）。
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

// ── 状态机（v0.12.0 第 12 章）──

/// IME 端点状态机（覆盖 waylandcraft 整个 IME 桥接生命周期）。
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
    /// 初始状态——无任何 IME 端点
    #[default]
    Disconnected,
    /// 探测中（host_bridge::probe 或 nested daemon 在跑）
    Connecting,
    /// IME 端点在线（im2 客户端 bind / host_bridge READY）——无焦点会话
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
mod tests {
    use super::*;

    fn make_state() -> crate::WLCState {
        // 直接构造 WLCState 不带 host_bridge——apply_ti3_outcome 必须 no-op
        // 而不是 panic。
        let display: smithay::reexports::wayland_server::Display<crate::WLCState> =
            smithay::reexports::wayland_server::Display::new().unwrap();
        crate::WLCState::new(display.handle(), None)
    }

    fn snapshot(text: &str, cursor: u32, anchor: u32, rect: Option<(i32, i32, i32, i32)>) -> AppState {
        AppState::from_pending(Some(text), cursor, anchor, 0, 0, 0, rect)
    }

    #[test]
    fn default_state_has_no_active_no_grab() {
        let ime = ImeState::default();
        assert!(!ime.app_active());
        assert!(!ime.keyboard_grabbed());
        assert!(!ime.nested_route_live());
    }

    #[test]
    fn clear_focus_resets_app_active() {
        let display: smithay::reexports::wayland_server::Display<crate::WLCState> =
            smithay::reexports::wayland_server::Display::new().unwrap();
        let mut state = crate::WLCState::new(display.handle(), None);
        state.ime.relay.set_app_enabled(true, "test");
        assert!(state.ime.app_active());
        // 拆分借用：clear_focus 需要 &mut WLCState（拿 host_bridge），
        // 同 bridge.rs 的 std::mem::take 模式。
        let mut ime = std::mem::take(&mut state.ime);
        ime.clear_focus(&mut state);
        state.ime = ime;
        assert!(!state.ime.app_active(), "clear_focus 必须复位 app_active");
        assert!(!state.ime.has_focus);
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
        // 无 host_bridge / 无 im2——apply_ti3_outcome 必须 no-op 而不 panic，
        // 但 relay 会话状态照常置位。
        let mut state = make_state();
        ImeState::apply_ti3_outcome(
            &mut state,
            Ti3CommitOutcome::Enabled(snapshot("hello", 5, 5, Some((10, 20, 30, 40)))),
        );
        assert!(state.ime.app_active());
    }

    #[test]
    fn ti3_outcome_disabled_clears_app_active() {
        let mut state = make_state();
        state.ime.relay.set_app_enabled(true, "test");
        ImeState::apply_ti3_outcome(&mut state, Ti3CommitOutcome::Disabled);
        assert!(!state.ime.app_active());
    }

    #[test]
    fn ti3_outcome_ignored_is_noop() {
        let mut state = make_state();
        let before = state.ime.app_active();
        ImeState::apply_ti3_outcome(&mut state, Ti3CommitOutcome::Ignored);
        ImeState::apply_ti3_outcome(&mut state, Ti3CommitOutcome::DisabledInactive);
        assert_eq!(state.ime.app_active(), before);
    }

    #[test]
    fn ti3_outcome_state_no_host_bridge_does_not_panic() {
        let mut state = make_state();
        ImeState::apply_ti3_outcome(
            &mut state,
            Ti3CommitOutcome::State(snapshot("abc", 3, 3, None)),
        );
    }

    #[test]
    fn handle_key_without_grab_returns_false() {
        // 无 im2 grab（daemon 未接管键盘）：mod 不拦——raw 直通路径继续。
        let mut ime = ImeState::default();
        let handled = ime.handle_key(30, KeyboardAction::Press, (0, 0, 0, 0));
        assert!(!handled);
    }

    #[test]
    fn apply_up_events_records_lookup_table() {
        let mut ime = ImeState::default();
        ime.apply_up_events(vec![UpEvent::LookupTable(LookupTable {
            candidates: vec!["候选1".into(), "候选2".into()],
            labels: vec![],
            cursor_pos: 0,
            cursor_visible: true,
            page_size: 9,
            orientation: 0,
            visible: true,
        })]);
        let snap = ime.take_lookup_table().expect("应有候选窗");
        assert_eq!(snap.candidates.len(), 2);
    }

    #[test]
    fn apply_up_events_inactive_session_discards() {
        // app_active=false（无激活会话）时 commit 不进 relay 缓冲，
        // Done flush 无事发生；无 ti3 实例也不 panic。
        let mut ime = ImeState::default();
        ime.apply_up_events(vec![
            UpEvent::Commit(Commit { text: "你".into() }),
            UpEvent::Done(Done { batch_id: 1 }),
        ]);
        assert_eq!(ime.relay.pending_ops_len(), 0);
    }

    // ── 集成测试（mock host_bridge）──────────────────────────────────
    // v0.13.4 起保留：验证 host_bridge 后备路径的 DownEvent 时序。
    // v0.14：真实 daemon 路径的 wire 级测试见 tests.rs。

    use crate::host_bridge::{HostBridge, HostBridgeHandle};
    use std::sync::{Arc, Mutex};

    /// 记录所有 submit + 提供 take_up_events 的 mock 后端
    #[derive(Clone)]
    struct MockHostBridge {
        received: Arc<Mutex<Vec<DownEvent>>>,
        next_events: Arc<Mutex<Vec<UpEvent>>>,
    }

    impl MockHostBridge {
        fn new() -> Self {
            Self {
                received: Arc::new(Mutex::new(Vec::new())),
                next_events: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl HostBridge for MockHostBridge {
        fn name(&self) -> &'static str { "mock" }
        fn is_ready(&self) -> bool { true }
        fn is_dead(&self) -> bool { false }
        fn submit(&mut self, ev: DownEvent) {
            self.received.lock().unwrap().push(ev);
        }
        fn take_up_events(&mut self) -> Vec<UpEvent> {
            std::mem::take(&mut *self.next_events.lock().unwrap())
        }
        fn update_cursor_rect(&mut self, _rect: CursorRect) {}
    }

    fn attach_mock(wc: &mut crate::WLCState, backend: MockHostBridge) {
        wc.host_bridge = Some(HostBridgeHandle::new(Box::new(backend)));
    }

    /// v1.2.7 验证：Enabled 路径按 FocusIn → Surrounding → CursorRect 顺序，
    /// 且 v0.13.2「空 surrounding 也发」修复不回退。
    #[test]
    fn enabled_sends_focus_in_then_surrounding_then_cursor_rect() {
        let mut state = make_state();
        let backend = MockHostBridge::new();
        let received = backend.received.clone();
        attach_mock(&mut state, backend);
        ImeState::apply_ti3_outcome(
            &mut state,
            Ti3CommitOutcome::Enabled(snapshot("hello", 5, 5, Some((10, 20, 30, 40)))),
        );
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 3, "3 DownEvent expected");
        assert!(matches!(received[0], DownEvent::State(FocusChange::Activate)));
        if let DownEvent::Surrounding(s) = &received[1] {
            assert_eq!(s.text, "hello");
            assert_eq!(s.cursor, 5);
        } else { panic!("Second should be Surrounding, got {:?}", received[1]); }
        if let DownEvent::CursorRect(c) = &received[2] {
            assert_eq!(c.x, 10);
            assert_eq!(c.y, 20);
        } else { panic!("Third should be CursorRect, got {:?}", received[2]); }
    }

    /// v0.13.2 修复不回退：空 surrounding 也必须发。
    #[test]
    fn empty_surrounding_still_sends() {
        let mut state = make_state();
        let backend = MockHostBridge::new();
        let received = backend.received.clone();
        attach_mock(&mut state, backend);
        ImeState::apply_ti3_outcome(
            &mut state,
            Ti3CommitOutcome::Enabled(snapshot("", 0, 0, None)),
        );
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2, "FocusIn + 空 Surrounding 都应发出");
    }

    /// Disabled 路径发 FocusOut。
    #[test]
    fn disabled_sends_focus_out() {
        let mut state = make_state();
        let backend = MockHostBridge::new();
        let received = backend.received.clone();
        attach_mock(&mut state, backend);
        ImeState::apply_ti3_outcome(&mut state, Ti3CommitOutcome::Enabled(snapshot("x", 1, 1, None)));
        received.lock().unwrap().clear();
        ImeState::apply_ti3_outcome(&mut state, Ti3CommitOutcome::Disabled);
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert!(matches!(received[0], DownEvent::State(FocusChange::Deactivate)));
    }

    /// mock 后端推回 preedit+done，take_up_events_batched 正确分组。
    #[test]
    fn take_up_events_batched_groups_by_done() {
        let backend = MockHostBridge::new();
        let next_events = backend.next_events.clone();
        let mut h = HostBridgeHandle::new(Box::new(backend));
        next_events.lock().unwrap().push(UpEvent::Preedit(PreeditUpdate::set("你".to_string(), 1i32, 1i32)));
        next_events.lock().unwrap().push(UpEvent::Done(Done { batch_id: 1 }));
        let batches = h.take_up_events_batched();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 2, "preedit + done");
    }

    /// host_bridge 上行 commit 灌入 relay → flush（app_active 时缓冲落地）。
    #[test]
    fn host_up_events_buffered_until_done_flush() {
        let mut ime = ImeState::default();
        ime.relay.set_app_enabled(true, "test");
        ime.apply_up_events(vec![UpEvent::Commit(Commit { text: "你".into() })]);
        assert_eq!(ime.relay.pending_ops_len(), 1, "Done 前缓冲保留");
        ime.apply_up_events(vec![UpEvent::Done(Done { batch_id: 1 })]);
        assert_eq!(ime.relay.pending_ops_len(), 0, "Done 触发 flush");
    }
}
