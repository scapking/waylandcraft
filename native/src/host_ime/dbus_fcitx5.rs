//! dbus-fcitx5 宿主输入法后端。
//!
//! 对接 fcitx5 的 **dbus frontend**（`org.freedesktop.portal.Fcitx`，
//! 接口 `org.fcitx.Fcitx.InputMethod1` / `InputContext1`）—— 与 fcitx5
//! 自家 GTK/Qt im 模块同一条路径。规格依据 fcitx5 上游
//! `src/frontend/dbusfrontend/dbusfrontend.cpp`（v5 master）：
//!
//! ```text
//! CreateInputContext  "a(ss)" → "oay"     （(program,display) 对；返回 IC 路径）
//! ProcessKeyEvent     "uuubu" → "b"       （keyval,evdev,state,release,time → handled?）
//! FocusIn/FocusOut    ""      → ""
//! SetCursorRect       "iiii"  → ""        （候选窗光标矩形，窗口相对像素）
//! ── 信号 ──
//! CommitString            "s"
//! UpdateFormattedPreedit  "a(si)i"        （分段 preedit + 总游标）
//! DeleteSurroundingText   "iu"            （offset,nchars，游标相对区间）
//! ForwardKey              "uub"           （keyval,evdev,is_release → 注入应用）
//! ```
//!
//! 线程模型、零阻塞按键裁决、HostEvent/Done 原子配对与 [`super::dbus_ibus`]
//! 完全一致；仅协议编解码不同。
//!
//! ## 如实说明
//!
//! 本仓库开发容器无 fcitx5 实机环境：本后端按上游源码规格实现 + 主线程侧
//! 单元测试覆盖，真实 fcitx5 桌面回归待社区验证（同 docs/IME.md §8 惯例）。

use super::dbus_ibus::{body_fields, find_int, find_text};
use super::{ForwardedKey, HostImBackend, SubmittedKey, IBUS_RELEASE_MASK};
use crate::ime::ImeCommand;
use crate::seat::KeyboardAction;
use crate::system_ime::{HostEvent, ImeInit};
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

macro_rules! ime_log {
    ($($arg:tt)*) => {
        crate::bridge::ime_log_write(&format!($($arg)*))
    };
}

const FCITX_SERVICE: &str = "org.freedesktop.portal.Fcitx";
const FCITX_IM_PATH: &str = "/org/freedesktop/portal/inputmethod";
const FCITX_IM_IFACE: &str = "org.fcitx.Fcitx.InputMethod1";
const FCITX_IC_IFACE: &str = "org.fcitx.Fcitx.InputContext1";

/// 我们监听的 InputContext 信号。
const WATCHED_SIGNALS: &[&str] =
    &["CommitString", "UpdateFormattedPreedit", "DeleteSurroundingText", "ForwardKey"];

enum ToWorker {
    FocusIn,
    FocusOut,
    SetCursorRect(i32, i32, i32, i32),
    /// keyval / evdev / ibus 风格 state / 是否 release / 时间戳 ms。
    Key {
        seq: u64,
        keysym: u32,
        evdev: u32,
        state: u32,
        release: bool,
        time_ms: u32,
    },
}

enum FromWorker {
    Ready,
    KeyReply {
        seq: u64,
        consumed: bool,
    },
    Ev(HostEvent),
    Forward(ForwardedKey),
    Fatal(String),
}

#[derive(Debug)]
struct PendingKey {
    seq: u64,
    key: u32,
    action: KeyboardAction,
}

pub struct DbusFcitx5Backend {
    cmd_tx: Sender<ToWorker>,
    ev_rx: Receiver<FromWorker>,
    events: Vec<HostEvent>,
    pending: VecDeque<PendingKey>,
    forwards: Vec<ForwardedKey>,
    ready: bool,
    dead: Option<String>,
    want_enabled: bool,
    focused: bool,
    last_cursor: Option<(i32, i32, i32, i32)>,
}

impl DbusFcitx5Backend {
    pub fn connect() -> ImeInit {
        ime_log!("[waylandcraft][host_ime][dbus-fcitx5] probing...");
        // 服务在不在：GetNameOwner 快速探测（复用 ibus 的分类逻辑）。
        match super::dbus_ibus::probe_service_owner(FCITX_SERVICE) {
            Ok(()) => {}
            Err(super::dbus_ibus::ProbeErr::Unsupported(msg)) => {
                ime_log!("[waylandcraft][host_ime][dbus-fcitx5] UNSUPPORTED: {msg}");
                return ImeInit::Unsupported(format!("dbus-fcitx5: {msg}"));
            }
            Err(super::dbus_ibus::ProbeErr::Transient(msg)) => {
                ime_log!("[waylandcraft][host_ime][dbus-fcitx5] TRANSIENT: {msg}");
                return ImeInit::Transient(format!("dbus-fcitx5: {msg}"));
            }
        }

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();

        super::dbus_ibus::spawn_thread("wc-fcitx5-cmd", {
            let ev_tx = ev_tx.clone();
            move || {
                if let Err(e) = command_loop(cmd_rx, ev_tx.clone()) {
                    let _ = ev_tx.send(FromWorker::Fatal(e));
                }
            }
        });

        ime_log!("[waylandcraft][host_ime][dbus-fcitx5] worker started");
        ImeInit::Ready(Box::new(Self {
            cmd_tx,
            ev_rx,
            events: Vec::new(),
            pending: VecDeque::new(),
            forwards: Vec::new(),
            ready: false,
            dead: None,
            want_enabled: false,
            focused: false,
            last_cursor: None,
        }))
    }

    #[cfg(test)]
    fn from_parts(cmd_tx: Sender<ToWorker>, ev_rx: Receiver<FromWorker>) -> Self {
        Self {
            cmd_tx,
            ev_rx,
            events: Vec::new(),
            pending: VecDeque::new(),
            forwards: Vec::new(),
            ready: true,
            dead: None,
            want_enabled: false,
            focused: false,
            last_cursor: None,
        }
    }
}

struct IcConnections {
    _conn: zbus::blocking::Connection,
    ic: zbus::blocking::Proxy<'static>,
}

fn connect_input_context() -> Result<IcConnections, String> {
    use zbus::blocking::Proxy;
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("session bus 连接失败: {e}"))?;
    let factory = Proxy::new_owned(
        conn.clone(),
        FCITX_SERVICE,
        FCITX_IM_PATH,
        FCITX_IM_IFACE,
    )
    .map_err(|e| format!("factory proxy: {e}"))?;
    // CreateInputContext("a(ss)") -> "(oay)"：IC 路径 + 客户端标识字节。
    let reply: (zbus::zvariant::OwnedObjectPath, Vec<u8>) = factory
        .call::<_, _, (zbus::zvariant::OwnedObjectPath, Vec<u8>)>(
            "CreateInputContext",
            &((
                vec![
                    ("program".to_string(), "waylandcraft".to_string()),
                    ("display".to_string(), String::new()),
                ],
            )),
        )
        .map_err(|e| format!("CreateInputContext: {e}"))?;
    let ic_path = reply.0;
    let ic: Proxy<'static> =
        Proxy::new_owned(conn.clone(), FCITX_SERVICE, ic_path, FCITX_IC_IFACE)
            .map_err(|e| format!("input context proxy: {e}"))?;
    Ok(IcConnections { _conn: conn, ic })
}

fn command_loop(
    cmd_rx: Receiver<ToWorker>,
    ev_tx: Sender<FromWorker>,
) -> Result<(), String> {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let setup_handle = std::thread::Builder::new()
        .name("wc-fcitx5-init".into())
        .spawn(move || done_tx.send(connect_input_context()))
        .map_err(|e| format!("init thread: {e}"))?;
    let ic_conns = match done_rx.recv_timeout(Duration::from_secs(6)) {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return Err(format!("init failed: {e}")),
        Err(_) => {
            setup_handle.join().ok();
            return Err("init timeout(6s)".into());
        }
    };

    let _ = ev_tx.send(FromWorker::Ready);
    ime_log!("[waylandcraft][host_ime][dbus-fcitx5] input context READY");

    for sig in WATCHED_SIGNALS {
        let ic = ic_conns.ic.clone();
        let ev_tx = ev_tx.clone();
        let name = (*sig).to_string();
        super::dbus_ibus::spawn_thread("wc-fcitx5-sig", move || match ic.receive_signal(name.as_str()) {
            Ok(iter) => {
                for msg in iter {
                    if let Err(e) = handle_signal(&name, &msg, &ev_tx) {
                        ime_log!(
                            "[waylandcraft][host_ime][dbus-fcitx5] signal {name} 解析失败: {e}"
                        );
                    }
                }
                let _ = ev_tx.send(FromWorker::Fatal(format!("signal {name} 流结束")));
            }
            Err(e) => {
                let _ =
                    ev_tx.send(FromWorker::Fatal(format!("订阅信号 {name} 失败: {e}")));
            }
        });
    }

    while let Ok(cmd) = cmd_rx.recv() {
        let res: Result<(), String> = match cmd {
            ToWorker::FocusIn => ic_conns
                .ic
                .call::<_, _, ()>("FocusIn", &())
                .map(|_| ())
                .map_err(|e| e.to_string()),
            ToWorker::FocusOut => ic_conns
                .ic
                .call::<_, _, ()>("FocusOut", &())
                .map(|_| ())
                .map_err(|e| e.to_string()),
            ToWorker::SetCursorRect(x, y, w, h) => ic_conns
                .ic
                .call::<_, _, ()>("SetCursorRect", &(x, y, w, h))
                .map(|_| ())
                .map_err(|e| e.to_string()),
            ToWorker::Key {
                seq,
                keysym,
                evdev,
                state,
                release,
                time_ms,
            } => ic_conns
                .ic
                .call::<_, _, bool>(
                    "ProcessKeyEvent",
                    &(keysym, evdev, state, release, time_ms),
                )
                .map_err(|e| format!("ProcessKeyEvent: {e}"))
                .and_then(|consumed| {
                    let _ = ev_tx.send(FromWorker::KeyReply { seq, consumed });
                    Ok(())
                }),
        };
        if let Err(e) = res {
            ime_log!("[waylandcraft][host_ime][dbus-fcitx5] 命令执行失败: {e}");
            let _ = ev_tx.send(FromWorker::Fatal(e.clone()));
            return Err(e);
        }
    }
    Ok(())
}

/// "a(si)i" → (拼接文本, 游标)。段内 i 为该段的游标标记（fcitx 内部语义），
/// 总游标取末尾 i32。
fn parse_formatted_preedit(
    fields: &[zbus::zvariant::Value<'static>],
) -> Option<(String, i32)> {
    let mut text = String::new();
    let mut cursor: Option<i32> = None;
    for v in fields {
        match v {
            // a(si)/av：分段 preedit；段内 i32 是段内标记位，不是总游标 —— 忽略。
            // 元素可能是变体包装（av）也可能是直接结构体（a(si)），两者都接受。
            zbus::zvariant::Value::Array(a) => {
                for e in a.iter() {
                    let ev: &zbus::zvariant::Value = match e {
                        zbus::zvariant::Value::Value(inner) => inner.as_ref(),
                        other => other,
                    };
                    if let zbus::zvariant::Value::Structure(st) = ev {
                        for f in st.fields() {
                            if let zbus::zvariant::Value::Str(s) = f {
                                text.push_str(s.as_str());
                            }
                        }
                    }
                }
            }
            // 尾随顶层 i32 = 拼接后字符串中的总游标位置。
            other => {
                if cursor.is_none() && let Some(n) = find_int(std::slice::from_ref(other)) {
                    cursor = Some(n as i32);
                }
            }
        }
    }
    let fallback_cursor = text.chars().count() as i32;
    Some((text, cursor.unwrap_or(fallback_cursor)))
}

/// 提交一批文本事件并立即补 Done（原子应用单位 = 单条信号）。
fn push_with_done(ev_tx: &Sender<FromWorker>, ev: HostEvent) {
    let _ = ev_tx.send(FromWorker::Ev(ev));
    let _ = ev_tx.send(FromWorker::Ev(HostEvent::Done(0)));
}

fn handle_signal(
    name: &str,
    msg: &zbus::message::Message,
    ev_tx: &Sender<FromWorker>,
) -> Result<(), String> {
    let fields = body_fields(msg)?;
    match name {
        "CommitString" => {
            let text = find_text(&fields)
                .ok_or_else(|| "CommitString 缺少文本字段".to_string())?;
            ime_log!("[waylandcraft][host_ime][dbus-fcitx5] commit {text:?}");
            push_with_done(ev_tx, HostEvent::CommitString(text));
        }
        "UpdateFormattedPreedit" => {
            let (text, cursor) =
                parse_formatted_preedit(&fields).unwrap_or_default();
            if !text.is_empty() {
                ime_log!(
                    "[waylandcraft][host_ime][dbus-fcitx5] preedit {text:?} cursor={cursor}"
                );
                push_with_done(ev_tx, HostEvent::PreeditString(text, cursor, cursor));
            } else {
                push_with_done(ev_tx, HostEvent::PreeditString(String::new(), 0, 0));
            }
        }
        "DeleteSurroundingText" => {
            // (i offset, u nchars)：游标相对区间 [cursor+offset, cursor+offset+nchars)
            // → ti3 语义 (before_length, after_length)。
            let offset = find_int(&fields).unwrap_or(0) as i32;
            let nchars = fields
                .get(1)
                .map(|v| find_int(std::slice::from_ref(v)))
                .unwrap_or(Some(0))
                .unwrap_or(0) as u32;
            let (before, after) = if offset <= 0 {
                (
                    (-offset) as u32,
                    nchars.saturating_sub((-offset) as u32),
                )
            } else {
                (0u32, nchars)
            };
            push_with_done(ev_tx, HostEvent::DeleteSurroundingText(before, after));
        }
        "ForwardKey" => {
            let nums: Vec<i64> = fields
                .iter()
                .filter_map(|v| find_int(std::slice::from_ref(v)))
                .collect();
            let evdev = nums.get(1).copied().unwrap_or(0) as u32;
            let is_release = nums.get(2).copied().unwrap_or(0) != 0;
            let _ = ev_tx.send(FromWorker::Forward(ForwardedKey {
                key: evdev.saturating_add(8),
                action: if is_release {
                    KeyboardAction::Release
                } else {
                    KeyboardAction::Press
                },
            }));
        }
        other => return Err(format!("未注册的信号 {other}")),
    }
    Ok(())
}

impl HostImBackend for DbusFcitx5Backend {
    fn name(&self) -> &'static str {
        "dbus-fcitx5"
    }

    fn is_ready(&self) -> bool {
        self.ready && self.dead.is_none()
    }

    fn set_active(&mut self, active: bool) {
        if self.want_enabled == active {
            return;
        }
        self.want_enabled = active;
        if active {
            let _ = self.cmd_tx.send(ToWorker::FocusIn);
            self.focused = true;
        } else {
            let _ = self.cmd_tx.send(ToWorker::FocusOut);
            self.focused = false;
        }
    }

    fn execute_commands(&mut self, commands: Vec<ImeCommand>) {
        if self.dead.is_some() {
            return;
        }
        for cmd in commands {
            match cmd {
                ImeCommand::Activate(st) => {
                    self.want_enabled = true;
                    if !self.focused {
                        let _ = self.cmd_tx.send(ToWorker::FocusIn);
                        self.focused = true;
                    }
                    if let Some(r) = st.cursor_rect {
                        HostImBackend::update_cursor_rect(self, r);
                    }
                }
                ImeCommand::PushState(st) => {
                    if let Some(r) = st.cursor_rect {
                        HostImBackend::update_cursor_rect(self, r);
                    }
                }
                ImeCommand::Deactivate => {
                    self.want_enabled = false;
                    if self.focused {
                        let _ = self.cmd_tx.send(ToWorker::FocusOut);
                        self.focused = false;
                    }
                }
            }
        }
    }

    fn poll(&mut self) {
        if self.dead.is_some() {
            return;
        }
        loop {
            match self.ev_rx.try_recv() {
                Ok(FromWorker::Ready) => {
                    self.ready = true;
                    ime_log!("[waylandcraft][host_ime][dbus-fcitx5] READY (main side)");
                }
                Ok(FromWorker::KeyReply { seq, consumed }) => match self.pending.front() {
                    Some(p) if p.seq == seq => {
                        let p = self.pending.pop_front().expect("front checked");
                        if !consumed {
                            self.forwards.push(ForwardedKey {
                                key: p.key,
                                action: p.action,
                            });
                        }
                    }
                    other => {
                        ime_log!(
                            "[waylandcraft][host_ime][dbus-fcitx5] KeyReply seq={seq} 错位（队首 {:?}）-> 重同步丢弃",
                            other.map(|p| p.seq)
                        );
                        self.pending.retain(|p| p.seq != seq);
                    }
                },
                Ok(FromWorker::Ev(e)) => self.events.push(e),
                Ok(FromWorker::Forward(f)) => self.forwards.push(f),
                Ok(FromWorker::Fatal(msg)) => {
                    ime_log!("[waylandcraft][host_ime][dbus-fcitx5] FATAL: {msg}");
                    self.dead = Some(msg);
                    return;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.dead = Some("worker channel closed".into());
                    return;
                }
            }
        }
    }

    fn take_events(&mut self) -> Vec<HostEvent> {
        std::mem::take(&mut self.events)
    }

    fn take_forwarded_keys(&mut self) -> Vec<ForwardedKey> {
        std::mem::take(&mut self.forwards)
    }

    fn submit_key(&mut self, sk: SubmittedKey) -> bool {
        if !self.ready || self.dead.is_some() {
            return false;
        }
        self.pending.push_back(PendingKey {
            seq: sk.seq,
            key: sk.key,
            action: sk.action,
        });
        let _ = self.cmd_tx.send(ToWorker::Key {
            seq: sk.seq,
            keysym: sk.keysym,
            evdev: sk.evdev,
            state: sk.state,
            release: sk.action == KeyboardAction::Release,
            time_ms: 0,
        });
        true
    }

    fn update_cursor_rect(&mut self, rect: (i32, i32, i32, i32)) {
        if self.last_cursor == Some(rect) {
            return;
        }
        self.last_cursor = Some(rect);
        let (x, y, w, h) = rect;
        let _ = self.cmd_tx.send(ToWorker::SetCursorRect(x, y, w, h));
    }

    fn is_dead(&self) -> bool {
        self.dead.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// parse_formatted_preedit：a(si) 分段拼接 + 尾部游标。
    #[test]
    fn preedit_parsing() {
        use zbus::zvariant as zv;
        // 构造 Value::Array([Struct[Str"ni",I32 0], Struct[Str"hao",I32 0]]) , I32 2
        let seg = |t: String| {
            let mut b = zv::StructureBuilder::new();
            b.push_value(zv::Value::from(t));
            b.push_value(zv::Value::from(0i32));
            zv::Value::Structure(b.build().expect("structure build"))
        };
        let arr = zv::Value::Array(
            vec![seg("ni".to_string()), seg("hao".to_string())]
                .try_into()
                .expect("array build"),
        );
        let fields = vec![arr, zv::Value::from(4i32)];
        let (text, cursor) = parse_formatted_preedit(&fields).expect("parse");
        assert_eq!(text, "nihao");
        assert_eq!(cursor, 4);
    }

    /// 未就绪不接管。
    #[test]
    fn not_ready_no_grab() {
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (_ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusFcitx5Backend::from_parts(cmd_tx, ev_rx);
        be.ready = false;
        assert!(!be.submit_key(SubmittedKey {
            seq: 1,
            key: 38,
            keysym: 0x61,
            evdev: 30,
            state: IBUS_RELEASE_MASK,
            action: KeyboardAction::Release,
            mods: (0, 0, 0, 0),
        }));
    }
}
