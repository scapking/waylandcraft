//! 输入法中继状态机 —— 协议语义的核心，零 Wayland 类型依赖。
//!
//! 这里是 text-input-v3 与输入法端点（input-method-v2 或宿主穿透）之间的
//! 唯一裁决层。所有 serial 记账、enable/disable 生命周期、IME 变更的
//! 原子缓冲与丢弃判定都在这里完成；wire 层（`text_input_v3` /
//! `input_method_v2` / `passthrough`）只做类型转换与事件收发。
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
//!   不匹配时「照常处理但不改变对象当前状态」——即整批丢弃 IME 的变更。
//! - `done` 事件本身不带 serial 参数。
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
    Done { serial: u32 },
}

/// 输入法端点发来的单条文本操作（缓冲于 [`Relay::pending_ops`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeOp {
    Preedit(String, i32, i32),
    DeleteSurrounding(u32, u32),
    CommitString(String),
}

/// [`Relay::ime_flush`] 的结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FlushResult {
    /// 应向 active 的 text-input 实例依序发出的事件。
    pub commands: Vec<TiCommand>,
    /// true = 批次被接受并应用（wire 层需补发 `done(<commit 计数>)`）；
    /// false = serial 校验失败或无内容，批次被丢弃（不得向 app 发 done）。
    pub applied: bool,
}

/// 中继状态机。
///
/// 一个合成器 seat 对应一个 Relay。它跟踪：
/// - app 侧激活状态（ti3 enable 且聚焦）
/// - IME 端点在线状态（im2 instance 存在 / 穿透就绪）
/// - 已向 IME 端点发出的 done 计数（= 协议要求 IME 回填的 serial 基准）
/// - IME 待应用操作缓冲（原子性：只有 flush 成功才落到 app）
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

    /// IME 端点上线/下线。
    ///
    /// 上线时若 app 已激活，立即下发 Activate（携带最新已知状态）；
    /// 下线时丢弃未应用的缓冲并复位 done 计数（新实例从 0 开始）。
    /// 返回需要执行的 ImeCommand 序列。
    pub fn set_ime_present(&mut self, present: bool) -> Vec<ImeCommand> {
        self.ime_present = present;
        let mut cmds = Vec::new();
        if present {
            // 新端点从零计数开始。
            self.ime_done_count = 0;
            if self.app_active && self.last_state.is_meaningful() {
                cmds.push(ImeCommand::Activate(self.last_state.clone()));
                self.ime_done_count += 1; // Activate 携带状态，wire 层会发一次 done
            } else if self.app_active {
                cmds.push(ImeCommand::Activate(AppState::default()));
                self.ime_done_count += 1;
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
    pub fn set_app_enabled(&mut self, enabled: bool) -> Vec<ImeCommand> {
        let mut cmds = Vec::new();
        if enabled == self.app_active {
            return cmds;
        }
        self.app_active = enabled;
        if enabled {
            if self.ime_present {
                cmds.push(ImeCommand::Activate(self.last_state.clone()));
                self.ime_done_count += 1;
            }
        } else {
            // 停用即作废 IME 未应用的变更（app 状态即将离开输入上下文）。
            self.pending_ops.clear();
            if self.ime_present {
                cmds.push(ImeCommand::Deactivate);
                self.ime_done_count += 1; // Deactivate 后 wire 层发 done
            }
        }
        cmds
    }

    /// 键盘焦点整体丢失（surface 失焦 / 窗口销毁）。
    /// 等价于强制 disable 并清空全部会话状态。
    pub fn focus_lost(&mut self) -> Vec<ImeCommand> {
        self.last_state = AppState::default();
        self.set_app_enabled(false)
    }

    /// 推送 App 最新已提交状态（ti3 commit 携带的新值）。
    /// 仅在激活且 IME 在线时有意义。
    pub fn push_app_state(&mut self, state: AppState) -> Vec<ImeCommand> {
        let meaningful_change = state != self.last_state;
        self.last_state = state.clone();
        if self.app_active && self.ime_present && meaningful_change {
            // PushState 落地时 wire 层会发一次 im2 done（状态更新必须由 done 应用），
            // 计数随命令产生即推进，保证 IME 回填的 serial 与之匹配。
            self.ime_done_count += 1;
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
    /// serial 必须 = 已向该端点发出的 done 计数；不匹配则整批丢弃
    /// （协议："proceed as normal, except it should not change the current
    /// state"）。返回 [`FlushResult`]，`applied=true` 时 wire 层负责在
    /// commands 之后补发 ti3 `done(<per-instance commit 计数>)`。
    pub fn ime_commit(&mut self, serial: u32) -> FlushResult {
        if serial != self.ime_done_count {
            // 过期/超前 serial：丢弃本批缓冲，不产生任何 app 可见变化。
            self.pending_ops.clear();
            return FlushResult::default();
        }
        self.finish_flush()
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
                ImeOp::Preedit(t, b, e) => commands.push(TiCommand::Preedit(t, b, e)),
                ImeOp::DeleteSurrounding(b, a) => {
                    commands.push(TiCommand::DeleteSurrounding(b, a))
                }
                ImeOp::CommitString(t) => commands.push(TiCommand::CommitString(t)),
            }
        }
        FlushResult {
            commands,
            applied: true,
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
        assert!(r.set_app_enabled(true).is_empty());
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
        r.set_app_enabled(true);
        r.set_ime_present(true);

        r.ime_op(ImeOp::Preedit("ni".into(), 0, 2));
        let cmds = r.set_app_enabled(false);
        assert_eq!(cmds, vec![ImeCommand::Deactivate]);

        // 缓冲已清空：flush 无事发生
        let fr = r.ime_flush();
        assert!(!fr.applied);
        assert!(fr.commands.is_empty());
    }

    /// serial 正确递增链：Activate(+1)、Deactivate(+1)、PushState 不加
    /// （PushState 不发 done？—— 不对：im2 PushState 后必须发 done！见下个测试）。
    #[test]
    fn serial_accounting_activate_deactivate() {
        let mut r = Relay::new();
        r.set_ime_present(true);
        r.set_app_enabled(true); // Activate → done#1
        assert_eq!(r.ime_done_count, 1);
        r.set_app_enabled(false); // Deactivate → done#2
        assert_eq!(r.ime_done_count, 2);
        r.set_app_enabled(true); // Activate → done#3
        assert_eq!(r.ime_done_count, 3);

        // IME 以旧 serial 提交 → 丢弃（缓冲被清空）
        r.ime_op(ImeOp::CommitString("你好".into()));
        let fr = r.ime_commit(1);
        assert!(!fr.applied);
        // IME 重发并以正确 serial 提交 → 应用
        r.ime_op(ImeOp::CommitString("你好".into()));
        let fr = r.ime_commit(3);
        assert!(fr.applied);
        assert_eq!(
            fr.commands,
            vec![TiCommand::CommitString("你好".into())]
        );
    }

    /// PushState 属于带 done 的状态更新，必须推进计数。
    #[test]
    fn push_state_advances_serial() {
        let mut r = Relay::new();
        r.set_app_enabled(true);
        r.set_ime_present(true); // Activate → 1
        assert_eq!(r.ime_done_count, 1);

        let st = AppState {
            surrounding_text: "abc".into(),
            surrounding_cursor: 3,
            surrounding_anchor: 3,
            ..Default::default()
        };
        let cmds = r.push_app_state(st.clone());
        assert_eq!(cmds.len(), 1, "有意义的状态变化要推送");
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
        r.set_app_enabled(true);

        // nihao 逐键演进的 preedit。
        // 协议语义：IME 的 commit(serial) 回填「已收到的 done 计数」；组合期间
        // 合成器不推送新状态 → 不发新 done → 每步回填同一 serial（Activate 后=1）。
        // 这正是真实 fcitx5 的行为。
        for s in ["n", "ni", "nih", "niha", "nihao"] {
            r.ime_op(ImeOp::Preedit(s.to_string(), s.len() as i32, s.len() as i32));
            let fr = r.ime_commit(1);
            assert!(fr.applied, "step {s} should apply");
            // FlushResult 只含操作命令；done 由 wire 层按协议补发。
            assert_eq!(fr.commands.len(), 1, "step {s}");
            assert_eq!(fr.commands[0], TiCommand::Preedit(s.to_string(), s.len() as i32, s.len() as i32));
        }

        // 选定候选「你好」：preedit 清空 + commit string 同批（仍回填 serial=1）
        r.ime_op(ImeOp::Preedit(String::new(), 0, 0));
        r.ime_op(ImeOp::CommitString("你好".into()));
        let fr = r.ime_commit(1);
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
        r.set_app_enabled(true);

        r.ime_op(ImeOp::DeleteSurrounding(3, 0));
        r.ime_op(ImeOp::CommitString("你".into()));
        let fr = r.ime_commit(1);
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
        r.set_app_enabled(true);
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
        r.set_app_enabled(true);
        r.set_ime_present(true); // done#1
        assert_eq!(r.ime_done_count, 1);

        r.set_ime_present(false);
        r.set_ime_present(true); // 重新 Activate → done#1（新对象）
        assert_eq!(r.ime_done_count, 1);

        r.ime_op(ImeOp::CommitString("好".into()));
        assert!(r.ime_commit(1).applied);
    }
}
