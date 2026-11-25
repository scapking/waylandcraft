//! 输入法中继状态机 —— 协议语义的核心，零 Wayland 类型依赖。
//!
//! v0.14 嵌套 IM 路线恢复：本文件自 `.deleted_v010/relay.rs` 还原（v0.9.27
//! 家族协议级重写版 + v0.9.44 ti3 抖动保序修复）。它是嵌套 daemon
//! （fcitx5/ibus，经 `zwp_input_method_v2` 接入）、嵌套应用（经
//! `zwp_text_input_v3` 接入）与 host_bridge（dbus 穿透，可选后备路径）
//! 之间的唯一裁决层；wire 层（`text_input_v3` / `input_method_v2` /
//! `host_bridge`）只做类型转换与事件收发。
//!
//! 依据的协议规范（wayland-protocols 官方 XML）：
//!
//! **zwp_text_input_v3**
//! - `done(serial)`：serial 必须等于该 text_input 对象已收到的 commit 请求数。
//!   客户端据此识别丢弃（收到不匹配的 serial，典型为 0）。
//! - leave 之后合成器必须忽略该实例的一切请求，直到下一次 enter。
//! - enter 必须发给聚焦 client 的全部 text_input 实例。
//!
//! **zwp_input_method_v2**
//! - `commit(serial)`：serial 必须 = 该 input_method 对象已发出的 done 事件数；
//!   不匹配时「照常处理但不改变对象当前状态」（协议原文：proceed as normal,
//!   except it should not change the current state）——**文本批次照常应用**，
//!   仅不做状态确认。sync7 实机故障正是把它错读成整批丢弃，serial 计数一旦
//!   漂移全部汉字上屏归零（修复说明见 [`Relay::ime_commit`]）。
//! - `done` 事件本身不带 serial 参数；计数由 wire 层
//!   （`input_method_v2.rs::InputMethodV2State`）按对象代际维护，是 commit
//!   serial 校验的唯一事实源；本模块的 `ime_done_count` 只是它的只读镜像
//!   （诊断用），不参与任何判定。
//! - 应用顺序：替换 preedit → 删除 surrounding → 插入 commit → 重算 surrounding → 新 preedit。
//!   因此 preedit/delete/commit_string 的**顺序不可乱**，必须原样转发。
//!
//! 本模块不持有任何 Wayland 对象：所有输出都是抽象命令
//! （[`ImeCommand`] / [`TiCommand`] / [`FlushResult`]），由 wire 层执行。

/// App 侧已提交的文本状态（ti3 double-buffer 应用后的当前值）。
///
/// 数值字段保存协议原始编码（u32 位掩码/枚举值），避免本模块依赖
/// wayland 类型；wire 层负责与 `ContentHint` / `ContentPurpose` 等互转。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppState {
    /// 环绕文本（光标前后的上下文）。空串表示 app 未上报。
    pub surrounding_text: String,
    /// 光标在环绕文本中的字节偏移（unicode 偏移，按协议为 uint）。
    pub surrounding_cursor: u32,
    /// 选区锚点偏移；等于 cursor 表示无选区。
    pub surrounding_anchor: u32,
    /// `zwp_text_input_v3.content_hint` 位掩码原始值。
    pub content_hint: u32,
    /// `zwp_text_input_v3.content_purpose` 枚举原始值。
    pub content_purpose: u32,
    /// `text_change_cause` 枚举原始值。
    pub change_cause: u32,
    /// App 上报的光标矩形 (x, y, width, height)，surface 局部坐标；
    /// None 表示尚未上报。用于输入法候选窗定位。
    pub cursor_rect: Option<(i32, i32, i32, i32)>,
}

impl AppState {
    /// wire 层从 ti3 double-buffer 构造快照的入口。
    #[allow(clippy::too_many_arguments)]
    pub fn from_pending(
        surrounding_text: Option<&str>,
        cursor: u32,
        anchor: u32,
        content_hint: u32,
        content_purpose: u32,
        change_cause: u32,
        cursor_rect: Option<(i32, i32, i32, i32)>,
    ) -> Self {
        Self {
            surrounding_text: surrounding_text.unwrap_or_default().to_string(),
            surrounding_cursor: cursor,
            surrounding_anchor: anchor,
            content_hint,
            content_purpose,
            change_cause,
            cursor_rect,
        }
    }

    /// 是否带有任何有意义的文本上下文（surrounding 或内容类型）。
    pub fn is_meaningful(&self) -> bool {
        !self.surrounding_text.is_empty()
            || self.content_hint != 0
            || self.content_purpose != 0
            || self.cursor_rect.is_some()
            || self.change_cause != 0
    }
}

/// 发往输入法端点的抽象命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeCommand {
    /// 激活输入法（对应 im2 `activate` / 穿透 enable），并附带当前 app 状态。
    Activate(AppState),
    /// 停用输入法（对应 im2 `deactivate` + `done` / 穿透 disable）。
    Deactivate,
    /// 激活期间的状态增量推送（surrounding/content_type/cursor_rect 等）。
    PushState(AppState),
}

/// 发往 App（text-input-v3 客户端）的事件命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TiCommand {
    Preedit(String, i32, i32),
    DeleteSurrounding(u32, u32),
    CommitString(String),
    /// 应用批次。`serial == 0` 表示丢弃标记（协议允许的复位手段），
    /// 否则由 wire 层以 per-instance commit 计数填充后发出。
    Done {
        serial: u32,
    },
}

/// 输入法端点发来的单条文本操作（缓冲于 [`Relay::pending_ops`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeOp {
    Preedit(String, i32, i32),
    DeleteSurrounding(u32, u32),
    CommitString(String),
}

/// [`Relay::ime_flush`] / [`Relay::ime_commit`] 的结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FlushResult {
    /// 应向 active 的 text-input 实例依序发出的事件。
    /// `flushed=true` 时**必须原样下发**——serial 不符不丢文本（协议）。
    pub commands: Vec<TiCommand>,
    /// true = 批次有内容且 serial 与 wire 层 done 计数一致（状态确认批）。
    /// false 有两种含义，用 [`FlushResult::flushed`] 区分：
    /// - flushed=true + applied=false：文本照常下发，仅「对象当前状态」
    ///   语义不做确认（协议原文的 mismatch 处理路径）。
    /// - flushed=false：无内容/无激活会话，无事发生。
    pub applied: bool,
    /// true = 本次把 pending 缓冲刷出并产出了 commands（与 serial 是否匹配无关）。
    pub flushed: bool,
}

/// 中继状态机。
///
/// 一个合成器 seat 对应一个 Relay。它跟踪：
/// - app 侧激活状态（ti3 enable 且聚焦）
/// - IME 端点在线状态（im2 instance 存在 / 穿透就绪）
/// - wire 层已发 done 计数的**只读镜像**（诊断/status 用；协议判定的
///   唯一事实源是 wire 层对象代际计数，见 `input_method_v2.rs`）
/// - IME 待应用操作缓冲（原子性：flush 时整批产出，serial 不符不丢文本）
#[derive(Debug, Default)]
pub struct Relay {
    app_active: bool,
    ime_present: bool,
    ime_done_count: u32,
    pending_ops: Vec<ImeOp>,
    last_state: AppState,
}

impl Relay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn app_active(&self) -> bool {
        self.app_active
    }

    pub fn ime_present(&self) -> bool {
        self.ime_present
    }

    /// wire 层当前 im2 对象已发 done 计数的只读镜像（诊断/status 上报用，
    /// 不参与协议判定）。生产运行时序上由 [`Relay::mirror_done_count`] 与
    /// wire 层保持同步；纯 relay 单测里只有显式 sync 才变化。
    pub fn ime_done_count(&self) -> u32 {
        self.ime_done_count
    }

    /// wire 层把「当前 zwp_input_method_v2 对象实际已发 done 数」同步进
    /// 只读镜像（每次 send_done 之后、对象代际创建/销毁时归零）。
    /// 本方法**只更新诊断镜像**——serial 校验永远对照 `ime_commit` 显式
    /// 传入的 wire 计数（单一事实源），与本字段无关。
    pub fn mirror_done_count(&mut self, wire_done: u32) {
        self.ime_done_count = wire_done;
    }

    /// 缓冲中尚未落地的 IME 操作数（诊断用：非零 = 有在途组合）。
    pub fn pending_ops_len(&self) -> usize {
        self.pending_ops.len()
    }

    /// IME 端点上线/下线。
    ///
    /// 上线时若 app 已激活，立即下发 Activate（携带最新已知状态）；
    /// 下线时丢弃未应用的缓冲并复位 done 计数（新实例从 0 开始）。
    /// 返回需要执行的 ImeCommand 序列。
    pub fn set_ime_present(&mut self, present: bool) -> Vec<ImeCommand> {
        self.ime_present = present;
        let mut cmds = Vec::new();
        if present {
            // 新对象代际：done 计数从 0 开始（wire 层随后逐次 sync 镜像）。
            self.ime_done_count = 0;
            if self.app_active && self.last_state.is_meaningful() {
                cmds.push(ImeCommand::Activate(self.last_state.clone()));
            } else if self.app_active {
                cmds.push(ImeCommand::Activate(AppState::default()));
            }
        } else {
            self.pending_ops.clear();
            self.ime_done_count = 0;
        }
        cmds
    }

    /// App 侧 enable/disable 生命周期变化。
    ///
    /// enable=true 且 IME 在线 → Activate；enable=false → 清缓冲 + Deactivate。
    /// `reason` 是变化来源（ti3.enable / ti3.disable / focus_lost / 测试），
    /// 写入日志用于实机诊断 set_active 周期性 toggle 的驱动源（P3）。
    ///
    /// v0.9.44 修法：firefox 等 GTK 应用的 ti3 enter/leave 会**频繁抖动**
    /// （事实 3：协议层正常行为，焦点跨 surface 时常见）。每 disable 强制
    /// 清 pending_ops 会让 host_bridge 还在路上的 commit/preedit 信号**全部
    /// 丢失**——用户看到的"打字能 commit 几次"就是这个原因。
    ///
    /// 修法：
    /// - `focus_lost` 才是真失焦（窗口销毁 / 焦点真离开 app）→ 照旧清缓冲
    /// - `ti3.disable`（enable → disable 抖动）→ **保留缓冲**（host_bridge
    ///   signal 仍会送达 → flush 仍能 push）
    /// - app_active 状态本身仍正常 toggle（让 im2 grab 知道当前是 focus
    ///   in 还是 out）
    pub fn set_app_enabled(
        &mut self,
        enabled: bool,
        reason: &str,
    ) -> Vec<ImeCommand> {
        let mut cmds = Vec::new();
        if enabled == self.app_active {
            return cmds;
        }
        let was_active = self.app_active;
        self.app_active = enabled;
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime][relay] set_app_enabled {enabled} (reason={reason})"
        ));
        if enabled {
            if self.ime_present {
                // done 由 wire 层实际发出后经 mirror_done_count 同步计数。
                cmds.push(ImeCommand::Activate(self.last_state.clone()));
            }
        } else {
            // v0.9.44 修法：只在真失焦时清缓冲（focus_lost 路径）。
            // ti3.disable 抖动期间保留 pending_ops——host_bridge 还在路上的
            // commit/preedit 信号仍能 flush 推到 firefox 文本框。
            if reason == "focus_lost" {
                self.pending_ops.clear();
            }
            if self.ime_present {
                cmds.push(ImeCommand::Deactivate);
            }
        }
        // 抑制日志
        let _ = was_active;
        cmds
    }

    /// 键盘焦点整体丢失（surface 失焦 / 窗口销毁）。
    /// 等价于强制 disable 并清空全部会话状态。
    pub fn focus_lost(&mut self) -> Vec<ImeCommand> {
        self.last_state = AppState::default();
        self.set_app_enabled(false, "focus_lost")
    }

    /// 推送 App 最新已提交状态（ti3 commit 携带的新值）。
    /// 仅在激活且 IME 在线时有意义。
    pub fn push_app_state(&mut self, state: AppState) -> Vec<ImeCommand> {
        let meaningful_change = state != self.last_state;
        self.last_state = state.clone();
        if self.app_active && self.ime_present && meaningful_change {
            // PushState 落地时 wire 层会发一次 im2 done（状态更新必须由 done 应用）；
            // 计数由 wire 层发出后经 mirror_done_count 同步——不在命令产生时乐观
            // 推进（sync7 漂移根因之一：命令被调用方丢弃/实例换代时，乐观计数
            // 永远追不上 wire 实况）。
            vec![ImeCommand::PushState(state)]
        } else {
            Vec::new()
        }
    }

    /// 当前缓存的最新 app 状态（供 wire 层在 IME 迟到上线时使用）。
    pub fn current_state(&self) -> &AppState {
        &self.last_state
    }

    /// IME 端点发来一条文本操作：仅缓冲，不落地。
    pub fn ime_op(&mut self, op: ImeOp) {
        if !self.app_active {
            // 未激活的 text input 不存在合法接收者；直接丢弃。
            return;
        }
        self.pending_ops.push(op);
    }

    /// IME 端点请求应用缓冲（im2 `commit(serial)` 路径）。
    ///
    /// `done_sent` = wire 层当前 zwp_input_method_v2 对象实际已发 done 数
    /// （单一事实源，由 `input_method_v2.rs` 维护；新代际对象为 0 起步）。
    /// 协议（input-method-unstable-v2 `<request name="commit">`）：serial 与
    /// 该计数不符时「照常处理，但不改变对象当前状态」——**文本批次必须照常
    /// 产出**，`applied=false` 仅表示本批未获状态确认。sync7 实机故障正是
    /// 把它错读成整批丢弃（shadow 乐观计数一漂移，之后所有汉字全灭）；
    /// 该 DROPPED 分支已按协议废除。
    ///
    /// 返回 [`FlushResult`]：`flushed` 标记本批是否有内容落地；`applied=true`
    /// 表示内容落地**且** serial 与 wire 计数匹配，wire 层两种情况都要把
    /// commands 推给 app 并补发 ti3 `done(<per-instance commit 计数>)`。
    pub fn ime_commit(&mut self, serial: u32, done_sent: u32) -> FlushResult {
        let mut fr = self.finish_flush();
        if fr.flushed && serial != done_sent {
            // 文本照常交付（永不丢），仅撤销「状态确认」语义。
            fr.applied = false;
        }
        fr
    }

    /// 无条件应用缓冲（宿主穿透路径：serial 防线由宿主合成器把守，
    /// 到达这里的批次视为已被校验）。
    pub fn ime_flush(&mut self) -> FlushResult {
        self.finish_flush()
    }

    fn finish_flush(&mut self) -> FlushResult {
        if self.pending_ops.is_empty() || !self.app_active {
            self.pending_ops.clear();
            return FlushResult::default();
        }
        let ops = std::mem::take(&mut self.pending_ops);
        let mut commands = Vec::with_capacity(ops.len() + 1);
        for op in ops {
            match op {
                ImeOp::Preedit(t, b, e) => {
                    commands.push(TiCommand::Preedit(t, b, e))
                }
                ImeOp::DeleteSurrounding(b, a) => {
                    commands.push(TiCommand::DeleteSurrounding(b, a))
                }
                ImeOp::CommitString(t) => {
                    commands.push(TiCommand::CommitString(t))
                }
            }
        }
        FlushResult {
            commands,
            applied: true,
            flushed: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IME 未上线时 app enable 不应产生命令（等 IME 上线再补 Activate）。
    #[test]
    fn enable_before_ime_defers_activation() {
        let mut r = Relay::new();
        assert!(r.set_app_enabled(true, "test").is_empty());
        assert!(r.app_active());

        // IME 迟到上线 → 补发 Activate
        let cmds = r.set_ime_present(true);
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            ImeCommand::Activate(st) => assert_eq!(st, &AppState::default()),
            other => panic!("expect Activate, got {other:?}"),
        }
    }

    /// disable 清空未应用缓冲并通知端点。
    #[test]
    fn disable_discards_pending_and_deactivates() {
        let mut r = Relay::new();
        r.set_app_enabled(true, "test");
        r.set_ime_present(true);

        r.ime_op(ImeOp::Preedit("ni".into(), 0, 2));
        let cmds = r.set_app_enabled(false, "test");
        assert_eq!(cmds, vec![ImeCommand::Deactivate]);

        // 缓冲已清空：flush 无事发生
        let fr = r.ime_flush();
        assert!(!fr.applied);
        assert!(fr.commands.is_empty());
    }

    /// serial 递增链：Activate、Deactivate、PushState 各对应一次 wire done。
    /// 计数经 mirror_done_count 从 wire 层同步（不再在命令产生时乐观自增——
    /// 这是 sync7 漂移根因；ime_done_count 是只读镜像，判定基准是显式参数）。
    #[test]
    fn serial_accounting_activate_deactivate() {
        let mut r = Relay::new();
        r.set_ime_present(true);
        let cmds = r.set_app_enabled(true, "test"); // Activate → done#1
        assert_eq!(cmds.len(), 1);
        r.mirror_done_count(1); // 模拟 wire 层实发后同步
        assert_eq!(r.ime_done_count, 1);
        r.set_app_enabled(false, "test"); // Deactivate → done#2
        r.mirror_done_count(2);
        assert_eq!(r.ime_done_count, 2);
        r.set_app_enabled(true, "test"); // Activate → done#3
        r.mirror_done_count(3);
        assert_eq!(r.ime_done_count, 3);

        // IME 以旧 serial 提交：协议「照常处理但不改变对象当前状态」——
        // 文本照常产出（永不丢），applied=false 仅表示本批未获状态确认。
        r.ime_op(ImeOp::CommitString("你好".into()));
        let fr = r.ime_commit(1, 3);
        assert!(fr.flushed, "serial 不符也必须 flush 文本（sync7 回归）");
        assert_eq!(fr.commands, vec![TiCommand::CommitString("你好".into())]);
        assert!(!fr.applied);
        // IME 重发并以正确 serial 提交 → 状态确认批
        r.ime_op(ImeOp::CommitString("你好".into()));
        let fr = r.ime_commit(3, 3);
        assert!(fr.applied);
        assert_eq!(fr.commands, vec![TiCommand::CommitString("你好".into())]);
    }

    /// PushState 属于带 done 的状态更新，wire 发出后计数推进。
    #[test]
    fn push_state_advances_serial() {
        let mut r = Relay::new();
        r.set_app_enabled(true, "test");
        let cmds = r.set_ime_present(true); // Activate → 1
        assert_eq!(cmds.len(), 1);
        r.mirror_done_count(1);
        assert_eq!(r.ime_done_count, 1);

        let st = AppState {
            surrounding_text: "abc".into(),
            surrounding_cursor: 3,
            surrounding_anchor: 3,
            ..Default::default()
        };
        let cmds = r.push_app_state(st.clone());
        assert_eq!(cmds.len(), 1, "有意义的状态变化要推送");
        r.mirror_done_count(2); // PushState 落地 wire done#2
        assert_eq!(r.ime_done_count, 2);

        // 无变化的推送不应产生命令/推进计数
        assert!(r.push_app_state(st).is_empty());
        assert_eq!(r.ime_done_count, 2);
    }

    /// 组合流：preedit 多次演进 → 最终 commit，全部原子落地。
    #[test]
    fn pinyin_composition_flow() {
        let mut r = Relay::new();
        r.set_ime_present(true);
        r.set_app_enabled(true, "test");

        // nihao 逐键演进的 preedit。
        // 协议语义：IME 的 commit(serial) 回填「已收到的 done 计数」；组合期间
        // 合成器不推送新状态 → 不发新 done → 每步回填同一 serial（Activate 后=1）。
        // 这正是真实 fcitx5 的行为。
        // wire 基准：Activate 已实发 done#1（mirror 同步 + done_sent 显式传入）。
        r.mirror_done_count(1);
        for s in ["n", "ni", "nih", "niha", "nihao"] {
            r.ime_op(ImeOp::Preedit(
                s.to_string(),
                s.len() as i32,
                s.len() as i32,
            ));
            let fr = r.ime_commit(1, 1);
            assert!(fr.applied, "step {s} should apply");
            // FlushResult 只含操作命令；done 由 wire 层按协议补发。
            assert_eq!(fr.commands.len(), 1, "step {s}");
            assert_eq!(
                fr.commands[0],
                TiCommand::Preedit(
                    s.to_string(),
                    s.len() as i32,
                    s.len() as i32
                )
            );
        }

        // 选定候选「你好」：preedit 清空 + commit string 同批（仍回填 serial=1）
        r.ime_op(ImeOp::Preedit(String::new(), 0, 0));
        r.ime_op(ImeOp::CommitString("你好".into()));
        let fr = r.ime_commit(1, 1);
        assert!(fr.applied);
        assert_eq!(
            fr.commands,
            vec![
                TiCommand::Preedit(String::new(), 0, 0),
                TiCommand::CommitString("你好".into()),
            ]
        );
    }

    /// 删除环绕文本 + 提交的顺序保持（选区重组场景）。
    #[test]
    fn delete_then_commit_order_preserved() {
        let mut r = Relay::new();
        r.set_ime_present(true);
        r.set_app_enabled(true, "test");

        r.ime_op(ImeOp::DeleteSurrounding(3, 0));
        r.ime_op(ImeOp::CommitString("你".into()));
        let fr = r.ime_commit(1, 1);
        assert!(fr.applied);
        assert_eq!(
            fr.commands,
            vec![
                TiCommand::DeleteSurrounding(3, 0),
                TiCommand::CommitString("你".into()),
            ],
            "delete 必须先于 commit 到达 app"
        );
    }

    /// 焦点丢失：清状态、通知端点、后续 IME 操作被拒。
    #[test]
    fn focus_lost_rejects_late_ops() {
        let mut r = Relay::new();
        r.set_ime_present(true);
        r.set_app_enabled(true, "test");
        let cmds = r.focus_lost();
        assert_eq!(cmds, vec![ImeCommand::Deactivate]);

        r.ime_op(ImeOp::CommitString("迟到的".into()));
        let fr = r.ime_flush();
        assert!(!fr.applied);
    }

    /// IME 下线复位计数：重连后的新对象从 0 开始。
    #[test]
    fn ime_restart_resets_serial() {
        let mut r = Relay::new();
        r.set_app_enabled(true, "test");
        let cmds = r.set_ime_present(true); // Activate → done#1
        assert_eq!(cmds.len(), 1);
        r.mirror_done_count(1);
        assert_eq!(r.ime_done_count, 1);

        r.set_ime_present(false);
        let cmds = r.set_ime_present(true); // 重新 Activate（新对象代际）
        assert_eq!(cmds.len(), 1);
        r.mirror_done_count(1); // 新代际 wire 层从头再数
        assert_eq!(r.ime_done_count, 1);

        r.ime_op(ImeOp::CommitString("好".into()));
        assert!(r.ime_commit(1, 1).applied);
    }

    /// sync7 生产事故回归（单测形态）：im2 已产生 Activate 命令但 wire 层
    /// done 实发数为 0（未 bind 完成/对象换代窗口——旧代码里 relay 的乐观
    /// 影子计数已 +1，之后每个 commit 都 DROPPED）。修复后 serial 校验只
    /// 对照显式传入的 wire 计数，且**永不丢文本**：fcitx5 以小 serial 提交
    /// 「你哈登」，文本批次必须原样产出（旧逻辑此处 commands 为空）。
    #[test]
    fn ime_commit_wire_zero_low_serial_still_delivers() {
        let mut r = Relay::new();
        r.set_app_enabled(true, "test");
        let cmds = r.set_ime_present(true); // Activate 产出，但 wire 未发 done
        assert_eq!(cmds.len(), 1);
        r.mirror_done_count(0); // wire 实况：新对象 0 done

        r.ime_op(ImeOp::Preedit(String::new(), 0, 0));
        r.ime_op(ImeOp::CommitString("你哈登".into()));
        let fr = r.ime_commit(8, 0); // fcitx5 回填 serial=8（实机日志首例）
        assert!(fr.flushed, "serial 不符也必须 flush 文本（协议 proceed as normal）");
        assert_eq!(
            fr.commands,
            vec![
                TiCommand::Preedit(String::new(), 0, 0),
                TiCommand::CommitString("你哈登".into()),
            ],
            "组合批次必须完整交到 wire 层推给 app，旧行为是整批丢弃"
        );
        assert!(!fr.applied, "serial 不符 → 不做对象当前状态确认");
    }

    /// sync7 回归（判定与镜像解耦）：协议判定只看显式 done_sent 参数——
    /// 即使只读镜像被同步成偏离值（模拟历史漂移形态），匹配 wire 实况的
    /// commit 仍被确认应用。
    #[test]
    fn ime_commit_validates_explicit_wire_count_not_mirror() {
        let mut r = Relay::new();
        r.set_ime_present(true);
        r.set_app_enabled(true, "test"); // Activate（假设 wire 已实发 done#1）
        r.mirror_done_count(42); // 故意污染诊断镜像（旧影子计数漂移形态）
        r.ime_op(ImeOp::CommitString("测".into()));
        let fr = r.ime_commit(1, 1); // serial 与 wire 实况一致，与镜像无关
        assert!(fr.flushed);
        assert!(fr.applied, "serial 判定基准必须是显式 wire 计数");
        assert_eq!(fr.commands, vec![TiCommand::CommitString("测".into())]);
    }

    /// sync7 回归（日志语义分离）：面板清空时 fcitx5 会发裸 commit(serial)
    /// （waylandimserverv2.cpp::updatePreeditDelegate——preedit 为空时不发
    /// set_preedit_string 也照发 commit）。这类无内容批次 flushed=false，
    /// 与「serial 不符的文本批」在 wire 层可区分，不再共用 DROPPED 文案。
    #[test]
    fn ime_commit_empty_batch_distinguishable_from_mismatch() {
        let mut r = Relay::new();
        r.set_ime_present(true);
        r.set_app_enabled(true, "test");
        r.mirror_done_count(1);
        // 无内容：不是丢文本。
        let empty = r.ime_commit(1, 1);
        assert!(!empty.flushed && !empty.applied && empty.commands.is_empty());
        // 有内容但 serial 超前：文本仍交付（flushed=true, applied=false）。
        r.ime_op(ImeOp::CommitString("哈".into()));
        let late = r.ime_commit(7, 1);
        assert!(late.flushed && !late.applied);
        assert_eq!(late.commands, vec![TiCommand::CommitString("哈".into())]);
    }
}
