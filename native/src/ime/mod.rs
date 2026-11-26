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
//!
//! 路由门（v0.14.1 收紧）：`nested_route_live()` 为 true 时，
//! set_focus / clear_focus / apply_ti3_outcome **一律不再向
//! host_bridge 提交 FocusIn/FocusOut/Surrounding/CursorRect**——
//! sync6 实机取证：嵌套主路径已激活，宿主 dbus-ibus 仍在收
//! FocusIn/FocusOut（弹候选窗、维持热焦点，纯干扰）。daemon 掉线
//! 回退后 route_live 变 false，ti3 事件恢复喂宿主（幂等补热）。

mod builtin;
mod ime_event;
mod input_method_v2;
pub(crate) mod input_popup;
pub(crate) mod metrics;
mod nested_daemon;
mod relay;
mod text_input_v3;
mod virtual_keyboard;

#[cfg(test)]
mod tests_builtin;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_real;
#[cfg(test)]
mod tests_popup;

pub use builtin::{BuiltinEngine, ImeSource};
pub use ime_event::{
    Commit, CursorRect, DeleteSurrounding, Done, DownEvent, FocusChange, KeyEvent,
    LookupTable, PreeditUpdate, SurroundingText, UpEvent,
};
pub use input_method_v2::InputMethodV2State;
pub use input_popup::{InputPopupState, PopupFrame, PopupSnapshot};
pub use nested_daemon::{NestedIme, NestedMode, NestedPhase};
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

    /// `zwp_input_popup_surface_v2` 候选窗 surface 的角色登记 + 帧采集队列
    /// （Phase 1）。挂在这里而非 `im2.popup` 字段：后者只维护协议对象与
    /// 矩形上报（input_method_v2.rs 职责），本字段负责像素采集与 JNI 取帧，
    /// 两者生命周期一致但关注点分离。
    pub(crate) popup: InputPopupState,

    /// 协议中继状态机：serial 记账、原子缓冲、丢弃判定。
    pub(crate) relay: Relay,

    /// im2 客户端对象是否在位（bind 过 get_input_method 且未销毁）。
    im2_bound: bool,

    /// 诊断计数：im2 `commit(serial)` 与 wire done 计数不符、但按协议照常
    /// 交付文本的批次数（sync7 起 serial 不符不再丢字）。status/回归测试用。
    serial_mismatch_commits: u32,

    /// 嵌套 IME daemon 生命周期 + 路由决策（imeNested auto|on|off）。
    pub(crate) nested: NestedIme,

    /// 观测网（角标原料）：预编辑流是否在场（置位=非空 preedit 批次 /
    /// im2 无内容 commit 心跳 / 候选窗新帧采集；清除=会话终结 / 失焦 /
    /// 候选窗内容真消失）。"中文引擎工作态"（协议不暴露引擎名，见
    /// popup_work_state）。只读镜像语义不变：不参与任何裁决。
    last_preedit_active: bool,

    /// 最近一次 fcitx5 种子 outcome 的引擎名列表（/wl ime info 透明化；
    /// 数据在 nested_tick 的种子分支就地留存——nested_daemon 种子逻辑不动）。
    seed_engines: Vec<String>,

    /// sync15 热路径仪表（先测量后修的测量半程）：keyboard_input 全程
    /// 直方图、submit_key_sync 等待、popup 交付经济学、按键路由分布、
    /// 10s 聚合行窗口簿记。全部单事件循环线程读写（日志 IO 计数在桥级
    /// relaxed 原子，跨线程安全但不进 ime_info）。
    pub(crate) metrics: metrics::ImeMetrics,

    /// 输入法数据来源偏好（settings 键 `imeSource`，默认 Auto）。
    /// Auto：fcitx5 在场/可用（nested 路由未判死）时**逐字节维持既有
    /// im2/host_bridge/raw 行为**；仅当 daemon 判定「未安装/连续失败」
    /// （NoBinary/Failed）时回退内置引擎。Fcitx5/Builtin 为强制档。
    ime_source: builtin::ImeSource,

    /// 上一次 handle_key 生效的来源裁决（fcitx5↔builtin 切换瞬间用于
    /// 清掉内置引擎的在途组合，杜绝孤儿 preedit）。
    last_effective_source: Option<builtin::ImeSource>,

    /// 内置拼音引擎（纯逻辑；键路由 seam 见 handle_key）。
    builtin: BuiltinEngine,

    /// 内嵌词库是否已尝试加载（WLCState::new 的 init 线程一次性；
    /// handle_key 里另有惰性兜底供测试/异常路径）。
    builtin_dict_ready: bool,
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

    // ── 候选窗 popup surface 采集（Phase 1，input_popup.rs 门面）──

    /// `zwp_input_method_v2.get_input_popup_surface` 落地：登记 input_popup 角色、
    /// 记住该 surface 以便 commit 时认领缓冲。由 input_method_v2.rs 调用。
    pub(crate) fn register_popup_role(
        &mut self,
        popup: &smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        self.popup.created(popup.id(), surface);
    }

    /// popup 角色对象销毁（`zwp_input_popup_surface_v2.destroy` 或客户端断连）：
    /// 仅当销毁的就是当前登记对象时解除采集；内容彻底终结（无 live 帧且
    /// 无 hold 接管）→ 组字证据清零（角标落）。
    pub(crate) fn clear_popup_role(
        &mut self,
        popup_id: smithay::reexports::wayland_server::backend::ObjectId,
    ) {
        self.popup.gone_for(popup_id);
        let now = self.popup.now();
        if !self.popup.has_content(now) {
            self.note_popup_content_gone();
        }
    }

    /// 可见性原料（焦点/激活态）变化时推进 epoch，让 Java 轮询感知
    /// "内容没变但可见性变了"（候选窗须即时显隐）。
    pub(crate) fn popup_touch(&mut self) {
        self.popup.touch();
    }

    /// 供 `input_popup::on_popup_commit` 采集帧后判定可见性用：IME 是否处于
    /// 激活会话（im2 端点在位且 relay 判定会话 active）。
    pub(crate) fn popup_ime_active(&self) -> bool {
        self.im2.instance.is_some() && self.relay.app_active()
    }

    /// 当前存活的 popup 底层 surface（Phase 3 点击回传目标；已销毁 → None）。
    pub(crate) fn popup_surface(&self) -> Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
        self.popup.surface().cloned()
    }

    /// 最近一次已知的聚焦应用 ti3 光标矩形（x, y, w, h，**该 surface 像素坐标**）。
    /// 世界形态候选窗的锚定原料（`ime_popup_anchor` JNI 只读出口）：
    /// push_state_events 在应用每次 ti3 commit 时同步（st.cursor_rect 原样写入），
    /// 应用未报光标时为 None——Java 侧回退锚窗口 quad 底部中央。
    /// 纯读零副作用：不消耗 epoch、不碰采集轨道（`ime_popup_frame` 的
    /// 13 字节头编码合同不受本通道影响）。
    pub(crate) fn popup_anchor(&self) -> Option<(i32, i32, i32, i32)> {
        self.im2.last_cursor_rect
    }

    /// 若销毁对象就是当前 popup 且其 surface 仍存活 → 返回它以补 pointer
    /// leave（client 侧 pointer 焦点一致性，见 seat::popup_pointer_leave）。
    pub(crate) fn popup_pointer_target(&self, popup_id: smithay::reexports::wayland_server::backend::ObjectId) -> Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
        if self.popup.role_matches(popup_id) {
            self.popup.surface().cloned()
        } else {
            None
        }
    }

    /// Java 每帧轮询候选窗帧（Phase 1 JNI 出口）。
    ///
    /// 返回 `None` 表示"无新状态，零拷贝"（seq 未变且可见性原料未变）；
    /// 返回 `Some(snapshot)` 时 snapshot 携带最新可见性与（可见时的）ARGB8888
    /// 像素。无 popup 角色时走极廉价路径（一次布尔比较即返回 None）。
    ///
    /// 可见性 = popup 角色在场 && 已采到帧 && IME 激活会话 && ti3 键盘焦点在
    /// && 游戏 focused（`kb_active`，Java keyboardActivate/Deactivate 维护）。
    /// 这是协议 activate/deactivate"popup 仅在 IM active 时可见"语义的本地
    /// 落实（v2 无显式 visible 事件，见 input_popup.rs 模块头）。
    /// Java 侧再叠加"聊天框 focused"等屏幕语境决定是否真的绘制。
    pub fn take_popup_snapshot(&mut self) -> Option<PopupSnapshot> {
        // display hold 到期结算（轮询驱动，无需定时器）：到期仍无新帧 →
        // 快照通道回落不可见（Java 隐藏），同时终结组字证据（角标落）。
        let now = self.popup.now();
        if self.popup.expire_hold(now) {
            self.note_popup_content_gone();
        }
        if !self.popup.changed() {
            return None;
        }
        // 可见性门 = 有内容（live 帧或 hold 暂存帧）&& IME 会话激活。
        //
        // sync11 生产根因修复：**不再用 has_focus / kb_active 做门**。
        // 那两个是「native 窗口键盘焦点 / 窗口捕获键盘 grab」信号——
        // 聊天框打字时没有 native 窗口被聚焦（focusSurface 只接受
        // ToplevelSurface），二者恒 false，导致聊天场景候选窗可见性恒 0
        // （用户四轮"采到帧但候选窗从不显示"的真正根因；last_visible_delivery
        // 偶尔有值只是用户开窗口捕获恰好撞上的巧合）。
        // 正确的会话语义由 popup_ime_active（im2 在位 && relay.app_active，
        // 即 ti3 enable/disable）承载，Activate/Deactivate 已 popup_touch
        // 推进 epoch；「用户是否在输入框打字」由 Java 侧 hasBox（EditBox
        // focused）门把守，native 不该重复且错误地用窗口信号再 gate 一次。
        let im2active = self.popup_ime_active();
        let visible = self.popup.has_content(now) && im2active;
        // sync15 交付去重闸：epoch 动了不等于 Java 能观测到的状态动了——
        // 每帧 focusSurface（keyboard_focus JNI）与 ti3 enable/disable 抖动
        // （Activate/Deactivate touch）都推进 epoch，但只要 (seq, visible)
        // 与上次交付一致，重新 clone 64KB + 编码 + JNI byte[] + Java 解码
        // 重传纹理就是纯浪费（真机 bench 实测：36 帧内容被交付 1423 次 /
        // 40MB JNI 字节 / 9.4s——sync14 "输入法特别卡" 的主通路之一）。
        // 去重轮以 None 消费 epoch：像素零拷贝、record_delivery/gate 日志
        // 都不计入（deliveries 计数从此等于真实状态转移数）。
        let seq_will = self.popup.delivery_seq(visible);
        if self.popup.delivery_redundant(seq_will, visible) {
            self.popup.consume_epoch();
            self.metrics.popup_dup_skipped += 1;
            return None;
        }
        let snap = self.popup.snapshot(visible);
        // 内容在场时的完整门快照（sync12 复盘盲区：status 只在空闲瞬间采样，
        // 抓不到"有帧时哪道门挡了"）。200ms 节流，一条日志直接指认。
        if snap.visible || (snap.width > 0 && snap.argb.is_some()) {
            self.popup.log_content_gate(
                self.popup.has_frame(),
                !self.popup.has_frame(),
                im2active,
                snap.visible,
                snap.seq,
                snap.width,
                snap.height,
            );
        }
        self.popup.mark_delivered(snap.visible);
        // 观测网（sync8 静默故障复盘）：交付侧记账——"native 采到了帧"
        // 与"Java 拿到了可见帧"必须可用数字切分，不再靠日志行序人肉对齐。
        self.popup.record_delivery(snap.visible);
        self.popup.note_delivery_fingerprint(snap.seq, snap.visible);
        Some(snap)
    }

    /// 候选窗状态简报（`/wl status` ime_candidate_popup 行 +
    /// `/wl ime info` 共用）。kb_active 由调用方传入（seat 与 ime 拆分借用）。
    pub fn popup_status_brief(&self, kb_active: bool) -> String {
        let last = match self.popup.last_visible_delivery_ago_secs() {
            Some(s) => format!("{s}s前"),
            None => "从未".to_string(),
        };
        // display hold 观测面（sync9 竞态修复判读）：held=暂存帧在窗口内，
        // remaining=距回落不可见的剩余 ms。latest.log 五元组的 visible 语义
        // 不变（快照交付合同原样）。
        let now = self.popup.now();
        let hold = match self.popup.hold_remaining_ms(now) {
            Some(ms) if self.popup.held_active(now) => format!("held=true remaining={ms}ms"),
            _ => "held=false".to_string(),
        };
        format!(
            "frames={} deliveries={} seq={} epoch={} delivered={} \
             gate=[role={} frame={} im2active={} focus={} kb={}] \
             hold=[{}] last_visible_delivery={}",
            self.popup.frames_captured(),
            self.popup.deliveries(),
            self.popup.seq_for_diag(),
            self.popup.epoch(),
            self.popup.delivered(),
            self.popup.surface().is_some(),
            self.popup.has_frame(),
            self.popup_ime_active(),
            self.has_focus,
            kb_active,
            hold,
            last,
        )
    }

    /// 中文引擎工作态（角标原料）：会话激活 + 预编辑流在场。信号源
    /// （sync9 根因修复后，弹窗/客户端预编辑两种形态都覆盖）：
    /// 1. 交付批次含非空 preedit（clientPreedit 模式）；
    /// 2. im2 无内容 commit 心跳（弹窗模式 updatePreeditDelegate——面板
    ///    在变而 clientPreedit 恒空）；
    /// 3. 候选窗采集到新帧（classicui 只在面板有内容时绘制）。
    /// 终结信号：Deactivate / clear_focus / 候选窗内容真消失（无 live 帧
    /// 且 hold 已到期）。
    /// **协议限制**：im2 不暴露引擎名，语义=「有预编辑/候选流的
    /// 工作态」而非「引擎=拼音」；keyboard-us 直通永不产生预编辑 → 无角标。
    pub fn popup_work_state(&self) -> bool {
        self.popup_ime_active() && self.last_preedit_active
    }

    /// 候选窗采到新帧 → 组字证据置位（input_popup::on_popup_commit 调用）。
    pub(crate) fn note_popup_frame_activity(&mut self) {
        self.last_preedit_active = true;
    }

    /// 候选窗内容真消失（attach(NULL) 已交付帧 / 角色解除且无 hold /
    /// hold 到期）→ 组字证据清零。
    pub(crate) fn note_popup_content_gone(&mut self) {
        self.last_preedit_active = false;
    }

    /// `/wl ime info` 多行报告：nested、种子引擎、候选窗统计、工作态、
    /// 路由决策原料、serial 诊断计数。全部纯读，零副作用。
    pub fn ime_info(&self, kb_active: bool) -> String {
        use crate::ime::NestedPhase;
        let engines = if self.seed_engines.is_empty() {
            "(尚未种植/未 spawn fcitx5)".to_string()
        } else {
            self.seed_engines.join(", ")
        };
        let route = if self.effective_source() == builtin::ImeSource::Builtin {
            "builtin"
        } else if self.nested_route_live() {
            if self.keyboard_grabbed() { "im2-grab" } else { "im2" }
        } else {
            "host_bridge/raw 回退"
        };
        let phase = match self.nested.phase() {
            NestedPhase::Active => "active",
            NestedPhase::Starting => "starting",
            NestedPhase::Disabled => "disabled",
            NestedPhase::Unconfigured => "unconfigured",
            NestedPhase::NoBinary => "no_binary",
            NestedPhase::Failed(_) => "failed",
        };
        let mut out = String::new();
        out.push_str(&format!("来源: {}\n", self.builtin_status_brief()));
        out.push_str(&format!("nested: {} (phase={phase} route={route})\n", self.nested_status_line()));
        out.push_str(&format!("已种引擎(种子 outcome): {engines}\n"));
        out.push_str(&format!("会话: app_active={} focus={} grab={} serial_mismatch={}\n",
            self.relay.app_active(), self.has_focus, self.keyboard_grabbed(), self.serial_mismatch_commits));
        out.push_str(&format!("候选窗: {}\n", self.popup_status_brief(kb_active)));
        // 世界形态锚定原料（sync14）：最近一次 ti3 光标矩形（聚焦应用 surface
        // 像素坐标）。None/0x0 = 应用尚未上报光标位置 → Java 世界渲染回退锚
        // 窗口 quad 底部中央（锚定模式判读见 Java 侧 /wl ime info 的 anchor= 行）。
        out.push_str(&format!(
            "光标矩形(ti3): {}\n",
            match self.im2.last_cursor_rect {
                Some((x, y, w, h)) => format!("({x},{y}) {w}x{h}"),
                None => "None（应用未上报 set_cursor_rectangle）".to_string(),
            }
        ));
        out.push_str(&format!("引擎工作态(预编辑流在场): {} —— 协议不暴露引擎名，此为「中文引擎工作态」判定\n", self.popup_work_state()));
        // sync15 延迟段：keyboard_input / submit_key_sync / popup 交付的
        // 直方图读数 + 按键路由分布 + 候选窗交付经济学。纯读，累计值。
        out.push_str(&self.metrics.format_latency_section(metrics::PopupDeliveryCounts {
            frames: self.popup.frames_captured() as u64,
            deliveries: self.popup.deliveries() as u64,
        }));
        out
    }

    /// sync15：由 `WaylandCraft::update()` 每帧驱动的热路径仪表推进。
    ///
    /// 职责：`update_rounds` 计数 + 满 10s 写一行聚合 debug 日志（无定时器，
    /// 轮询驱动同 expire_hold 风格）。日志经 `bridge::ime_log_write`，顺带
    /// 被 Phase C 的缓冲/重复合并覆盖（本行内容独立，不会被相邻合并吞掉——
    /// 内容含窗口计数，每窗口变化）。
    pub fn metrics_tick(&mut self, now: std::time::Instant) {
        let popup = metrics::PopupDeliveryCounts {
            frames: self.popup.frames_captured() as u64,
            deliveries: self.popup.deliveries() as u64,
        };
        if let Some(line) = self.metrics.tick_window_report(now, popup, &crate::bridge::IME_LOG_IO) {
            crate::bridge::ime_log_write(&line);
            // 心跳落盘点：缓冲日志的最坏可见延迟 = 窗口周期（Phase C 合同）。
            crate::bridge::ime_log_flush();
        }
    }

    /// 是否有激活的文本输入会话（relay 是唯一事实源，ti3 enable 驱动）。
    pub fn app_active(&self) -> bool {
        self.relay.app_active()
    }

    /// serial 不符但照常交付文本的批次数（诊断/status 用）。
    pub fn serial_mismatch_count(&self) -> u32 {
        self.serial_mismatch_commits
    }

    /// v0.13.4：当前是否有 ti3 / keyboard 焦点（status.rs 用）。
    pub fn has_focus(&self) -> bool {
        self.has_focus
    }

    /// im2 grab 是否抓走了键盘（抓走期间原始按键只发给 IME）。
    pub fn keyboard_grabbed(&self) -> bool {
        self.im2.grab.is_some()
    }

    /// 嵌套 IM 主路径是否在线（路由决策核心，⑥ 决策行见 nested_daemon）：
    /// mode≠off、未被用户 stop 锁 Disabled、im2 客户端在位、relay 认为
    /// IME 端点上线。bridge.rs keyboard_input 用它决定
    /// 「键送 im2 通道（grab 在场）/ raw 直通（无 grab）」还是
    /// 「回退 v0.13 host_bridge consumed 分流」。
    pub fn nested_route_live(&self) -> bool {
        self.nested.mode() != NestedMode::Off
            && !matches!(self.nested.phase(), NestedPhase::Disabled)
            && self.im2_bound
            && self.relay.ime_present()
    }

    /// 每帧由 WaylandCraft::update() 调用：驱动 daemon 探活/退避/spawn 决策。
    /// spawn 在主线程同步执行（双 fork 后立即返回，无阻塞等待）。
    pub fn nested_tick(&mut self, now: std::time::Instant, socket: &std::ffi::OsStr) {
        self.nested.set_socket(socket);
        let im2_bound = self.im2_bound;
        if let Some(req) = self.nested.decide(now, im2_bound, nested_daemon::proc_comm_matches) {
            let kind = req.kind;
            // fcitx5 spawn 前种 v2 默认配置（全部已装引擎 + ActiveByDefault
            // + Ctrl+Shift+B 枚举热键；所有权标记判定重写/保留）。根因：
            // 无 [Behavior] ActiveByDefault=True 时隔离实例每个输入上下文
            // 都以 keyboard-us 直通启动——拼音引擎根本没挂上（取证链见
            // nested_daemon::seed_fcitx5_home_v2）。
            if kind == nested_daemon::DaemonKind::Fcitx5 {
                if let Some(home) = self.nested.home_path() {
                    let outcome = nested_daemon::seed_fcitx5_home_v2(
                        home,
                        &nested_daemon::SeedPaths::from_process_env(),
                    );
                    self.nested.note_seed_engines(&outcome);
                    // /wl ime info 透明化：引擎名列表就地留存（种子逻辑不动）。
                    self.seed_engines = outcome.engines.clone();
                    for note in &outcome.notes {
                        crate::bridge::ime_log_write(&format!(
                            "[waylandcraft][ime][nested] 种子: {note}"
                        ));
                    }
                }
            }
            let result = nested_daemon::spawn_daemon(&req.plan);
            self.nested.note_spawn_result(kind, result, now);
        }
    }

    /// `/wl ime nested [status|start|stop]` 入口（JNI 桥调用）。
    pub fn nested_command(&mut self, sub: &str, now: std::time::Instant) -> Option<String> {
        match sub {
            "status" => Some(self.nested_status_line()),
            "start" => Some(self.nested.request_start(now)),
            "stop" => Some(self.nested.request_stop()),
            _ => None,
        }
    }

    /// status 汇总行（status.rs 子系统 + /wl ime nested status 共用）。
    pub fn nested_status_line(&self) -> String {
        self.nested
            .status_line(self.im2_bound, self.im2.grab.is_some(), self.vks.count())
    }

    /// settings 注入：mode 字符串（auto|on|off），非法返回 false 不改状态。
    pub fn nested_set_mode(&mut self, raw: &str) -> bool {
        self.nested.set_mode(raw)
    }

    /// settings 注入：隔离 HOME（<gameDir>/waylandcraft/ime-nested）。
    pub fn nested_set_home(&mut self, path: &std::path::Path) {
        self.nested.set_home(path);
    }

    /// daemon 相位的稳定短名（status.rs 用，避免 Display Debug 泄漏到协议面）。
    pub fn nested_phase_name(&self) -> &'static str {
        match self.nested.phase() {
            NestedPhase::Disabled => "disabled",
            NestedPhase::Unconfigured => "unconfigured",
            NestedPhase::NoBinary => "no_binary",
            NestedPhase::Starting => "starting",
            NestedPhase::Active => "active",
            NestedPhase::Failed(_) => "failed",
        }
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
        // sync15：Java 渲染循环每帧调 focusSurface（WaylandCraft.ensureKeyboard
        // Focus "tick"），幂等地重复聚焦同一 surface 也照常进来。旧实现每帧
        // popup_touch 推 epoch——配合交付端的同帧重交付，构成 60fps 的全量
        // 复制风暴。可见性公式（sync11 起）已不含 has_focus/surface 身份，
        // 焦点原料只在**首次获得**时推一次（内容/激活态变化另有触点；交付
        // 端指纹去重兜底其余触点），同 surface 重复 set_focus 不再推 epoch。
        if !self.has_focus {
            self.popup_touch();
        }
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
        // v0.14.1 路由门收紧：嵌套主路径在线时**不发**——宿主输入法
        // （GNOME/ibus）收到 FocusIn 会弹自己的候选窗/状态栏抢焦点，
        // 实测（sync6 ime.log）是"嵌套已激活但宿主仍持续 FocusIn/Out"
        // 的干扰源。daemon 掉线回退时下一次 set_focus / ti3 enable 会
        // 重新补热宿主状态（submit 幂等）。
        if let Some(hb) = state.host_bridge.as_mut() {
            if hb.is_ready() {
                if self.nested_route_live() {
                    crate::bridge::ime_log_write(
                        "[waylandcraft][ime][host_bridge] FocusIn 跳过（nested 主路径在线，路由门收紧）"
                    );
                } else {
                    crate::bridge::ime_log_write(&format!(
                        "[waylandcraft][ime][host_bridge] FocusIn（set_focus 路径，switched={}）",
                        switched
                    ));
                    hb.submit(DownEvent::State(FocusChange::Activate));
                }
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
            // host_bridge FocusOut（后备路径）。路由门收紧：nested 主
            // 路径在线时不发（宿主本就不该跟踪我们的焦点；见 set_focus
            // 同款注释）。
            if let Some(hb) = state.host_bridge.as_mut() {
                if hb.is_ready() {
                    if self.nested_route_live() {
                        crate::bridge::ime_log_write(
                            "[waylandcraft][ime][host_bridge] FocusOut 跳过（nested 主路径在线，路由门收紧）"
                        );
                    } else {
                        hb.submit(DownEvent::State(FocusChange::Deactivate));
                        crate::bridge::ime_log_write(&format!(
                            "[waylandcraft][ime][host_bridge] FocusOut（ti3 焦点离开）"
                        ));
                    }
                }
            }
        }
        // 始终复位内部状态——clear_focus = 强制 IME 不活跃。
        // sync7 修复：这里曾直接丢弃返回值——ti3 层无聚焦但 relay 会话仍
        // active 时，Deactivate 命令（含 wire done）被扔掉的形态正是当年
        // 影子计数「记了数但 wire 没发」的漂移来源之一。必须照常执行。
        // sync15：焦点原料真实 false→? 变化才推 epoch——生产聊天场景无
        // 聚焦窗口时 WaylandCraft.update 每帧 focusSurface(null) 走到这里，
        // 旧代码每帧 popup_touch = 每帧空转交付（配合 set_focus 同款修复）。
        let had_focus = std::mem::replace(&mut self.has_focus, false);
        let cmds = self.relay.focus_lost();
        // 失焦 = 组合流终结兜底（focus_lost 在无激活会话时不产 Deactivate，
        // exec_on_im2 的复位分支会被跳过——这里统一清工作态角标原料）。
        self.last_preedit_active = false;
        self.execute_ime_commands(cmds);
        if had_focus {
            self.popup_touch(); // 可见性原料（ti3 焦点）真实消失 → 轮询端感知
        }
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
        // 对象换代：wire 计数从 0 起步（sync7 漂移根因之二——旧实现里 relay
        // 影子计数跨代际保留，新对象收到的 commit serial 从此永远对不上）。
        self.relay.mirror_done_count(self.im2.done_count());
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
        // popup 角色随 input_method 对象一并销毁（协议 destroy 语义：input_method
        // 销毁即销毁其 popup 子对象）：解除采集轨道、让 Java 轮询看到消失
        // （未交付帧仍走 hold 窗口；内容真终结 → 角标原料清零）。
        self.popup.cleared();
        let now = self.popup.now();
        if !self.popup.has_content(now) {
            self.note_popup_content_gone();
        }
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
    ///
    /// v0.14.1 路由门收紧：**nested 主路径在线时完全跳过 host_bridge**
    /// （FocusIn/FocusOut/Surrounding/CursorRect 一律不发）。实机取证
    /// （sync6 ime.log 14:34+）：嵌套 daemon 已 Active、按键走 im2，
    /// 但 dbus-ibus 的 FocusIn/FocusOut 仍在持续进宿主会话——宿主
    /// GNOME/ibus 会因此弹自己的候选 UI、维持焦点状态，干扰用户并让
    /// 回退瞬间的宿主状态语义含糊。daemon 掉线回退后 nested_route_live
    /// 变 false，下一次 ti3 事件即恢复 v0.13 的宿主喂法（焦点切窗口
    /// /enable 抖动都会重发，状态重新补热）。
    pub(crate) fn apply_ti3_outcome(state: &mut crate::WLCState, outcome: Ti3CommitOutcome) {
        let ime = &mut state.ime;
        let nested_live = ime.nested_route_live();
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
                // v0.14.1 路由门收紧：nested 主路径在线时整段跳过。
                if nested_live {
                    crate::bridge::ime_log_write(
                        "[waylandcraft][ime] apply_ti3_outcome Enabled: host_bridge 提交跳过（nested 主路径在线，路由门收紧）"
                    );
                } else if let Some(hb) = hb {
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
                // 路由门收紧：nested 在线时不向宿主发 FocusOut。
                if !nested_live {
                    if let Some(hb) = hb {
                        if hb.is_ready() {
                            hb.submit(DownEvent::State(FocusChange::Deactivate));
                        }
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
                // 路由门收紧：nested 在线时不再向宿主喂 Surrounding/
                // 光标矩形（这类提交在 sync6 实机日志里与 FocusIn/Out
                // 一同泄漏，宿主 ibus 因此维持热焦点状态）。
                if !nested_live {
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
                }
                cmds
            }
        };
        ime.execute_ime_commands(cmds);
    }

    // ── im2 commit(serial) 落地（input_method_v2.rs 的 Dispatch 调用）──

    /// IME 端点经 im2 wire 发来 commit(serial)：对照 wire 层当前
    /// zwp_input_method_v2 对象的实际已发 done 数（单一事实源，
    /// `InputMethodV2State::done_count`，新代际从 0 起步）校验后落地。
    ///
    /// sync7 修复（实机 51/51 汉字全丢根因）：
    /// - 旧逻辑用 relay 的乐观影子计数做严格相等判定，计数一旦漂移
    ///   （命令被丢弃未执行 / 对象换代 wire 归零 / 实例未 bind）后续
    ///   所有 commit 整批 DROPPED——且「serial 不符即丢文本」本身违反
    ///   协议（input-method-unstable-v2：mismatch 时 "proceed as normal,
    ///   except it should not change the current state"）。
    /// - 现在：有内容批次**一律下发**（preedit/delete/commit + ti3 done）；
    ///   serial 不符仅跳过状态确认语义（对本层近似无操作），并出双计数
    ///   WARN（ime_log + ⑥ error ring 相邻去重），绝不静默丢字。
    pub(crate) fn ime_commit_from_wire(&mut self, serial: u32) {
        let done_sent = self.im2.done_count();
        let fr = self.relay.ime_commit(serial, done_sent);
        if !fr.flushed {
            // 无内容裸 commit：fcitx5 updatePreeditDelegate 面板为空时
            // 也照发 commit(serial)（waylandimserverv2.cpp），属正常心跳，
            // 与 sync7 时代混在 DROPPED 文案里的真丢字是两回事。
            //
            // 角标原料（sync9 根因的另一半）：clientPreedit 为空但面板在变
            // = 弹窗模式组字中——这是生产预编辑流唯一的 im2 线级证据
            // （updatePreeditDelegate 每次面板更新都以 commit(serial) 收尾；
            // preedit 像素走 popup 采集轨道，不经过本批处理点）。
            if self.popup_ime_active() {
                self.last_preedit_active = true;
            }
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][relay] im2 commit(serial={serial}) 无内容（wire done={done_sent}），忽略"
            ));
            return;
        }
        if !fr.applied {
            // serial 与 wire 计数不符：协议要求照常处理文本，
            // 只是不改变该 im2 对象 current state（本层无额外状态依赖它）。
            self.serial_mismatch_commits += 1;
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][relay] WARN im2 commit(serial={serial}) ≠ wire done 计数({done_sent})：文本照常交付，本批不做状态确认"
            ));
            crate::status::note_error(
                "warn",
                &format!(
                    "[ime][serial] im2 commit serial={serial} wire_done={done_sent} 不符：文本已照常交付，未确认对象当前状态"
                ),
            );
        }
        self.state.transition(ImeStateMachine::Committing);
        self.emit_ti_batch(fr.commands);
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
        // 观测网：批次实际交付 → 记录"引擎工作态"原料（非空预编辑在场）。
        // 放在 DROPPED/无实例早退之后，保证 flag 反映真实交付给应用的形态。
        //
        // sync9 根因修复：**只置位、不清除**。旧实现每批无条件赋值
        // `any(非空 Preedit)`，而生产弹窗模式（fcitx5 classicui 绘制
        // preedit，clientPreedit 恒空）下能到达这里的批次只有
        // commit_string/delete_surrounding（选字上屏、退格）——flag 被
        // 这些"输出批次"反复清零，组字期间角标永不出现（用户实锤
        // workState=false 且 commit 正常）。清除改由显式终结信号负责：
        // Deactivate（会话终结）、clear_focus（失焦兜底）、候选窗内容
        // 真消失（note_popup_content_gone / hold 到期）。
        if commands.iter().any(|c| matches!(c, TiCommand::Preedit(t, _, _) if !t.is_empty())) {
            self.last_preedit_active = true;
        }
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
                self.send_im2_done();
                // 协议：activate 使全部现有 popup 可见 → 推进采集轨道 epoch，
                // 让 Java 轮询即时反映候选窗显隐（v2 无显式 visible 事件）。
                self.popup_touch();
            }
            ImeCommand::Deactivate => {
                if let Some(inst) = &self.im2.instance {
                    inst.obj.deactivate();
                }
                self.send_im2_done();
                // 协议：deactivate 必须使全部现有 popup 不可见 → 同上。
                self.popup_touch();
                // 会话终结 = 组合流终结：工作态角标原料必须同步复位
                //（否则下次 activate 到第一个新批次之间角标虚假在场）。
                self.last_preedit_active = false;
            }
            ImeCommand::PushState(st) => {
                self.im2.push_state_events(&st);
                self.send_im2_done();
            }
        }
    }

    /// wire 层发 done（计数唯一事实源在 `InputMethodV2State.done_count`），
    /// 随后把实况同步进 relay 的只读诊断镜像。无实例时 send_done 无操作，
    /// 镜像同样保持 wire 实况——relay 侧不再有乐观自增，漂移无从产生。
    fn send_im2_done(&mut self) {
        self.im2.send_done();
        let done = self.im2.done_count();
        self.relay.mirror_done_count(done);
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
        // ── ime_source 裁决（唯一新增 seam；Fcitx5/Auto-fcitx 在场分支原样）──
        let effective = self.effective_source();
        if self.last_effective_source != Some(effective) {
            self.last_effective_source = Some(effective);
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][source] 路由切换 → {effective:?}（source={:?}）",
                self.ime_source
            ));
            // 切离内置档：在途组合立即终结（否则 fcitx5 接手后残留 preedit
            // 永远没人清）。
            if effective != builtin::ImeSource::Builtin
                && self.builtin.cancel()
            {
                self.emit_ti_batch(vec![TiCommand::Preedit(String::new(), 0, 0)]);
            }
        }
        if effective == builtin::ImeSource::Builtin {
            return self.builtin_key(key, action, mods);
        }
        // 路由裁决：mode=off / 用户 stop 后即使残留 grab 对象（daemon 断连
        // 清理要等一轮 dispatch）也绝不把键交给它。
        if !self.nested_route_live() {
            return false;
        }
        let Some(grab) = &self.im2.grab else {
            return false;
        };
        let serial = new_serial();
        let wire = key.saturating_sub(8);
        grab.key(serial, get_time(), wire, action.key_state());
        grab.modifiers(serial, mods.0, mods.1, mods.2, mods.3);
        true
    }

    // ── 内置引擎 seam（builtin/ 模块的门面接入点）──

    /// 当前配置的来源偏好（status/ime_info 用）。
    pub fn ime_source(&self) -> builtin::ImeSource {
        self.ime_source
    }

    /// 实际生效的来源：Auto 时 fcitx5 嵌套 daemon 判死（未安装/连续失败）
    /// 才落内置；Unconfigured/Starting/Active/Disabled 一律维持既有行为
    /// （逐字节回退红线：既有 fcitx5/host_bridge/raw 逻辑不被内置档触碰）。
    pub fn effective_source(&self) -> builtin::ImeSource {
        match self.ime_source {
            builtin::ImeSource::Fcitx5 => builtin::ImeSource::Fcitx5,
            builtin::ImeSource::Builtin => builtin::ImeSource::Builtin,
            builtin::ImeSource::Auto => {
                let dead = matches!(
                    self.nested.phase(),
                    NestedPhase::NoBinary | NestedPhase::Failed(_)
                );
                if dead {
                    builtin::ImeSource::Builtin
                } else {
                    builtin::ImeSource::Fcitx5
                }
            }
        }
    }

    /// init 线程一次性加载内嵌词库（WLCState::new 调用；µs~ms 级，不占 tick）。
    pub fn init_builtin_dict(&mut self) {
        if self.builtin_dict_ready {
            return;
        }
        let t = std::time::Instant::now();
        match builtin::embedded_dict() {
            Some(d) => {
                let (syl, nodes, terms) = d.stats();
                let micros = t.elapsed().as_micros();
                self.builtin.set_dict(Some(d));
                self.builtin_dict_ready = true;
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][builtin] 内嵌拼音词库就绪: {terms} 词条 / {syl} 音节 / {nodes} 节点, parse {micros}µs（零拷贝）"
                ));
            }
            None => {
                crate::bridge::ime_log_write(
                    "[waylandcraft][ime][builtin] 内嵌词库解析失败——内置引擎不可用（构建异常）",
                );
                crate::status::note_error("error", "[ime][builtin] 内嵌拼音词库解析失败");
            }
        }
    }

    /// 内置档按键处理：仅当存在激活的 ti3 会话才接管（游戏操控零干扰）。
    /// commit/preedit 复用 relay 原子缓冲 → emit_ti_batch 既有出口。
    fn builtin_key(
        &mut self,
        key: u32,
        action: KeyboardAction,
        mods: (u32, u32, u32, u32),
    ) -> bool {
        if !self.relay.app_active() {
            if self.builtin.cancel() {
                self.emit_ti_batch(vec![TiCommand::Preedit(String::new(), 0, 0)]);
            }
            return false;
        }
        // 命令行豁免：应用光标上下文以 `/` 开头（MC 指令栏形态）→ 字母
        // 直通，防内置档吃掉 "/wl …" 之类的命令输入。
        if self.relay.current_state().surrounding_text.trim_start().starts_with('/') {
            if self.builtin.cancel() {
                self.emit_ti_batch(vec![TiCommand::Preedit(String::new(), 0, 0)]);
            }
            return false;
        }
        if !self.builtin_dict_ready {
            self.init_builtin_dict();
        }
        let press = matches!(action, KeyboardAction::Press | KeyboardAction::Repeat);
        let result = self.builtin.on_key(key, press, mods.0);
        if !result.ops.is_empty() {
            let ops = result.ops;
            for op in ops {
                self.relay.ime_op(op);
            }
            // 内置引擎没有 im2 serial 通道（非协议端点）：等同宿主穿透的
            // Done 语义——无条件原子应用（serial 防线不存在，批次由本层
            // 同步构造，天然成序）。
            let fr = self.relay.ime_flush();
            if fr.flushed {
                self.emit_ti_batch(fr.commands);
            }
        }
        if !self.builtin.composing() {
            // 内置档无 popup 像素轨道：组字证据由引擎组合态直接维护。
            self.last_preedit_active = false;
        }
        result.consumed
    }

    /// 内嵌词库是否已就绪（status/ime_info 用；纯读）。
    pub fn builtin_dict_ready_probe(&self) -> bool {
        self.builtin.has_dict()
    }

    /// Java 候选面板轮询：内置引擎快照 JSON；`None` = 无变化（轻量合同）。
    pub fn take_builtin_snapshot(&mut self) -> Option<String> {
        self.builtin.snapshot_json()
    }

    /// 候选面板点选（页内下标）。返回提交文本（空 = 无变化）。
    pub fn builtin_select(&mut self, page_index: usize) -> String {
        if !self.relay.app_active() {
            return String::new();
        }
        let result = self.builtin.ui_select_page_index(page_index);
        if !result.consumed {
            return String::new();
        }
        let mut committed = String::new();
        for op in result.ops {
            if let crate::ime::relay::ImeOp::CommitString(t) = &op {
                committed = t.clone();
            }
            self.relay.ime_op(op);
        }
        let fr = self.relay.ime_flush();
        if fr.flushed {
            self.emit_ti_batch(fr.commands);
        }
        self.last_preedit_active = false;
        committed
    }

    /// 候选面板翻页（delta=±1）。返回是否实际翻页。
    pub fn builtin_page(&mut self, delta: i32) -> bool {
        let r = self.builtin.ui_page_step(delta);
        r.consumed
    }

    /// `/wl ime source auto|fcitx5|builtin`：改配置即时生效；非法值拒绝。
    pub fn set_ime_source(&mut self, raw: &str) -> Result<String, String> {
        let Some(src) = builtin::ImeSource::parse(raw) else {
            return Err(format!("非法来源 {raw:?}（合法 auto|fcitx5|builtin）"));
        };
        let prev = std::mem::replace(&mut self.ime_source, src);
        if src != builtin::ImeSource::Builtin && self.builtin.cancel() {
            self.emit_ti_batch(vec![TiCommand::Preedit(String::new(), 0, 0)]);
        }
        self.last_effective_source = Some(self.effective_source());
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime][source] {prev:?} → {src:?}（effective={:?}）",
            self.effective_source()
        ));
        Ok(format!(
            "imeSource = {}（effective={}）",
            src.as_str(),
            self.effective_source().as_str()
        ))
    }

    /// `/wl ime eng`：引擎清单如实上报（内置现仅拼音；fcitx5 侧为种子引擎）。
    pub fn ime_eng_list(&self) -> String {
        let dict = match self.builtin.dict_stats() {
            Some((syl, _nodes, terms)) => format!("拼音（内嵌 v1 词库: {terms} 词条 / {syl} 音节）"),
            None if self.builtin_dict_ready => "拼音（词库损坏，不可用）".to_string(),
            None => "拼音（词库未加载）".to_string(),
        };
        let mut out = String::new();
        out.push_str(&format!("内置: {dict}\n"));
        out.push_str(&format!("内置语言档: {}（裸 Shift 切换）\n", if self.builtin.english_mode() { "西文" } else { "拼音" }));
        if self.seed_engines.is_empty() {
            out.push_str("fcitx5: （daemon 未种子/未启动）\n");
        } else {
            out.push_str(&format!("fcitx5: {}\n", self.seed_engines.join(", ")));
        }
        out
    }

    /// status.rs / ime_info 共用的一行简报（纯读）。
    pub fn builtin_status_brief(&self) -> String {
        let dict = match self.builtin.dict_stats() {
            Some((_syl, _n, terms)) => format!("dict={terms}词条"),
            None => "dict=未加载".to_string(),
        };
        let comp = if self.builtin.composing() {
            format!(
                "组合={} 候选={} 页={}/{}",
                self.builtin.buffer(),
                self.builtin.candidate_count(),
                self.builtin.page() + 1,
                self.builtin.page_count().max(1)
            )
        } else {
            "空闲".to_string()
        };
        format!(
            "source={} effective={} mode={} {dict} {comp}",
            self.ime_source.as_str(),
            self.effective_source().as_str(),
            if self.builtin.english_mode() { "en" } else { "cn" },
        )
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
mod facade_tests {
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

    // ── 观测面（sync8 静默故障复盘）：工作态 / popup 简报 / ime_info ──

    #[test]
    fn popup_work_state_requires_active_session_and_preedit() {
        // 默认态：无 im2 端点 → 工作态必须 false（keyboard-us/未激活无角标）。
        let mut ime = ImeState::default();
        assert!(!ime.popup_work_state());
        // 会话 enable 但 im2 端点不在 → 仍 false（popup_ime_active 门）。
        ime.relay.set_app_enabled(true, "test");
        assert!(!ime.popup_work_state());
    }

    #[test]
    fn popup_anchor_reads_last_cursor_rect() {
        // 世界形态锚定原料（sync14）：popup_anchor 是 im2.last_cursor_rect 的
        // 只读镜像——默认 None（应用还没发 set_cursor_rectangle），置值后原样
        // 透出；纯读不推进任何 epoch/计数（ime_popup_frame 编码合同零影响）。
        let mut ime = ImeState::default();
        assert_eq!(ime.popup_anchor(), None);
        ime.im2.last_cursor_rect = Some((12, 30, 1, 18));
        assert_eq!(ime.popup_anchor(), Some((12, 30, 1, 18)));
        // 归零矩形原样透出（0x0 由 Java 侧判为无效并回退窗口底部锚）。
        ime.im2.last_cursor_rect = Some((0, 0, 0, 0));
        assert_eq!(ime.popup_anchor(), Some((0, 0, 0, 0)));
        assert_eq!(ime.popup.epoch(), 0, "锚点纯读不得动采集轨道");
        assert_eq!(ime.popup.deliveries(), 0);
    }

    #[test]
    fn ime_info_reports_cursor_rect_line() {
        // /wl ime info 锚定透明化：光标矩形行存在且反映当前值（sync14）。
        let mut ime = ImeState::default();
        let report = ime.ime_info(false);
        assert!(report.contains("光标矩形(ti3): None"), "{report}");
        ime.im2.last_cursor_rect = Some((5, 7, 2, 20));
        let report = ime.ime_info(false);
        assert!(report.contains("光标矩形(ti3): (5,7) 2x20"), "{report}");
    }

    #[test]
    fn popup_status_brief_lists_gate_materials() {
        // 简报必须包含采集/交付计数、epoch/delivered 与五道可见性门原料，
        // 让 "/wl status" 一行切分"native 采到帧"与"Java 收到可见帧"。
        let ime = ImeState::default();
        let brief = ime.popup_status_brief(true);
        assert!(brief.contains("frames=0"), "{brief}");
        assert!(brief.contains("deliveries=0"), "{brief}");
        assert!(brief.contains("epoch="), "{brief}");
        assert!(brief.contains("gate=["), "{brief}");
        assert!(brief.contains("kb=true"), "{brief}");
        assert!(brief.contains("last_visible_delivery=从未"), "{brief}");
    }

    #[test]
    fn ime_info_reports_all_sections() {
        // 报告分节齐全且为纯读（调用两次结果一致、不消耗任何状态）。
        let ime = ImeState::default();
        let a = ime.ime_info(true);
        let b = ime.ime_info(true);
        assert_eq!(a, b, "ime_info 必须零副作用");
        assert!(a.contains("nested:"), "{a}");
        assert!(a.contains("已种引擎"), "{a}");
        assert!(a.contains("候选窗:"), "{a}");
        assert!(a.contains("引擎工作态"), "{a}");
        assert!(a.contains("会话:"), "{a}");
    }

    // ── sync15 仪表：延迟段 + 窗口行（测量半程）──

    #[test]
    fn ime_info_includes_latency_section_pure_read() {
        // 延迟段存在且反映当前计数；两次调用一致（既有零副作用合同
        // 覆盖到新区块——刻意不读桥级全局，保证并行测试下也确定）。
        let mut ime = ImeState::default();
        ime.metrics.key_input.record_us(120);
        ime.metrics.popup_delivery.record_us(30_000);
        ime.metrics.route_im2_grab = 3;
        ime.metrics.keys_during_starting = 2;
        let a = ime.ime_info(false);
        let b = ime.ime_info(false);
        assert_eq!(a, b, "latency 段不得有副作用");
        assert!(a.contains("延迟 keyboard_input(全程)"), "{a}");
        assert!(a.contains("延迟 submit_key_sync(等待): n=0"), "{a}");
        assert!(a.contains("im2-grab=3"), "{a}");
        assert!(a.contains("Starting期(词典窗口)=2"), "{a}");
    }

    #[test]
    fn metrics_tick_counts_rounds_and_fires_window_line() {
        // update() 驱动：首轮建窗计数 + 到 10s 差分推进（窗口行经
        // ime_log_write 落日志——这里只验簿记与调用安全，行内容在
        // metrics.rs 单测覆盖）。
        let mut ime = ImeState::default();
        let t0 = std::time::Instant::now();
        ime.metrics_tick(t0);
        assert_eq!(ime.metrics.update_rounds, 1);
        ime.metrics_tick(t0 + std::time::Duration::from_secs(1));
        assert_eq!(ime.metrics.update_rounds, 2, "1s 不满窗不再出行");
        ime.metrics_tick(t0 + std::time::Duration::from_secs(11));
        assert_eq!(ime.metrics.update_rounds, 3);
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
