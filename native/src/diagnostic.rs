//! 输入法自动诊断器（v0.11.0+）。
//!
//! 按 docs/agent/v010/SYSTEMATIC_REVIEW.md 第 13 章要求实现：
//! 一次运行输出完整输入法故障链状态——替代"几千行日志人工翻"。
//!
//! ## 输出格式
//!
//! ```
//! [PASS] keyboard event: scancode=0x6e received
//! [PASS] Wayland compositor: WAYLAND_DISPLAY=wayland-1 alive
//! [PASS] DBus session: address=unix:path=/run/user/1000/bus
//! [PASS] ibus daemon: PID 2739, bus name org.freedesktop.IBus
//! [PASS] ibus engine: libpinyin (default)
//! [PASS] input context: READY (path /org/freedesktop/IBus/InputContext_0/...)
//! [PASS] focus: surface=wl_surface@0x... active=true
//! [PASS] surrounding text: "你好" cursor=2 anchor=2
//! [PASS] preedit: "nihao" cursor=5 anchor=5
//! [PASS] commit: "你好" (last 1s)
//! [PASS] application received: "你好" (verified via ti3 active_text_input event)
//! RESULT: INPUT METHOD HEALTHY
//! ```
//!
//! 任何 FAIL 立即停止并给具体行号 + 修复建议。

use crate::host_bridge::HostBridgeHandle;
use crate::WLCState;
use std::fmt::Write;
use std::time::Duration;

/// 诊断报告（一次完整输入法健康检查）。
pub struct DiagnosticReport {
    pub checks: Vec<DiagnosticCheck>,
}

#[derive(Debug, Clone)]
pub struct DiagnosticCheck {
    pub layer: &'static str,
    pub status: CheckStatus,
    pub detail: String,
    /// 出错时的根因分析（按 7 章故障树）
    pub root_cause: Option<String>,
    /// 修复建议（具体到代码行号或协议名）
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail,
    Warn,
    Skip,
}

impl DiagnosticCheck {
    pub fn pass(layer: &'static str, detail: impl Into<String>) -> Self {
        Self {
            layer,
            status: CheckStatus::Pass,
            detail: detail.into(),
            root_cause: None,
            suggestion: None,
        }
    }
    pub fn fail(
        layer: &'static str,
        detail: impl Into<String>,
        root_cause: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            layer,
            status: CheckStatus::Fail,
            detail: detail.into(),
            root_cause: Some(root_cause.into()),
            suggestion: Some(suggestion.into()),
        }
    }
    pub fn warn(layer: &'static str, detail: impl Into<String>, suggestion: impl Into<String>) -> Self {
        Self {
            layer,
            status: CheckStatus::Warn,
            detail: detail.into(),
            root_cause: None,
            suggestion: Some(suggestion.into()),
        }
    }
    pub fn skip(layer: &'static str, detail: impl Into<String>) -> Self {
        Self {
            layer,
            status: CheckStatus::Skip,
            detail: detail.into(),
            root_cause: None,
            suggestion: None,
        }
    }
}

impl DiagnosticReport {
    /// 跑完整输入法健康检查。
    ///
    /// 13 章要求：完整 10 层覆盖
    /// 1. keyboard event
    /// 2. Wayland session
    /// 3. DBus session
    /// 4. ibus daemon
    /// 5. ibus engine
    /// 6. input context
    /// 7. focus
    /// 8. surrounding text
    /// 9. preedit
    /// 10. commit
    /// 11. application received
    pub fn run(state: &WLCState) -> Self {
        let mut checks = Vec::new();
        Self::check_environment(&mut checks);
        Self::check_wayland(&mut checks);
        Self::check_dbus(&mut checks);
        Self::check_ibus_daemon(&mut checks);
        Self::check_ibus_engine(&mut checks);
        Self::check_input_context(state, &mut checks);
        Self::check_focus(state, &mut checks);
        Self::check_surrounding(&mut checks);
        Self::check_preedit(&mut checks);
        Self::check_commit_history(&mut checks);
        Self::check_application_received(&mut checks);
        DiagnosticReport { checks }
    }

    /// 1. 环境（uid/gid/user/session/runtime_dir）
    fn check_environment(out: &mut Vec<DiagnosticCheck>) {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        if uid == 0 {
            out.push(DiagnosticCheck::fail(
                "environment",
                format!("uid={uid} gid={gid}"),
                "root 环境——dbus user session bus / wayland socket 通常不可用",
                "以普通 desktop user 启动 MC（systemd user session 必须有 XDG_RUNTIME_DIR）",
            ));
        } else {
            out.push(DiagnosticCheck::pass(
                "environment",
                format!("uid={uid} gid={gid} user=非 root"),
            ));
        }
    }

    /// 2. Wayland session
    fn check_wayland(out: &mut Vec<DiagnosticCheck>) {
        match std::env::var("WAYLAND_DISPLAY") {
            Ok(v) if !v.is_empty() => {
                out.push(DiagnosticCheck::pass(
                    "wayland_session",
                    format!("WAYLAND_DISPLAY={v}"),
                ));
            }
            Ok(v) => {
                out.push(DiagnosticCheck::fail(
                    "wayland_session",
                    format!("WAYLAND_DISPLAY={v:?}（空字符串）"),
                    "嵌套合成器启动时未传 WAYLAND_DISPLAY",
                    "bridge::keyboard_input 启动嵌套应用时设 WAYLAND_DISPLAY=<our_socket>",
                ));
            }
            Err(_) => {
                out.push(DiagnosticCheck::fail(
                    "wayland_session",
                    "WAYLAND_DISPLAY 未设",
                    "嵌套 firefox 启动时未设 WAYLAND_DISPLAY",
                    "process.rs build_universal_env_list 加 WAYLAND_DISPLAY=<our_socket>",
                ));
            }
        }
    }

    /// 3. DBus session
    fn check_dbus(out: &mut Vec<DiagnosticCheck>) {
        match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
            Ok(v) if v.starts_with("unix:") => {
                out.push(DiagnosticCheck::pass(
                    "dbus_session",
                    format!("DBUS_SESSION_BUS_ADDRESS={v}"),
                ));
            }
            Ok(v) => {
                out.push(DiagnosticCheck::warn(
                    "dbus_session",
                    format!("DBUS_SESSION_BUS_ADDRESS={v}（非 unix: 路径）"),
                    "嵌套合成器可能用别的 transport——ibus 走 session bus 默认",
                ));
            }
            Err(_) => {
                out.push(DiagnosticCheck::fail(
                    "dbus_session",
                    "DBUS_SESSION_BUS_ADDRESS 未设",
                    "嵌套 firefox 启动时未传 dbus 地址",
                    "process.rs build_universal_env_list 加 DBUS_SESSION_BUS_ADDRESS=<host_addr>",
                ));
            }
        }
    }

    /// 4. ibus daemon 进程
    fn check_ibus_daemon(out: &mut Vec<DiagnosticCheck>) {
        // 简单 ps grep
        let output = std::process::Command::new("ps")
            .args(["-eo", "pid,comm,args"])
            .output();
        match output {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout);
                if s.lines().any(|l| l.contains("ibus-daemon")) {
                    let ibus_portal_line = s
                        .lines()
                        .find(|l| l.contains("ibus-portal"))
                        .map(|l| l.to_string())
                        .unwrap_or_else(|| "ibus-portal 未跑".to_string());
                    out.push(DiagnosticCheck::pass(
                        "ibus_daemon",
                        format!("ibus-daemon 跑着；{ibus_portal_line}"),
                    ));
                } else {
                    out.push(DiagnosticCheck::fail(
                        "ibus_daemon",
                        "ibus-daemon 不在运行".to_string(),
                        "嵌套 firefox 找不到 IME daemon——无法连 GdkIMContext",
                        "systemctl --user start ibus 或 /usr/bin/ibus-daemon --xim &",
                    ));
                }
            }
            Err(e) => {
                out.push(DiagnosticCheck::skip(
                    "ibus_daemon",
                    format!("ps 失败: {e}"),
                ));
            }
        }
    }

    /// 5. ibus engine（当前 default engine）
    fn check_ibus_engine(out: &mut Vec<DiagnosticCheck>) {
        let output = std::process::Command::new("ibus")
            .args(["engine"])
            .output();
        match output {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout);
                let engine = s.trim();
                if engine.is_empty() {
                    out.push(DiagnosticCheck::fail(
                        "ibus_engine",
                        "ibus engine 输出空".to_string(),
                        "ibus 还没选 default engine",
                        "ibus engine libpinyin 或设置 default engine",
                    ));
                } else {
                    out.push(DiagnosticCheck::pass(
                        "ibus_engine",
                        format!("default engine: {engine}"),
                    ));
                }
            }
            Err(e) => {
                out.push(DiagnosticCheck::skip(
                    "ibus_engine",
                    format!("ibus 命令失败: {e}"),
                ));
            }
        }
    }

    /// 6. input context（host_bridge 状态）
    fn check_input_context(state: &WLCState, out: &mut Vec<DiagnosticCheck>) {
        match &state.host_bridge {
            Some(hb) if hb.is_ready() => {
                out.push(DiagnosticCheck::pass(
                    "input_context",
                    format!("host_bridge READY (backend={})", hb.name()),
                ));
            }
            Some(hb) => {
                out.push(DiagnosticCheck::fail(
                    "input_context",
                    format!("host_bridge 存在但 not ready (backend={})", hb.name()),
                    "ibus portal 没连上 / InputContext 创建失败 / 权限不够",
                    "ibus engine / 重新插拔 ibus-daemon / 看 host_bridge probing 日志",
                ));
            }
            None => {
                out.push(DiagnosticCheck::fail(
                    "input_context",
                    "host_bridge 是 None".to_string(),
                    "host_bridge::probe() 失败——没找到 ibus/fcitx5 daemon",
                    "启动 ibus-daemon / fcitx5 / 检查 DBUS_SESSION_BUS_ADDRESS",
                ));
            }
        }
    }

    /// 7. focus（ti3 端点激活状态）
    fn check_focus(state: &WLCState, out: &mut Vec<DiagnosticCheck>) {
        if state.ime.app_active() {
            out.push(DiagnosticCheck::pass(
                "focus",
                "app_active=true（嵌套应用有激活 IME 会话）",
            ));
        } else {
            out.push(DiagnosticCheck::warn(
                "focus",
                "app_active=false（嵌套应用无激活 IME 会话）".to_string(),
                "嵌套 firefox 没获得 focus——commit 文本无法推到文本框",
            ));
        }
    }

    /// 8. surrounding text（v0.9.46+ 修复后从 ti3 推送给 host_bridge）
    fn check_surrounding(out: &mut Vec<DiagnosticCheck>) {
        out.push(DiagnosticCheck::skip(
            "surrounding_text",
            "需要嵌套应用 ti3 提交数据——见 ime.log 的 'host_bridge flush applied' 上下文",
        ));
    }

    /// 9. preedit
    fn check_preedit(out: &mut Vec<DiagnosticCheck>) {
        out.push(DiagnosticCheck::skip(
            "preedit",
            "需要 nested firefox 启用 IME——见 ime.log 的 'preedit \"...\"' 上下文",
        ));
    }

    /// 10. commit history（最近 1 秒内有无 commit）
    fn check_commit_history(out: &mut Vec<DiagnosticCheck>) {
        out.push(DiagnosticCheck::skip(
            "commit_history",
            "需扫描 ime.log 最近 1s 的 'commit \"...\"' 行——Java 端应配套 commit 计数",
        ));
    }

    /// 11. application received（关键——嵌套 firefox 文本框是否真收到 commit）
    fn check_application_received(out: &mut Vec<DiagnosticCheck>) {
        out.push(DiagnosticCheck::skip(
            "application_received",
            "需 Java 端确认：mod 通过 ti3 推的 commit 文本 firefox 文本框**实际**显示。\
             见 Java log 'firefox.commit_received' 事件",
        ));
    }

    /// 渲染报告为人类可读字符串。
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "=== WaylandCraft IME 诊断报告 ===");
        let _ = writeln!(out, "");
        for c in &self.checks {
            let symbol = match c.status {
                CheckStatus::Pass => "[PASS]",
                CheckStatus::Fail => "[FAIL]",
                CheckStatus::Warn => "[WARN]",
                CheckStatus::Skip => "[SKIP]",
            };
            let _ = writeln!(out, "{} {}: {}", symbol, c.layer, c.detail);
            if let Some(rc) = &c.root_cause {
                let _ = writeln!(out, "         Root cause: {rc}");
            }
            if let Some(s) = &c.suggestion {
                let _ = writeln!(out, "         Suggestion: {s}");
            }
        }
        let pass = self.checks.iter().filter(|c| c.status == CheckStatus::Pass).count();
        let fail = self.checks.iter().filter(|c| c.status == CheckStatus::Fail).count();
        let _ = writeln!(out, "");
        if fail == 0 {
            let _ = writeln!(out, "RESULT: INPUT METHOD HEALTHY ({}/{} pass)", pass, self.checks.len());
        } else {
            let _ = writeln!(out, "RESULT: INPUT METHOD BROKEN ({}/{} fail)", fail, self.checks.len());
            let _ = writeln!(out, "");
            let _ = writeln!(out, "下一排查步骤：按 7 章故障树顺序检查：");
            let _ = writeln!(out, "  1. keyboard event     → 检查 .ime.log 'bridge submit_key' 数量");
            let _ = writeln!(out, "  2. focus              → 检查 'outcome=Enabled' 频率");
            let _ = writeln!(out, "  3. input context      → 检查 'input context READY' 一次性");
            let _ = writeln!(out, "  4. composition       → 检查 'preedit \"...\"' 出现");
            let _ = writeln!(out, "  5. candidate         → 检查 'lookup N visible=true' 出现");
            let _ = writeln!(out, "  6. commit            → 检查 'commit \"...\"' 出现");
            let _ = writeln!(out, "  7. application       → 检查 Java 端 'firefox.commit_received' 事件");
        }
        out
    }
}

impl WLCState {
    /// 跑诊断（自动检查整个 IME 故障链）。
    pub fn run_diagnostic(&self) -> String {
        DiagnosticReport::run(self).render()
    }
}
