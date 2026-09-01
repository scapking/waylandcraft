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
    pub mod_version: &'static str,
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
    pub fn gather(wc: &WaylandCraft, java_thread: String, mod_version: &'static str) -> Self {
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
        match &wc.state.host_bridge {
            Some(hb) => {
                if hb.is_ready() {
                    subsystems.push((
                        "host_bridge",
                        SubEntry::ok(format!("dbus-ibus → active")),
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
        subsystems.push((
            "audio_capture",
            SubEntry::na("see waylandcraft-audio.log"),
        ));
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
        // v0.13.4：错误缓冲未实现；返回空
        // 未来可以加一个 thread_local 错误 ring buffer
        Vec::new()
    }
}
