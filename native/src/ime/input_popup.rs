//! `zwp_input_popup_surface_v2` —— 输入法候选窗 surface 的角色登记、缓冲采集
//! 与帧队列（游戏内渲染候选窗的 native 半程，Phase 1）。
//!
//! ## 协议事实（input-method-unstable-v2，以官方 XML 为准）
//!
//! - popup 对象由 **IME 客户端**经 `zwp_input_method_v2.get_input_popup_surface
//!   (id, surface)` 创建；该 surface 被赋予 "input_popup" 角色；同一 surface
//!   二次赋予角色 → 协议错误（XML 无 error 枚举，沿用 KWin im2 实现
//!   code=1 "Role" 约定）。
//! - `zwp_input_popup_surface_v2` 本身只有一个事件（`text_input_rectangle`）
//!   和一个请求（`destroy`）。**v2 没有显式 `visible` 事件**——可见性由
//!   `zwp_input_method_v2` 的 activate/deactivate 语义蕴含（"activate 使全部
//!   现有 popup 可见；deactivate 必须使全部现有 popup 不可见"）。本模块把
//!   该语义落实为合成器内部状态：
//!   `popup_visible = popup 在场 && ti3 焦点在 && 游戏 focused && 会话激活
//!   && 已采到帧`，Java 每帧轮询取快照（Phase 2 overlay 显隐依据）。
//! - popup 的像素走**普通 wl_surface 请求**（attach/damage/commit）——
//!   smithay 的 compositor dispatch 照常更新 SurfaceAttributes，我们在
//!   `CompositorHandler::commit`（lib.rs 钩子）里认领该 surface 并拷缓冲。
//!
//! ## 取证（fcitx5 5.1.12 源码，/tmp/wc-ime-evidence）
//!
//! - 候选窗确实走 im2 popup surface：`src/ui/classic/waylandinputwindow.cpp`
//!   `WaylandInputWindow::update()` 对 frontend `wayland_v2` 的输入上下文调
//!   `im->getInputPopupSurface(window_->surface())`；隐藏时
//!   `attach(NULL)+commit` → 销毁 popup 对象 → 销毁 surface。
//! - 缓冲恒为 **wl_shm ARGB8888**（`waylandshmwindow.cpp` `newBuffer` →
//!   `WL_SHM_FORMAT_ARGB8888`，cairo 自绘），双缓冲、`wl_buffer.release`
//!   驱动下一帧重绘——因此采集后**必须 release**，否则 fcitx5 两帧后
//!   缓冲耗尽、候选窗冻结。
//! - 鼠标命中在引擎侧：`waylandpointer.cpp`（enter/motion→hover、
//!   button→click、axis 累积 ±2560 触发滚轮翻页）+ `inputwindow.cpp`
//!   （`candidateRegions_` 命中、`wheel()` 翻页）。合成器只需把
//!   wl_pointer 事件发到 popup surface（Phase 3），候选语义全由引擎完成。
//!
//! ## dmabuf 边界（如实记录）
//!
//! fcitx5 classicui 的候选窗**不走 dmabuf**（`WaylandUI::newWindow()` 恒
//! `WaylandShmWindow`，源码取证），本模块对非 shm 缓冲记日志并 release
//! （防引擎卡死），不做 GPU readback。现有 GL 基建（egl.rs dmabuf→EGLImage
//! →Java 纹理）服务于窗口合成轨道，与本 CPU 帧通道不同轨；若将来要支持
//! 自绘 dmabuf 候选窗的 IME 客户端，走 readback 或改投 WLCSurface 通道。
//! **TODO**: dmabuf popup → `egl.rs::dmabuf_to_image` + FBO readback。

use crate::WLCState;
use smithay::reexports::wayland_server::{
    backend::ObjectId,
    protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
    Resource,
};
use smithay::wayland::compositor::{with_states, BufferAssignment, SurfaceAttributes};
use smithay::wayland::shm::{with_buffer_contents, BufferAccessError};
use std::ops::DerefMut;

/// 采集到的一帧候选窗位图。像素布局 = wl_shm ARGB8888 的小端内存序
/// （每像素 4 字节 [B,G,R,A]，即 u32 0xAARRGGBB）——Java 侧用
/// `ByteBuffer.order(LITTLE_ENDIAN).getInt()` 逐像素解出标准 ARGB int。
#[derive(Clone)]
pub struct PopupFrame {
    pub width: i32,
    pub height: i32,
    /// width*height*4 字节。
    pub argb: Vec<u8>,
}

/// 显示保持窗口（display hold），单位 ms。
///
/// 定值依据（sync9 生产实锤）：
/// - 故障形态：fcitx5 classicui 在**同一个 wayland 事件循环回合**里完成
///   "attach 内容帧 commit → attach(NULL) commit（常伴角色更替）"，帧内容
///   生命周期 < Java 轮询间隔 → 采到的帧从未交付过就被清掉（status.log
///   frames=19 / deliveries=134278 / visible 恒 false）。400ms 相对帧间
///   空窗（通常 <1 个事件循环，即亚毫秒到几十毫秒）有 1~2 个数量级余量。
/// - 视觉代价：候选窗真实消失后最多残留 400ms——新帧到达立即覆盖重排
///   （seq 前进重传），常见场景用户无感；真隐藏（弹窗收起）只延迟
///   400ms 才不可见，处于"输入框旁边"的边缘视觉区，可接受。
/// - 轮询节奏：游戏 60fps ≈ 16.7ms/tick，400ms ≈ 24 个轮询周期，保证
///   每个真实采到的帧至少经历一次 visible=true 交付。
pub(crate) const POPUP_HOLD_MS: u64 = 400;

/// hold 期内继续交付的已采帧快照（帧内容 + 采到时的 seq，原样交付）。
struct HeldFrame {
    frame: PopupFrame,
    seq: u32,
}

/// popup 采集快照（JNI 轮询出口，bridge.rs 编码成 byte[] 给 Java）。
pub struct PopupSnapshot {
    pub width: i32,
    pub height: i32,
    /// 内容序号：每采到一帧 +1（seq 不变 = 无新帧）。
    pub seq: u32,
    /// 合成器裁决的可见性（协议 activate/focus 语义的本地落实，见模块头）。
    pub visible: bool,
    /// ARGB8888 字节流；`visible=false` 时为 None。
    pub argb: Option<Vec<u8>>,
}

/// popup 角色登记 + 帧状态。挂在 `ImeState.popup`。
#[derive(Default)]
pub struct InputPopupState {
    /// 当前 popup 角色对象（按 id 比对，旧 popup 对象的 destroyed 不误清新登记）。
    role: Option<ObjectId>,
    /// 持有 input_popup 角色的 surface。
    surface: Option<WlSurface>,
    /// 最近一次采到的帧（attach(NULL)/销毁后清）。
    frame: Option<PopupFrame>,
    /// 帧序号（内容变化计数）。
    seq: u32,
    /// 观察状态 epoch：内容、角色在场、帧在场任何变化都 +1。
    /// 焦点/激活侧由 [`ImeState`](super::ImeState) 在 set_focus/clear_focus/
    /// exec_on_im2/keyboard_activate 处 `touch()` 推进——可见性是这些状态
    /// 的纯函数，epoch 让轮询端能感知"内容没变但可见性变了"。
    epoch: u32,
    /// 上一次交付给 Java 的 epoch（"无变化零拷贝返回 null"的判据）。
    delivered: u32,
    /// 非 shm 缓冲告警一次性标志（防刷屏）。
    dmabuf_warned: bool,
    /// 诊断计数（观测网，`/wl status` + `/wl ime info` 消费）：
    /// 累计采到的帧数（note_frame 次数）。"native 采到帧"与"Java 收到
    /// 可见帧"之间的断链（生产 sync8 故障形态）靠这组数字切分。
    frames_captured: u32,
    /// 累计 JNI 交付次数（take_popup_snapshot 返回 Some 的次数）。
    deliveries: u32,
    /// 最后一次交付 visible=true 的时刻（Instant，仅相对时长用）。
    last_visible_delivery: Option<std::time::Instant>,
    /// 假时钟注入（测试免 sleep）：Some = 固定时刻；None = Instant::now。
    /// 见 [`Self::now`]——display hold 到期判定统一走这条时钟线。
    clock_override: Option<std::time::Instant>,
    /// display hold 内容（见 [`POPUP_HOLD_MS`]）：采到但**从未随
    /// visible=true 交付过**的帧在缓冲被回收后继续交付的暂存。
    /// 不变式：`frame.is_some()` 时 `held` 恒为 None（新帧顶替、
    /// 交付后再回收才会产生 held）。
    held: Option<HeldFrame>,
    /// held 的保持截止时刻（note_frame_removed/cleared/created 接管时
    /// 设为 now+POPUP_HOLD_MS；note_frame 置 None）。
    hold_until: Option<std::time::Instant>,
    /// 当前 live 帧自采到后是否已被快照交付过（mark_delivered 清除）。
    /// display hold 的判据：**只有从未交付的帧**在回收时进入 hold——
    /// 已交付过的帧 Java 已见其内容，attach(NULL) 是真实隐藏（协议语义），
    /// 不得伪造续显（sync9 的竞态吞帧只发生在"采到→回收 < 轮询间隔"
    /// 的从未交付形态；既有隐藏用例 popup_null_attach 依赖本区分）。
    frame_pending_deliver: bool,
}

impl InputPopupState {
    /// `get_input_popup_surface` 落地：登记角色、复位帧状态。
    /// 同角色域：若旧 live 帧尚未交付过（sync9 竞态形态里的"角色更替"），
    /// 先接管进 hold——新角色登记不得吞掉旧对象刚采到的内容。
    pub(crate) fn created(&mut self, role: ObjectId, surface: &WlSurface) {
        self.take_over_undelivered();
        self.role = Some(role);
        self.surface = Some(surface.clone());
        self.frame = None;
        self.seq = 0;
        self.epoch = self.epoch.wrapping_add(1);
        self.dmabuf_warned = false;
        crate::bridge::ime_log_write(
            "[waylandcraft][ime][popup] input_popup 角色登记（候选窗 surface 进入采集轨道）",
        );
    }

    /// 指定 popup 对象销毁：仅当它就是当前登记的对象才解除
    /// （IM 切上下文时旧 popup 的 destroyed 可能晚于新 popup 的 created）。
    pub(crate) fn gone_for(&mut self, role: ObjectId) {
        if self.role != Some(role) {
            return;
        }
        self.cleared();
    }

    /// IM 断连/顶替/destroy：清状态并让 Java 下次轮询看到"消失"。
    /// 从未交付过的 live 帧先接管进 hold（角色解除 ≠ 内容作废——
    /// sync9 竞态里 destroy 只是"同回合隐藏再显"的中间态；真实销毁时
    /// hold 到期后自然回落不可见）。
    pub(crate) fn cleared(&mut self) {
        if self.role.is_none() && self.surface.is_none() && self.frame.is_none() {
            return;
        }
        self.take_over_undelivered();
        self.role = None;
        self.surface = None;
        self.frame = None;
        self.seq = 0;
        self.epoch = self.epoch.wrapping_add(1);
        crate::bridge::ime_log_write("[waylandcraft][ime][popup] popup 角色解除（候选窗销毁/断连）");
    }

    /// 统一时钟入口：display hold 的到期判定、last_visible_delivery 的相对
    /// 时长都从这里取，杜绝 Instant::now 散落。测试经 [`Self::set_clock`]
    /// 注入假时钟做免 sleep 的到期断言。
    pub(crate) fn now(&self) -> std::time::Instant {
        self.clock_override.unwrap_or_else(std::time::Instant::now)
    }

    /// 测试注入假时钟（免 sleep）。生产代码不得调用。
    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, t: std::time::Instant) {
        self.clock_override = Some(t);
    }

    /// hold 仍在交付窗口内（有暂存帧且未到期）？
    pub(crate) fn held_active(&self, now: std::time::Instant) -> bool {
        self.held.is_some() && self.hold_until.is_some_and(|t| now < t)
    }

    /// 当前可交付内容是否存在：live 帧或 hold 暂存帧（可见性门的原料）。
    pub(crate) fn has_content(&self, now: std::time::Instant) -> bool {
        self.frame.is_some() || self.held_active(now)
    }

    /// 从未交付的 live 帧移入 hold（note_frame_removed/cleared/created 共用）。
    /// 已交付过的帧（frame_pending_deliver=false）是真实隐藏的起点——
    /// 协议语义照旧立即清，不伪造续显。
    fn take_over_undelivered(&mut self) {
        if self.frame.is_some() && self.frame_pending_deliver {
            let frame = self.frame.take().expect("just checked");
            self.held = Some(HeldFrame { frame, seq: self.seq });
            self.hold_until = Some(self.now() + std::time::Duration::from_millis(POPUP_HOLD_MS));
            self.frame_pending_deliver = false;
        }
    }

    /// 轮询驱动的 hold 到期结算：到期仍无新帧 → 清暂存、推进 epoch
    /// （下一次轮询交付 visible=false，Java 隐藏）。返回是否发生了到期清除
    /// （mod.rs 用它同步角标工作态）。
    pub(crate) fn expire_hold(&mut self, now: std::time::Instant) -> bool {
        if self.held.is_some() && !self.held_active(now) {
            self.held = None;
            self.hold_until = None;
            self.epoch = self.epoch.wrapping_add(1);
            return true;
        }
        false
    }

    /// surface 是否已被当前 popup 角色占用（协议二次赋角色判定 + commit 认领）。
    pub(crate) fn role_taken(&self, surface: &WlSurface) -> bool {
        self.owns(surface)
    }

    /// 指定 popup 对象是否就是当前登记对象（destroyed 时决定要不要
    /// 对 pointer 通道补 leave）。
    pub(crate) fn role_matches(&self, role: ObjectId) -> bool {
        self.role == Some(role)
    }

    /// 该 surface 是否就是 popup surface（commit 认领判据，极廉价：
    /// 无 popup 时只做一次 Option::is_none）。
    pub(crate) fn owns(&self, surface: &WlSurface) -> bool {
        match &self.surface {
            Some(s) => s.id() == surface.id(),
            None => false,
        }
    }

    pub(crate) fn surface(&self) -> Option<&WlSurface> {
        self.surface.as_ref().filter(|s| s.is_alive())
    }

    /// 采集到新帧。新帧顶替 hold 暂存（新内容优先于残影）；seq 单调推进
    /// （跨角色代际对齐 held 的旧 seq，保证 Java 侧"seq 变化 = 新内容"，
    /// 同 seq = 同内容免重传的合同不被代际重置打破）。
    pub(crate) fn note_frame(&mut self, width: i32, height: i32, argb: Vec<u8>) {
        let prev = self.seq.max(self.held.as_ref().map_or(0, |h| h.seq));
        self.held = None;
        self.hold_until = None;
        self.frame = Some(PopupFrame { width, height, argb });
        self.seq = prev.wrapping_add(1);
        self.frame_pending_deliver = true;
        self.epoch = self.epoch.wrapping_add(1);
        self.frames_captured = self.frames_captured.wrapping_add(1);
    }

    /// 缓冲移除（attach(NULL) = fcitx5 隐藏候选窗第一步）：
    /// - **从未交付过的帧**（同回合竞态形态）：移入 display hold，
    ///   hold 窗口内快照继续交付原帧（visible=true + 原 argb + 原 seq）；
    /// - 已交付过的帧：真实隐藏，立即清（既有协议语义不变）。
    /// 两种情况都推进 epoch（观察态已变）；角色保留（隐藏 ≠ 销毁）。
    pub(crate) fn note_frame_removed(&mut self) {
        if self.frame.is_none() {
            return;
        }
        self.take_over_undelivered();
        self.frame = None;
        self.epoch = self.epoch.wrapping_add(1);
        crate::bridge::ime_log_write("[waylandcraft][ime][popup] attach(NULL) → 帧清空（候选窗隐藏）");
    }

    pub(crate) fn has_frame(&self) -> bool {
        self.frame.is_some()
    }

    /// 可见性原料变化（焦点/激活/game focused）：推进 epoch。
    pub(crate) fn touch(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// 有未交付的变化？（轮询第一道闸：无变化直接 null，零像素拷贝）
    pub(crate) fn changed(&self) -> bool {
        self.delivered != self.epoch
    }

    /// 构造快照（不改 delivered——由 [`Self::mark_delivered`] 配对完成，
    /// 使"计算可见性 → 交付"顺序显式化）。
    ///
    /// display hold：live 帧不在、可见性门为真且 hold 未到期时，交付
    /// 暂存帧（原 argb、**原 seq**——Java 侧同 seq 免重传合同由
    /// ImeCandidateOverlay 的"非 null 即整帧重上传"实现兜底，见该文件
    /// uploadFrame；同内容重复上传无害）。不可见路径不携带 held 尺寸
    /// （`snapshot(false)` 的既有合同：无 live 帧即 0x0）。
    pub(crate) fn snapshot(&self, visible: bool) -> PopupSnapshot {
        match &self.frame {
            Some(f) if visible => PopupSnapshot {
                width: f.width,
                height: f.height,
                seq: self.seq,
                visible: true,
                argb: Some(f.argb.clone()),
            },
            Some(f) => PopupSnapshot {
                width: f.width,
                height: f.height,
                seq: self.seq,
                visible: false,
                argb: None,
            },
            None => match &self.held {
                Some(h) if visible && self.held_active(self.now()) => PopupSnapshot {
                    width: h.frame.width,
                    height: h.frame.height,
                    seq: h.seq,
                    visible: true,
                    argb: Some(h.frame.argb.clone()),
                },
                _ => PopupSnapshot {
                    width: 0,
                    height: 0,
                    seq: self.seq,
                    visible: false,
                    argb: None,
                },
            },
        }
    }

    pub(crate) fn mark_delivered(&mut self) {
        self.delivered = self.epoch;
        self.frame_pending_deliver = false;
    }

    /// 观测网：take_popup_snapshot 每返回一个 Some(snap) 调用一次，记账
    /// 交付次数与"最后一次可见交付"时刻（排错时区分"native 采到了但
    /// Java 从未见 visible"——sync8 生产故障的核心切分点）。
    pub(crate) fn record_delivery(&mut self, visible: bool) {
        self.deliveries = self.deliveries.wrapping_add(1);
        if visible {
            self.last_visible_delivery = Some(self.now());
        }
    }

    /// 观测网：hold 剩余 ms（status brief 消费；无 hold 在场为 None）。
    pub(crate) fn hold_remaining_ms(&self, now: std::time::Instant) -> Option<u64> {
        self.hold_until
            .and_then(|t| t.checked_duration_since(now))
            .map(|d| d.as_millis() as u64)
    }

    /// 观测网访问器（status.rs / ime_info 消费）。
    pub(crate) fn frames_captured(&self) -> u32 {
        self.frames_captured
    }
    pub(crate) fn deliveries(&self) -> u32 {
        self.deliveries
    }
    pub(crate) fn last_visible_delivery_ago_secs(&self) -> Option<u64> {
        // checked_duration_since 而非 elapsed：测试注入假时钟时 elapsed()
        // 对"未来时刻"饱和为 0，会掩盖记账方向错误。
        let now = self.now();
        self.last_visible_delivery.map(|t| now.checked_duration_since(t).map_or(0, |d| d.as_secs()))
    }
    pub(crate) fn epoch(&self) -> u32 {
        self.epoch
    }
    pub(crate) fn seq_for_diag(&self) -> u32 {
        self.seq
    }
    pub(crate) fn delivered(&self) -> u32 {
        self.delivered
    }
}

// ─────────────────────────────────────────────────────────────
// commit 认领：popup surface 的缓冲拷贝（CompositorHandler::commit 调）
// ─────────────────────────────────────────────────────────────

/// `CompositorHandler::commit` 钩子：surface 若持有 input_popup 角色，
/// 把当前缓冲拷成 ARGB8888 帧，并按 wl 语义 release（fcitx5 双缓冲靠
/// release 驱动重绘）。非 popup surface 极廉价返回。
pub(crate) fn on_popup_commit(wlc: &mut WLCState, surface: &WlSurface) {
    if !wlc.ime.popup.owns(surface) {
        return;
    }

    // 摘下当前缓冲（take：本轨道是 popup 缓冲的唯一消费者，
    // 摘走即免除任何二次 attach/release 竞态）。
    let slot = with_states(surface, |states| {
        let mut guard = states.cached_state.get::<SurfaceAttributes>();
        guard.deref_mut().current().buffer.take()
    });

    match slot {
        None => {}
        Some(BufferAssignment::Removed) => {
            wlc.ime.popup.note_frame_removed();
            // 真实隐藏（已交付帧、无 hold 接管）→ 组字证据终结，角标落。
            let now = wlc.ime.popup.now();
            if !wlc.ime.popup.has_content(now) {
                wlc.ime.note_popup_content_gone();
            }
        }
        Some(BufferAssignment::NewBuffer(buffer)) => {
            capture_buffer(wlc, &buffer);
            buffer.release();
        }
    }
}

fn capture_buffer(wlc: &mut WLCState, buffer: &WlBuffer) {
    // 指针仅在回调期间有效：当场拷成 owned Vec<u8>（逐像素 read_unaligned，
    // 不对易变 shm 建长生命周期引用——见 smithay with_buffer_contents 文档）。
    let outcome = with_buffer_contents(buffer, |ptr, len, meta| {
        let info = ShmBufInfo {
            width: meta.width,
            height: meta.height,
            stride: meta.stride,
            offset: meta.offset,
            format: meta.format as u32,
        };
        // SAFETY: with_buffer_contents 保证 ptr..ptr+len 为已映射可读池内存。
        (unsafe { convert_shm_to_argb(ptr, len, &info) }, info.width, info.height)
    });
    match outcome {
        Ok((Some(argb), width, height)) => {
            wlc.ime.popup.note_frame(width, height, argb);
            // 弹窗模式组字证据：classicui 只在面板有内容时绘制候选窗——
            // 采到新帧 = 预编辑/候选流在场（角标原料，见 mod.rs
            // popup_work_state 的生产形态注释）。
            wlc.ime.note_popup_frame_activity();
        }
        Ok((None, width, height)) => {
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][popup] 缓冲格式/几何不支持（{width}x{height}），跳帧"
            ));
        }
        Err(BufferAccessError::NotManaged) => {
            if !wlc.ime.popup.dmabuf_warned {
                wlc.ime.popup.dmabuf_warned = true;
                crate::bridge::ime_log_write(
                    "[waylandcraft][ime][popup] 非 shm 缓冲（dmabuf/single-pixel？）：不采集直接 release。\
                     fcitx5 classicui 恒用 shm ARGB8888（源码取证）；其他 IME 走 dmabuf 见本文件 TODO",
                );
            }
        }
        Err(_) => {
            crate::bridge::ime_log_write("[waylandcraft][ime][popup] shm 映射失败（SIGBUS 防护路径）");
        }
    }
}

// ─────────────────────────────────────────────────────────────
// 纯函数：shm 池内存 → ARGB8888 转换（无 wayland 对象依赖，单测直打）
// ─────────────────────────────────────────────────────────────

/// wl_shm 缓冲元数据（`BufferData` 的 plain 拷贝，便于单测构造）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShmBufInfo {
    pub width: i32,
    pub height: i32,
    /// 字节/行。
    pub stride: i32,
    /// 池内字节偏移。
    pub offset: i32,
    /// `wl_shm.format` 协议原始值。
    pub format: u32,
}

/// 候选窗很小：尺寸/面积双重封顶，防御恶意或错乱入参造成的巨大分配。
const MAX_DIM: i32 = 4096;
const MAX_AREA: usize = 4_000_000;

// 协议格式常量（`wl_shm::Format` 的线值，经 `as u32` 与 bridge 现有
// `(metadata.format as u32)` 同一来源，杜绝手抄 fourcc 出错）。
const F_ARGB8888: u32 = wl_shm::Format::Argb8888 as u32;
const F_XRGB8888: u32 = wl_shm::Format::Xrgb8888 as u32;
const F_ABGR8888: u32 = wl_shm::Format::Abgr8888 as u32;
const F_XBGR8888: u32 = wl_shm::Format::Xbgr8888 as u32;
const F_XRGB2101010: u32 = wl_shm::Format::Xrgb2101010 as u32;
const F_XBGR2101010: u32 = wl_shm::Format::Xbgr2101010 as u32;
const F_RGB565: u32 = wl_shm::Format::Rgb565 as u32;
const F_ARGB4444: u32 = wl_shm::Format::Argb4444 as u32;

/// 把已映射 shm 池 `pool`（长 `len` 字节）描述的一段缓冲转成 ARGB8888
/// 字节流（小端 u32/像素，内存序 [B,G,R,A]——wl_shm ARGB8888 规范布局）。
///
/// 返回 `None`：格式不支持 / 尺寸非法 / offset+stride 越界 / 面积超限。
/// 所有解引用前完成边界证明，越界不 panic 只拒绝。
///
/// # Safety
/// `pool..pool+len` 必须是已映射可读内存（with_buffer_contents 回调保证）。
pub(crate) unsafe fn convert_shm_to_argb(
    pool: *const u8,
    len: usize,
    info: &ShmBufInfo,
) -> Option<Vec<u8>> {
    let w = info.width;
    let h = info.height;
    if w <= 0 || h <= 0 || w > MAX_DIM || h > MAX_DIM {
        return None;
    }
    if info.stride <= 0 || info.offset < 0 {
        return None;
    }
    let bpp: usize = match info.format {
        F_ARGB8888 | F_XRGB8888 | F_ABGR8888 | F_XBGR8888 | F_XRGB2101010 | F_XBGR2101010 => 4,
        F_RGB565 | F_ARGB4444 => 2,
        _ => return None, // 其余格式（C8/RGB888/YUV/…）候选窗不可能用到，不支持
    };
    let (w, h) = (w as usize, h as usize);
    let area = w.checked_mul(h)?;
    if area > MAX_AREA {
        return None;
    }
    let stride = info.stride as usize;
    let row_bytes = w.checked_mul(bpp)?;
    if stride < row_bytes {
        return None;
    }
    let offset = info.offset as usize;
    let need = offset
        .checked_add(stride.checked_mul(h - 1)?)?
        .checked_add(row_bytes)?;
    if need > len {
        return None;
    }

    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            // SAFETY: offset+stride/row 边界已在上文证明落于 len 内。
            let src = unsafe { pool.add(offset + y * stride + x * bpp) };
            let argb: u32 = if bpp == 4 {
                let raw = u32::from_le_bytes(unsafe { (src as *const [u8; 4]).read_unaligned() });
                match info.format {
                    F_ARGB8888 => raw,
                    F_XRGB8888 => raw | 0xff00_0000,
                    F_ABGR8888 => swap_rb(raw) | 0xff00_0000,
                    F_XBGR8888 => swap_rb(raw) | 0xff00_0000,
                    F_XRGB2101010 => x2101010_to_argb(raw),
                    F_XBGR2101010 => swap_rb(x2101010_to_argb(raw)),
                    _ => return None,
                }
            } else {
                let raw = u16::from_le_bytes(unsafe { (src as *const [u8; 2]).read_unaligned() });
                match info.format {
                    F_RGB565 => rgb565_to_argb(raw),
                    F_ARGB4444 => argb4444_to_argb(raw),
                    _ => return None,
                }
            };
            let di = (y * w + x) * 4;
            out[di..di + 4].copy_from_slice(&argb.to_le_bytes());
        }
    }
    Some(out)
}

/// 4bit nibble 高复制到 8bit（0xA → 0xAA）。
const fn nibble_to_byte(n: u32) -> u32 {
    (n << 4) | n
}

/// R/B 通道交换（ABGR/XBGR 系 → ARGB 语义）。
const fn swap_rb(v: u32) -> u32 {
    (v & 0xff00_ff00) | ((v & 0xff) << 16) | ((v >> 16) & 0xff)
}

/// 10bit XRGB2101010（[r10 g10 b10]，r 在高位）→ 8bit 全不透明 ARGB。
const fn x2101010_to_argb(v: u32) -> u32 {
    let r = (v >> 20) & 0x3ff;
    let g = (v >> 10) & 0x3ff;
    let b = v & 0x3ff;
    0xff00_0000 | ((r >> 2) << 16) | ((g >> 2) << 8) | (b >> 2)
}

/// RGB565（RRRRRGGGGGBBBBB）→ 8bit ARGB（通道高位复制扩展）。
const fn rgb565_to_argb(v: u16) -> u32 {
    let r = ((v >> 11) & 0x1f) as u32;
    let g = ((v >> 5) & 0x3f) as u32;
    let b = (v & 0x1f) as u32;
    0xff00_0000 | ((r << 3) | (r >> 2)) << 16 | ((g << 2) | (g >> 4)) << 8 | (b << 3) | (b >> 2)
}

/// ARGB4444（0xARGB 各 4bit）→ 8bit（nibble 高复制到低位扩展）。
const fn argb4444_to_argb(v: u16) -> u32 {
    let a = nibble_to_byte(((v >> 12) & 0xf) as u32);
    let r = nibble_to_byte(((v >> 8) & 0xf) as u32);
    let g = nibble_to_byte(((v >> 4) & 0xf) as u32);
    let b = nibble_to_byte((v & 0xf) as u32);
    (a << 24) | (r << 16) | (g << 8) | b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(w: i32, h: i32, stride: i32, offset: i32, format: u32) -> ShmBufInfo {
        ShmBufInfo { width: w, height: h, stride, offset, format }
    }

    /// 测试友好包装：从 &[u8] 转换（unsafe 只在薄封装内一次）。
    fn cvt(pool: &[u8], i: &ShmBufInfo) -> Option<Vec<u8>> {
        unsafe { convert_shm_to_argb(pool.as_ptr(), pool.len(), i) }
    }

    #[test]
    fn argb8888_passthrough_le_bytes() {
        // 2x1：像素 [0xFF112233, 0x80AABBCC]，ARGB8888 应逐字节直通。
        let mut buf = 0xFF11_2233u32.to_le_bytes().to_vec();
        buf.extend_from_slice(&0x80AA_BBCCu32.to_le_bytes());
        let out = cvt(&buf, &info(2, 1, 8, 0, F_ARGB8888)).expect("应转换成功");
        assert_eq!(out, buf);
    }

    #[test]
    fn xrgb8888_forces_opaque_alpha() {
        let buf = 0x0011_2233u32.to_le_bytes().to_vec();
        let out = cvt(&buf, &info(1, 1, 4, 0, F_XRGB8888)).expect("应转换成功");
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 0xFF11_2233);
    }

    #[test]
    fn abgr_and_xbgr_swap_rb() {
        // 0xAABBGGRR → 0xAARRGGBB
        let buf = 0xFFEEDDCCu32.to_le_bytes().to_vec();
        let out = cvt(&buf, &info(1, 1, 4, 0, F_ABGR8888)).expect("应转换成功");
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 0xFFCCDDEE);
        let buf = 0x00EEDDCCu32.to_le_bytes().to_vec();
        let out = cvt(&buf, &info(1, 1, 4, 0, F_XBGR8888)).expect("应转换成功");
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 0xFFCCDDEE);
    }

    #[test]
    fn rgb2101010_downshifts() {
        // r=0x3FF(全), g=0, b=0 → 0xFFFF0000
        let buf = 0x3FF0_0000u32.to_le_bytes().to_vec();
        let out = cvt(&buf, &info(1, 1, 4, 0, F_XRGB2101010)).expect("应转换成功");
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 0xFFFF_0000);
        // Xbgr 形态：b 在高位 → 输出蓝通道
        let buf = 0x3FF0_0000u32.to_le_bytes().to_vec();
        let out = cvt(&buf, &info(1, 1, 4, 0, F_XBGR2101010)).expect("应转换成功");
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 0xFF00_00FF);
    }

    #[test]
    fn rgb565_expands_channels() {
        // 0xF800 = 纯红
        let buf = 0xF800u16.to_le_bytes().to_vec();
        let out = cvt(&buf, &info(1, 1, 2, 0, F_RGB565)).expect("应转换成功");
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 0xFFFF0000);
        // 0x07E0 = 纯绿
        let buf = 0x07E0u16.to_le_bytes().to_vec();
        let out = cvt(&buf, &info(1, 1, 2, 0, F_RGB565)).expect("应转换成功");
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 0xFF00FF00);
    }

    #[test]
    fn argb4444_extends_nibbles() {
        let buf = 0xF123u16.to_le_bytes().to_vec();
        let out = cvt(&buf, &info(1, 1, 2, 0, F_ARGB4444)).expect("应转换成功");
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 0xFF112233);
    }

    #[test]
    fn respects_stride_and_offset() {
        // 2x2，stride 12（8 有效 + 4 填充），offset 4（前面垫一行垃圾）。
        let mut buf = vec![0u8; 4];
        buf.extend_from_slice(&0xFF00_0001u32.to_le_bytes());
        buf.extend_from_slice(&0xFF00_0002u32.to_le_bytes());
        buf.extend_from_slice(&[0; 4]); // 行填充
        buf.extend_from_slice(&0xFF00_0003u32.to_le_bytes());
        buf.extend_from_slice(&0xFF00_0004u32.to_le_bytes());
        buf.extend_from_slice(&[0; 4]);
        let out = cvt(&buf, &info(2, 2, 12, 4, F_ARGB8888)).expect("应转换成功");
        assert_eq!(
            out,
            vec![1, 0, 0, 0xff, 2, 0, 0, 0xff, 3, 0, 0, 0xff, 4, 0, 0, 0xff,]
        );
    }

    #[test]
    fn rejects_bad_geometry_or_oob() {
        let buf = vec![0u8; 16];
        // 零/负尺寸、负 stride/offset
        assert!(cvt(&buf, &info(0, 1, 4, 0, F_ARGB8888)).is_none());
        assert!(cvt(&buf, &info(1, -1, 4, 0, F_ARGB8888)).is_none());
        assert!(cvt(&buf, &info(1, 1, -4, 0, F_ARGB8888)).is_none());
        assert!(cvt(&buf, &info(1, 1, 4, -1, F_ARGB8888)).is_none());
        // 越界：1x1 stride 4 offset 20 超出 16 字节池
        assert!(cvt(&buf, &info(1, 1, 4, 20, F_ARGB8888)).is_none());
        // 越界：3x3 stride 12 需要 36 字节 > 16
        assert!(cvt(&buf, &info(3, 3, 12, 0, F_ARGB8888)).is_none());
        // 不支持格式（C8）
        assert!(cvt(&buf, &info(1, 1, 1, 0, wl_shm::Format::C8 as u32)).is_none());
        // stride 小于行字节数（2 像素需要 8，stride 4）
        assert!(cvt(&buf, &info(2, 1, 4, 0, F_ARGB8888)).is_none());
    }

    #[test]
    fn popup_state_epoch_flow() {
        // 纯状态机（不涉及 WlSurface 实例）：note_frame/removed/touch 的
        // epoch、seq、快照语义。
        let mut st = InputPopupState::default();
        assert!(!st.changed());
        st.note_frame(10, 20, vec![0u8; 800]);
        assert_eq!(st.seq, 1);
        assert!(st.changed());
        let snap = st.snapshot(true);
        assert_eq!((snap.width, snap.height), (10, 20));
        assert_eq!(snap.argb.as_ref().unwrap().len(), 800);
        st.mark_delivered();
        assert!(!st.changed());
        // touch（可见性原料变化）→ 再次可轮询
        st.touch();
        assert!(st.changed());
        // attach(NULL) → 帧清、epoch 推进、seq 不变（无新内容）
        st.mark_delivered();
        st.note_frame_removed();
        assert!(st.changed());
        assert_eq!(st.seq, 1);
        let snap2 = st.snapshot(false);
        assert!(snap2.argb.is_none());
        assert!(!snap2.visible);
        assert_eq!(snap2.width, 0);
        st.mark_delivered();
        assert!(!st.changed());
    }

    #[test]
    fn delivery_stats_track_frames_and_visible_delivery() {
        // 观测网记账：采集计数随 note_frame 增长；record_delivery 区分
        // 可见/不可见交付（last_visible 只在 visible=true 时刷新）。
        let mut st = InputPopupState::default();
        assert_eq!(st.frames_captured(), 0);
        assert_eq!(st.deliveries(), 0);
        assert!(st.last_visible_delivery_ago_secs().is_none());
        st.note_frame(2, 2, vec![0u8; 16]);
        st.note_frame(2, 2, vec![1u8; 16]);
        assert_eq!(st.frames_captured(), 2);
        st.record_delivery(false);
        assert_eq!(st.deliveries(), 1);
        assert!(st.last_visible_delivery_ago_secs().is_none(), "不可见交付不刷新 last_visible");
        st.record_delivery(true);
        assert!(st.last_visible_delivery_ago_secs().is_some());
        assert_eq!(st.deliveries(), 2);
    }

    /// display hold（sync9 竞态修复）：采到但从未交付的帧在 attach(NULL)
    /// 后继续交付原帧（visible=true、原 argb、**原 seq**——同 seq 免重传
    /// 合同），直到新帧顶替或窗口到期。假时钟推进，无 sleep。
    #[test]
    fn hold_keeps_undelivered_frame_after_attach_null() {
        let mut st = InputPopupState::default();
        let t0 = std::time::Instant::now();
        st.set_clock(t0);
        st.note_frame(4, 3, vec![7u8; 48]);
        st.note_frame_removed();
        // live 帧已清（has_frame 语义不变），但内容仍持有。
        assert!(!st.has_frame());
        assert!(st.held_active(t0));
        assert_eq!(st.hold_remaining_ms(t0), Some(POPUP_HOLD_MS));
        // 下一次轮询必须交付原帧原 seq（seq=1，从未交付过 → hold 接管）。
        let snap = st.snapshot(true);
        assert!(snap.visible, "hold 窗口内 attach(NULL) 不得吞帧");
        assert_eq!(snap.seq, 1);
        assert_eq!(snap.argb.as_deref(), Some(&vec![7u8; 48][..]));
        // 到期：expire_hold 清暂存、推进 epoch（下一次轮询交付不可见）。
        assert!(!st.expire_hold(t0 + std::time::Duration::from_millis(POPUP_HOLD_MS - 1)));
        assert!(st.held_active(t0 + std::time::Duration::from_millis(POPUP_HOLD_MS - 1)));
        assert!(st.expire_hold(t0 + std::time::Duration::from_millis(POPUP_HOLD_MS + 1)));
        assert!(!st.held_active(t0));
        let snap2 = st.snapshot(true);
        assert!(!snap2.visible, "到期无新帧必须回落不可见");
        assert!(snap2.argb.is_none());
    }

    /// hold 期内新帧到达 → 顶替暂存、seq 续排、到期不再回落隐藏
    /// （新帧自身成为交付源）。
    #[test]
    fn hold_renewed_by_new_frame_seq_monotonic() {
        let mut st = InputPopupState::default();
        let t0 = std::time::Instant::now();
        st.set_clock(t0);
        st.note_frame(2, 2, vec![1u8; 16]); // seq=1
        st.note_frame_removed(); // hold(seq=1)
        st.note_frame(2, 2, vec![2u8; 16]); // 顶替：seq=2，held 清空
        assert!(!st.held_active(t0), "新帧顶替暂存");
        let snap = st.snapshot(true);
        assert_eq!(snap.seq, 2);
        assert_eq!(snap.argb.as_deref(), Some(&vec![2u8; 16][..]));
        // 到期结算只针对 held：live 帧不受 expire_hold 影响。
        st.note_frame_removed(); // seq=2 已交付？——未 mark_delivered → 仍接管
        assert!(st.held_active(t0));
        assert_eq!(st.snapshot(true).seq, 2);
        assert!(st.expire_hold(t0 + std::time::Duration::from_millis(POPUP_HOLD_MS + 1)));
        assert!(!st.snapshot(true).visible);
    }

    /// 角色解除（cleared/gone_for）同走 hold：未交付帧保留交付；
    /// 到期后真清。既有字段语义（has_frame=false、seq 归零代际）不变。
    #[test]
    fn role_cleared_keeps_undelivered_frame_held() {
        let mut st = InputPopupState::default();
        let t0 = std::time::Instant::now();
        st.set_clock(t0);
        st.note_frame(3, 1, vec![9u8; 12]);
        st.cleared();
        assert!(!st.has_frame());
        assert_eq!(st.seq, 0, "代际计数语义不变");
        assert!(st.held_active(t0), "角色解除不得吞掉未交付帧");
        let snap = st.snapshot(true);
        assert!(snap.visible);
        assert_eq!(snap.seq, 1, "hold 交付采到时的 seq");
        assert_eq!(snap.argb.as_deref(), Some(&vec![9u8; 12][..]));
        // 新代际第一帧的 seq 必须跨过 held 旧值（免撞 Java 免重传合同）。
        st.note_frame(3, 1, vec![8u8; 12]);
        assert_eq!(st.seq, 2, "新帧 seq 跨代际单调");
    }

    /// 已交付过的帧被 attach(NULL)：真实隐藏，立即清（既有协议合同——
    /// popup_null_attach/popup_java_polling 用例锁死此形态）。
    #[test]
    fn delivered_frame_removed_hides_immediately() {
        let mut st = InputPopupState::default();
        let t0 = std::time::Instant::now();
        st.set_clock(t0);
        st.note_frame(2, 2, vec![3u8; 16]);
        st.snapshot(true);
        st.mark_delivered(); // 本帧已随快照交付
        st.note_frame_removed();
        assert!(!st.held_active(t0), "已交付帧不进入 hold");
        let snap = st.snapshot(true);
        assert!(!snap.visible);
        assert_eq!(snap.width, 0);
    }

    #[test]
    fn gone_for_only_clears_matching_role() {
        // 无 surface 时 gone_for 不炸且不误清（role 不匹配路径）。
        let mut st = InputPopupState::default();
        // 用假 ObjectId 不便构造——改测 cleared 幂等 + role 不匹配语义的
        // 可观察面：cleared 空态直接 return，epoch 不变。
        st.cleared();
        assert!(!st.changed());
        st.note_frame(1, 1, vec![0, 0, 0, 0]);
        st.cleared();
        assert!(!st.has_frame());
        assert_eq!(st.seq, 0);
        assert!(st.changed());
    }
}
