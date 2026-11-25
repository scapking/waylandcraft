//! mod 整体状态诊断 — 一个 JSON snapshot 报告所有子系统。
//!
//! Java 端每 N 帧调用 `bridge.getStatusReport()`，把 JSON 写入覆盖式
//! `status.log`。这样排查问题时打开一个文件就能看到所有子系统状态，
//! 不必切换 4 个独立日志（launch / ime / kb / audio）。
//!
//! ## 输出格式
//!
//! JSON（每行可解析）— 但因为 Java 写入是覆盖式，实际文件里**只有最新一份**。
//! 人类可读的关键行（version / state 摘要）放在前几行注释里。

use std::fmt::Write;

use crate::WaylandCraft;

// ═══ ⑥ recent_errors：全局错误环形缓冲（v0.13.4 只有结构没有生产者，
// status.log 的 errors 永远为空——本块补上生产者/消费者全链路）═══
//
// 设计：任何线程可 note_error()；连续相同消息去重（fcitx5 类重试路径
// 会按节流重复报同一错误，刷屏没有意义）；容量 64，满丢最旧。
// getStatusReport 每次 gather 时快照进 errors 字段，status.log 和
// /wl status（Java 侧解析）共用同一来源。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::LazyLock;

static RECENT_ERRORS: LazyLock<Mutex<VecDeque<ErrorEntry>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));
const ERROR_RING_CAPACITY: usize = 64;

/// 记录一条子系统错误（非 panic 路径的失败事件）。线程安全、去重、永不阻塞。
pub fn note_error(level: &str, msg: &str) {
    let Ok(mut ring) = RECENT_ERRORS.try_lock() else { return }; // 锁忙直接放弃——诊断绝不反噬主流程
    if let Some(last) = ring.back() {
        if last.msg == msg {
            return; // 连续相同消息去重
        }
    }
    let entry = ErrorEntry {
        ts: crate::bridge::log_timestamp(),
        level: level.to_string(),
        msg: msg.to_string(),
    };
    ring.push_back(entry);
    while ring.len() > ERROR_RING_CAPACITY {
        ring.pop_front();
    }
}

/// 快照（供 gather）。
pub fn snapshot_errors() -> Vec<ErrorEntry> {
    RECENT_ERRORS.lock().map(|ring| ring.iter().cloned().collect()).unwrap_or_default()
}

/// ⑥ mod_version 由 Java 在初始化时经 setModVersionNative 注入真实值
/// （旧实现 Rust 硬编码 "1.2.13"，bump 三次都忘同步——status.log 版本谎报根因）。
pub fn set_mod_version(v: &str) {
    if let Ok(mut g) = MOD_VERSION.lock() {
        *g = v.to_string();
    }
}
fn mod_version_field_empty() -> bool {
    MOD_VERSION.lock().map(|g| g.is_empty()).unwrap_or(false)
}
pub fn mod_version() -> String {
    MOD_VERSION.lock().map(|g| g.clone()).unwrap_or_else(|_| "unknown".into())
}
static MOD_VERSION: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// 状态档位（与 issue 严重度对齐）
#[derive(Debug, Clone, Copy)]
pub enum State {
    /// 完全工作
    Ok,
    /// 部分功能损失但不致命（如 dmabuf 不可用走 shm fallback）
    Degraded,
    /// 关键功能失败（如 native lib 未加载、host_bridge probe 失败）
    Error,
    /// 用户显式禁用（如 IME 用户关了）
    Disabled,
    /// 暂未使用 / 不适用
    NotApplicable,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Degraded => "degraded",
            State::Error => "error",
            State::Disabled => "disabled",
            State::NotApplicable => "n/a",
        }
    }
}

/// 单个子系统报告
#[derive(Debug, Clone)]
pub struct SubEntry {
    pub state: State,
    pub details: String,
}

impl SubEntry {
    pub fn ok(details: impl Into<String>) -> Self {
        Self { state: State::Ok, details: details.into() }
    }
    pub fn degraded(details: impl Into<String>) -> Self {
        Self { state: State::Degraded, details: details.into() }
    }
    pub fn error(details: impl Into<String>) -> Self {
        Self { state: State::Error, details: details.into() }
    }
    pub fn disabled(details: impl Into<String>) -> Self {
        Self { state: State::Disabled, details: details.into() }
    }
    pub fn na(details: impl Into<String>) -> Self {
        Self { state: State::NotApplicable, details: details.into() }
    }
}

/// 完整状态报告
pub struct StatusReport {
    pub mod_version: String,
    pub native_version: &'static str,
    pub uptime_s: u64,
    pub java_thread: String,
    pub subsystems: Vec<(&'static str, SubEntry)>,
    pub metrics: Vec<(&'static str, String)>,
    pub errors: Vec<ErrorEntry>,
}

#[derive(Debug, Clone)]
pub struct ErrorEntry {
    pub ts: String,
    pub level: String,
    pub msg: String,
}

impl StatusReport {
    /// 收集所有子系统状态。从 `WaylandCraft` 拿只读引用，**不**持有可变借用。
    /// 内部用 `StatusExt` trait 取数据——避免 lib.rs 暴露大量 getter。
    pub fn gather(wc: &WaylandCraft, java_thread: String) -> Self {
        // ⑥ 真实 mod 版本（Java setModVersionNative 注入；未注入显示 not-set）
        let mod_version = if mod_version_field_empty() { "not-set".to_string() } else { mod_version() };
        use StatusExt;
        let mut subsystems = Vec::new();

        // ── native lib / EGL ──────────────────────────────────────
        subsystems.push((
            "native_lib",
            SubEntry::ok("loaded libwaylandcraft-linux-gnu-x86_64.so"),
        ));
        subsystems.push((
            "egl_display",
            SubEntry::ok(format!("0x{:x}", wc.egl_display_raw())),
        ));

        // ── wayland display：Java 侧 glfwGetWaylandDisplay() ──
        // wc 没有这个字段，状态由 Java 传入（在 wayland_display 字段）。
        // 这里先标 "n/a" 留给 Java 填。
        subsystems.push((
            "wayland_display",
            SubEntry::na("see Java side log"),
        ));

        // ── host_bridge ──────────────────────────────────────────
        // ⑤ 修谎报：update() 帧末把 host_bridge 从 state 挪回 self（mem::take 模式），
        // getStatusReport 恰在帧间调用 → 旧实现读 state.host_bridge 恒 None。
        // 正确来源 = self 优先、state 兜底。
        match wc.host_bridge.as_ref().or(wc.state.host_bridge.as_ref()) {
            Some(hb) => {
                if hb.is_ready() {
                    subsystems.push((
                        "host_bridge",
                        SubEntry::ok(format!("{} → active", hb.name())),
                    ));
                } else if hb.is_dead() {
                    subsystems.push((
                        "host_bridge",
                        SubEntry::error("worker channel closed; ibus disconnected"),
                    ));
                } else {
                    subsystems.push((
                        "host_bridge",
                        SubEntry::degraded("not ready (transient init)"),
                    ));
                }
            }
            None => {
                subsystems.push((
                    "host_bridge",
                    SubEntry::error("host_bridge = None; ibus/fcitx5 not bridged"),
                ));
            }
        }

        // ── nested_ime（v0.14 嵌套 IME daemon 路线）──────────────────
        // 状态如实上报：Active=主路径在线；Failed/NoBinary=回退
        // host_bridge/raw 且已进 error ring。详见 ime.log 的 ⑥ 决策行。
        {
            use crate::ime::NestedPhase;
            let ime = &wc.state.ime;
            let line = ime.nested_status_line();
            let entry = match ime.nested.phase() {
                NestedPhase::Active => SubEntry::ok(format!("{line}; route=im2")),
                NestedPhase::Starting => SubEntry::degraded(format!("{line}; 等 im2 bind")),
                NestedPhase::Failed(_) => {
                    SubEntry::error(format!("{line}; 回退 host_bridge/raw"))
                }
                NestedPhase::NoBinary => {
                    SubEntry::degraded(format!("{line}; 无 fcitx5/ibus-daemon，走 host_bridge/raw"))
                }
                NestedPhase::Disabled => SubEntry::disabled(line),
                NestedPhase::Unconfigured => SubEntry::na(format!("{line}; 等 Java settings 同步")),
            };
            subsystems.push(("nested_ime", entry));
        }

        // ── ime_candidate_popup（游戏内候选窗 overlay 的 native 侧观测面）──
        // sync8 复盘：候选窗静默不显示，日志无法切分"native 采到帧但
        // Java 从未收到"。这行把采集/交付/可见性门原料变成可对比数字；
        // Java 侧 /wl status 逐子系统透传，无需改 Java 解析。
        {
            let ime = &wc.state.ime;
            let brief = ime.popup_status_brief(wc.state.seat.kb_active);
            let entry = if ime.popup.frames_captured() == 0 {
                SubEntry::na(format!("{brief}; 尚无采集帧（未组合或 popup 未在场）"))
            } else if ime.popup.deliveries() == 0 {
                SubEntry::error(format!("{brief}; 已采集但 Java 从未轮询到变化（tick/JNI 断链）"))
            } else {
                SubEntry::ok(brief)
            };
            subsystems.push(("ime_candidate_popup", entry));
        }

        // ── xwayland-satellite ──────────────────────────────────
        if let Some(sat) = &wc.state.satellite {
            subsystems.push((
                "xwayland_satellite",
                SubEntry::ok(format!("DISPLAY={}", sat.get_display())),
            ));
        } else {
            subsystems.push((
                "xwayland_satellite",
                SubEntry::degraded("not started; X11 apps can't connect via :2"),
            ));
        }

        // ── seat ─────────────────────────────────────────────────
        subsystems.push(("seat", SubEntry::ok("WlSeat global + keyboard + pointer")));

        // ── IME 状态（粗粒度）──────────────────────────────────
        if wc.ime_has_focus() {
            let ti3_count = wc.ti3_instance_count();
            if ti3_count > 0 {
                subsystems.push((
                    "ime_ti3",
                    SubEntry::degraded(format!(
                        "{} ti3 instance(s) enabled; ibus may not respond (issue #1)",
                        ti3_count
                    )),
                ));
            } else {
                subsystems.push((
                    "ime_ti3",
                    SubEntry::ok("focused but no ti3 client bound"),
                ));
            }
        } else {
            subsystems.push(("ime_ti3", SubEntry::disabled("no focus")));
        }
        if wc.has_xim_clients() {
            subsystems.push((
                "ime_xim",
                SubEntry::ok(format!("{} XIM clients", wc.xim_client_count())),
            ));
        } else {
            subsystems.push(("ime_xim", SubEntry::disabled("no XIM clients bound")));
        }

        // ── audio / portal ──────────────────────────────────────
        // per-app 捕获上线后不再只指路日志：直接报捕获模式
        // （per-app / monitor-fallback / idle）+ 目标节点。
        // audio_capture_brief 用 try_lock，状态采集绝不阻塞帧循环。
        match crate::audio_capture::audio_capture_brief() {
            crate::audio_capture::CaptureBrief::PerApp(d) => {
                subsystems.push(("audio_capture", SubEntry::ok(format!("mode=per-app {}", d))));
            }
            crate::audio_capture::CaptureBrief::MonitorFallback(d) => {
                subsystems.push((
                    "audio_capture",
                    SubEntry::degraded(format!("mode=monitor-fallback {}", d)),
                ));
            }
            crate::audio_capture::CaptureBrief::Idle(d) => {
                subsystems.push(("audio_capture", SubEntry::na(format!("{}; see waylandcraft-audio.log", d))));
            }
        }
        subsystems.push((
            "portal_capture",
            SubEntry::na("see waylandcraft-launch.log"),
        ));

        // ── EGL context ─────────────────────────────────────────
        subsystems.push((
            "egl_context",
            SubEntry::ok("Mesa llvmpipe / Intel / AMD (per system)"),
        ));

        // ── wayland globals（每个 protocol 一个状态）────────────
        subsystems.push(("shm", SubEntry::ok("ShmState registered")));
        subsystems.push(("xdg_shell", SubEntry::ok("XdgShellState registered")));
        subsystems.push(("viewporter", SubEntry::ok("global v1")));
        subsystems.push(("single_pixel", SubEntry::ok("global v3")));
        if wc.state.dmabuf_global.is_some() {
            subsystems.push(("dmabuf", SubEntry::ok("DmabufGlobal available")));
        } else {
            subsystems.push((
                "dmabuf",
                SubEntry::degraded("no render node; shm fallback used"),
            ));
        }
        subsystems.push(("cursor_shape", SubEntry::ok("WpCursorShapeManagerV1 v2")));
        subsystems.push(("pointer_constraints", SubEntry::ok("ZwpPointerConstraintsV1 v1")));
        subsystems.push(("relative_pointer", SubEntry::ok("ZwpRelativePointerManagerV1 v1")));

        // ── metrics ─────────────────────────────────────────────
        let mut metrics = Vec::new();
        metrics.push(("wlc_socket", wc.state.socket.to_string_lossy().to_string()));
        // v0.14 当前按键路由（im2=嵌套 daemon 主路径；host=host_bridge；
        // raw=纯直通）——status.log 一行定位"中文到底走哪条路"。
        metrics.push((
            "ime_route",
            if wc.state.ime.nested_route_live() {
                if wc.state.ime.keyboard_grabbed() { "im2-grab" } else { "im2" }
            } else if wc
                .host_bridge
                .as_ref()
                .or(wc.state.host_bridge.as_ref())
                .map(|h| h.is_ready())
                .unwrap_or(false)
            {
                "host_bridge"
            } else {
                "raw"
            }
            .to_string(),
        ));
        let uptime = wc.start_time.elapsed().as_secs();
        metrics.push(("uptime_s", uptime.to_string()));
        let (toplevels, popups) = wc.count_surfaces();
        metrics.push(("toplevels", toplevels.to_string()));
        metrics.push(("popups", popups.to_string()));

        // ── 错误缓冲（最近 N 条）────────────────────────────────
        let errors = wc.recent_errors();

        Self {
            mod_version,
            native_version: env!("CARGO_PKG_VERSION"),
            uptime_s: uptime,
            java_thread,
            subsystems,
            metrics,
            errors,
        }
    }

    /// 序列化为单行 JSON + 人类可读头部注释
    /// （Java 写入是覆盖式，文件**只有最新一份**——不需要多行历史）
    pub fn to_json(&self) -> String {
        let mut out = String::new();

        // 头部注释：快速可读
        let _ = writeln!(
            out,
            "# WaylandCraft Status v{} (native v{}) — overwritten each refresh",
            self.mod_version, self.native_version
        );
        let _ = writeln!(out, "# Subsystems: {}", self.subsystem_summary());
        let _ = writeln!(out, "# Java thread: {}", self.java_thread);
        let _ = writeln!(out, "# Errors: {}", self.errors.len());
        let _ = writeln!(out);

        // JSON body
        out.push_str("{\n");
        let _ = writeln!(out, "  \"version\": \"{}\",", self.mod_version);
        let _ = writeln!(out, "  \"native_version\": \"{}\",", self.native_version);
        let _ = writeln!(out, "  \"uptime_s\": {},", self.uptime_s);
        let _ = writeln!(out, "  \"java_thread\": \"{}\",", self.java_thread);
        out.push_str("  \"subsystems\": {\n");
        for (i, (name, e)) in self.subsystems.iter().enumerate() {
            let comma = if i + 1 < self.subsystems.len() { "," } else { "" };
            let _ = writeln!(
                out,
                "    \"{}\": {{ \"state\": \"{}\", \"details\": \"{}\" }}{}",
                name,
                e.state.as_str(),
                escape_json(&e.details),
                comma
            );
        }
        out.push_str("  },\n");
        out.push_str("  \"metrics\": {\n");
        for (i, (k, v)) in self.metrics.iter().enumerate() {
            let comma = if i + 1 < self.metrics.len() { "," } else { "" };
            let _ = writeln!(out, "    \"{}\": \"{}\"{}", k, escape_json(v), comma);
        }
        out.push_str("  },\n");
        out.push_str("  \"errors\": [\n");
        for (i, e) in self.errors.iter().enumerate() {
            let comma = if i + 1 < self.errors.len() { "," } else { "" };
            let _ = writeln!(
                out,
                "    {{ \"ts\": \"{}\", \"level\": \"{}\", \"msg\": \"{}\" }}{}",
                escape_json(&e.ts),
                escape_json(&e.level),
                escape_json(&e.msg),
                comma
            );
        }
        out.push_str("  ]\n");
        out.push_str("}\n");
        out
    }

    /// 单行状态摘要（写到头部注释里）
    fn subsystem_summary(&self) -> String {
        let mut counts = [0usize; 5];
        for (_, e) in &self.subsystems {
            counts[match e.state {
                State::Ok => 0,
                State::Degraded => 1,
                State::Error => 2,
                State::Disabled => 3,
                State::NotApplicable => 4,
            }] += 1;
        }
        format!(
            "{} ok, {} degraded, {} error, {} disabled, {} n/a",
            counts[0], counts[1], counts[2], counts[3], counts[4]
        )
    }
}

/// 简单的 JSON 字符串转义（不依赖 serde_json——保持 lib 体积小）
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── WaylandCraft 字段扩展需要：egl_display_raw, has_focus,
//     ti3_instance_count, has_xim_clients, xim_client_count, count_surfaces,
//     start_time, recent_errors
// ── 这些 helper 加上（避免 lib.rs 太大）──────────────────────

/// helper：让 WaylandCraft 提供额外信息
pub trait StatusExt {
    fn egl_display_raw(&self) -> u64;
    fn ime_has_focus(&self) -> bool;
    fn ti3_instance_count(&self) -> usize;
    fn has_xim_clients(&self) -> bool;
    fn xim_client_count(&self) -> usize;
    fn count_surfaces(&self) -> (usize, usize);
    fn recent_errors(&self) -> Vec<ErrorEntry>;
}

impl<'a> StatusExt for WaylandCraft<'a> {
    fn egl_display_raw(&self) -> u64 {
        // wlc 启动时 egl.display 是 *mut c_void；保留为 0 兜底
        0
    }
    fn ime_has_focus(&self) -> bool {
        self.state.ime.has_focus
    }
    fn ti3_instance_count(&self) -> usize {
        self.state.ime.ti3.instance_count()
    }
    fn has_xim_clients(&self) -> bool {
        // v0.13.4 XIM 实现未完成，XIM 协议没注册
        false
    }
    fn xim_client_count(&self) -> usize {
        0
    }
    fn count_surfaces(&self) -> (usize, usize) {
        (self.bridge.toplevels.len(), self.bridge.popups.len())
    }
    fn recent_errors(&self) -> Vec<ErrorEntry> {
        // ⑥ 全局环形缓冲快照（生产者见 note_error 调用点：
        // host_bridge 死亡/重探失败、satellite 重启、Err→Ok 降级、音频启动失败）
        snapshot_errors()
    }
}
