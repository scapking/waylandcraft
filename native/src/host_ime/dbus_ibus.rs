//! dbus-ibus 宿主输入法后端。
//!
//! 直接以 **org.freedesktop.IBus** 标准 DBus API 作为宿主输入法客户端 ——
//! 与 GTK/Qt/Chromium 在 GNOME 上使用的是同一条官方路径。核心价值：
//! **与窗口系统完全无关**，Minecraft 无论跑原生 Wayland 还是 XWayland 都可用，
//! 绕开 wayland-ti3 后端「enter 需要 surface/client 关联」的结构性限制。
//!
//! ## 协议映射
//!
//! ```text
//! ImeCommand::Activate   → FocusIn + SetCursorLocationRelative
//! ImeCommand::PushState  → SetCursorLocationRelative（候选窗光标定位）
//! ImeCommand::Deactivate → FocusOut
//! 原始按键               → ProcessKeyEvent(keysym, evdev, state) -> bool
//!   reply=true  → 按键被 IME 消费（preedit 更新走信号）
//!   reply=false → 放行，经 take_forwarded_keys 补投递给焦点应用
//! CommitText 信号        → HostEvent::CommitString + Done
//! UpdatePreedit* 信号    → HostEvent::PreeditString + Done
//! DeleteSurroundingText  → HostEvent::DeleteSurroundingText + Done
//! HidePreeditText        → HostEvent::PreeditString("",0,0) + Done
//! ForwardKeyEvent        → ForwardedKey（放行注入）
//! ```
//!
//! ## 线程模型（渲染线程零阻塞）
//!
//! - **命令线程**（1 个）：独占 ProcessKeyEvent 调用，保证按键顺序；
//! - **信号线程**（每个信号名 1 个）：阻塞迭代各自信号流，解析后统一
//!   送入同一个主线程接收队列。线程长期存活、绝大多数时间空闲。
//! - 主线程只做非阻塞 `try_recv` 轮询；按键裁决完全异步 —— `submit_key`
//!   只入队并立即返回「已接管」，reply 到达后的下一帧 `poll` 才决定
//!   丢弃或放行。**没有任何阻塞等待**，最坏情况是按键晚一帧到达应用。
//!
//! ## 能力协商与降级
//!
//! - 初始化前快速探测（2s 超时）：session bus 不存在 → TRANSIENT（桌面未
//!   就绪，可重试）；bus 正常但 org.freedesktop.IBus 无主 → UNSUPPORTED
//!   （本机不用 ibus）。
//! - Capabilities = PREEDIT | AUXILIARY | LOOKUP_TABLE | PROPERTY |
//!   FOCUS | SURROUNDING_TEXT (0x3F)，声明完整客户端能力。
//! - 信号体解析采用容错策略（在变体/结构体字段里定位首个字符串等），
//!   不硬编码 IBus 内部序列化细节；无法识别的形态记日志并安全丢弃，
//!   绝不让坏消息炸掉输入链路。

use super::{ForwardedKey, HostImBackend, SubmittedKey};
use crate::ime::ImeCommand;
use crate::seat::KeyboardAction;
use crate::system_ime::{HostEvent, ImeInit};
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

/// [system_ime] 日志宏（与 system_ime.rs 同款，独立复制避免作用域耦合）。
macro_rules! ime_log {
    ($($arg:tt)*) => {
        crate::bridge::ime_log_write(&format!($($arg)*))
    };
}

const IBUS_SERVICE: &str = "org.freedesktop.IBus";
const IBUS_FACTORY_PATH: &str = "/org/freedesktop/IBus";
const IBUS_FACTORY_IFACE: &str = "org.freedesktop.IBus";
const IBUS_IC_IFACE: &str = "org.freedesktop.IBus.InputContext";

/// IBUS_CAP_*：PREEDIT_TEXT | AUXILIARY_TEXT | LOOKUP_TABLE | PROPERTY |
/// FOCUS | SURROUNDING_TEXT。
const IC_CAPABILITIES: u32 = 0x3F;

/// IBUS_RELEASE_MASK（state 位掩码 bit30）。
const IBUS_RELEASE_MASK: u32 = 1 << 30;

/// 我们监听的 InputContext 信号名列表。
const WATCHED_SIGNALS: &[&str] = &[
    "CommitText",
    "UpdatePreeditText",
    "UpdatePreeditTextWithMode",
    "DeleteSurroundingText",
    "HidePreeditText",
    "ForwardKeyEvent",
];

// ── 主线程 ↔ 工作线程协议 ─────────────────────────────────────────

/// 发往命令线程的请求（顺序敏感：单消费者保证按键次序）。
enum ToWorker {
    FocusIn,
    FocusOut,
    SetCursorRect(i32, i32, i32, i32),
    /// keysym / evdev keycode / ibus state 位掩码。
    Key {
        seq: u64,
        keysym: u32,
        evdev: u32,
        state: u32,
    },
}

/// 工作线程 → 主线程的消息。
enum FromWorker {
    /// 输入上下文就绪（此后 Key/Focus 命令真正生效）。
    Ready,
    /// ProcessKeyEvent 往返结果；seq 对应 SubmittedKey::seq。
    KeyReply {
        seq: u64,
        consumed: bool,
    },
    /// 与 wayland-ti3 相同语义的宿主事件流。
    Ev(HostEvent),
    /// ForwardKeyEvent 等需要注入应用的按键。
    Forward(ForwardedKey),
    /// 连接终结，携带原因（此后工作线程全部退出）。
    Fatal(String),
}

#[derive(Debug)]
struct PendingKey {
    seq: u64,
    key: u32,
    action: KeyboardAction,
}

/// 主线程侧句柄。实现 [`HostImBackend`]。
pub struct DbusIbusBackend {
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

impl DbusIbusBackend {
    /// 快速探测 + 启动工作线程组。
    pub fn connect() -> ImeInit {
        ime_log!("[waylandcraft][host_ime][dbus-ibus] probing...");
        match probe_service_owner(IBUS_SERVICE) {
            Ok(()) => {}
            Err(ProbeErr::Unsupported(msg)) => {
                ime_log!("[waylandcraft][host_ime][dbus-ibus] UNSUPPORTED: {msg}");
                return ImeInit::Unsupported(format!("dbus-ibus: {msg}"));
            }
            Err(ProbeErr::Transient(msg)) => {
                ime_log!("[waylandcraft][host_ime][dbus-ibus] TRANSIENT: {msg}");
                return ImeInit::Transient(format!("dbus-ibus: {msg}"));
            }
        }

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();

        // 命令线程：初始化连接 → 就绪通知 → 串行处理 Focus/Cursor/Key。
        spawn_thread("wc-ibus-cmd", {
            let ev_tx = ev_tx.clone();
            move || {
                if let Err(e) = command_loop(cmd_rx, ev_tx.clone()) {
                    let _ = ev_tx.send(FromWorker::Fatal(e));
                }
            }
        });

        ime_log!("[waylandcraft][host_ime][dbus-ibus] worker started");
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

    /// 测试构造器：注入既有通道（不启动任何线程）。
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

// ── 快速探测 ─────────────────────────────────────────────────────

pub(crate) enum ProbeErr {
    Unsupported(String),
    Transient(String),
}

/// 独立小线程内做带超时的探测（不引入异步运行时依赖）。
pub(crate) fn spawn_thread<F: FnOnce() + Send + 'static>(name: &str, f: F) {
    let _ = std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .inspect_err(|e| eprintln!("[waylandcraft] thread {name} spawn failed: {e}"));
}

/// 通用服务在主探测：GetNameOwner（独立小线程 + 超时，不引入异步运行时）。
pub(crate) fn probe_service_owner(service: &str) -> Result<(), ProbeErr> {
    let (tx, rx) = std::sync::mpsc::channel();
    let svc = service.to_string();
    spawn_thread("wc-dbus-probe", move || {
        tx.send(probe_service_owner_impl(&svc)).ok();
    });
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(r) => r,
        Err(_) => Err(ProbeErr::Transient("bus 探测超时(3s)".into())),
    }
}

/// 向 bus daemon 查询 `service` 是否有主；无主 → Unsupported，其余错误 → Transient。
fn probe_service_owner_impl(service: &str) -> Result<(), ProbeErr> {
    use zbus::blocking::Proxy;

    let conn = zbus::blocking::Connection::session()
        .map_err(|e| ProbeErr::Transient(format!("无 session bus: {e}")))?;
    let dbus = Proxy::new(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .map_err(|e| ProbeErr::Transient(format!("dbus proxy: {e}")))?;
    let _owner: String = dbus
        .call::<_, _, String>("GetNameOwner", &(service,))
        .map_err(|e| {
            let s = e.to_string();
            if s.contains("ServiceUnknown")
                || s.contains("NameHasNoOwner")
                || s.contains("The name does not exist")
                || s.contains("1.2") // NameHasNoOwner error name: org.freedesktop.DBus.Error.NameHasNoOwner
            {
                ProbeErr::Unsupported(format!(
                    "session bus 上没有运行中的服务 {service}（名字无主）"
                ))
            } else {
                ProbeErr::Transient(format!("GetNameOwner 失败: {e}"))
            }
        })?;
    Ok(())
}

// ── 命令线程 ────────────────────────────────────────────────────

struct IcConnections {
    _conn: zbus::blocking::Connection,
    ic: zbus::blocking::Proxy<'static>,
}
fn connect_input_context() -> Result<IcConnections, String> {
    use zbus::blocking::Proxy;
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("session bus 连接失败: {e}"))?;
    let factory = Proxy::new(&conn, IBUS_SERVICE, IBUS_FACTORY_PATH, IBUS_FACTORY_IFACE)
        .map_err(|e| format!("factory proxy: {e}"))?;
    let ic_path: zbus::zvariant::OwnedObjectPath = factory
        .call::<_, _, zbus::zvariant::OwnedObjectPath>("CreateInputContext", &("waylandcraft",))
        .map_err(|e| format!("CreateInputContext: {e}"))?;
    // new_owned：得到 'static 的 Proxy（内部克隆连接），可安全移入信号线程。
    let ic: Proxy<'static> =
        Proxy::new_owned(conn.clone(), IBUS_SERVICE, ic_path, IBUS_IC_IFACE)
            .map_err(|e| format!("input context proxy: {e}"))?;
    ic.call::<_, _, ()>("SetCapabilities", &(IC_CAPABILITIES,))
        .map_err(|e| format!("SetCapabilities: {e}"))?;
    Ok(IcConnections { _conn: conn, ic })
}

fn command_loop(
    cmd_rx: Receiver<ToWorker>,
    ev_tx: Sender<FromWorker>,
) -> Result<(), String> {
    // 初始化限时：ibus 卡死时按 TRANSIENT 报告而非永久挂起。
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let setup_handle = std::thread::Builder::new()
        .name("wc-ibus-init".into())
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
    ime_log!("[waylandcraft][host_ime][dbus-ibus] input context READY");

    // 每个信号一个迭代线程（见模块文档：线程模型）。
    for sig in WATCHED_SIGNALS {
        let ic = ic_conns.ic.clone();
        let ev_tx = ev_tx.clone();
        let name = (*sig).to_string();
        spawn_thread("wc-ibus-sig", move || {
            match ic.receive_signal(name.as_str()) {
                Ok(iter) => {
                    for msg in iter {
                        match handle_signal(&name, &msg, &ev_tx) {
                            Ok(()) => {}
                            Err(e) => {
                                ime_log!(
                                    "[waylandcraft][host_ime][dbus-ibus] signal {name} 解析失败: {e}"
                                );
                            }
                        }
                    }
                    // 迭代器结束 = 连接断开。
                    let _ = ev_tx.send(FromWorker::Fatal(format!("signal {name} 流结束")));
                }
                Err(e) => {
                    let _ = ev_tx.send(FromWorker::Fatal(format!(
                        "订阅信号 {name} 失败: {e}"
                    )));
                }
            }
        });
    }

    // 命令主循环：单消费者，保序。
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
                .call::<_, _, ()>("SetCursorLocationRelative", &(x, y, w, h))
                .map(|_| ())
                .map_err(|e| e.to_string()),
            ToWorker::Key {
                seq,
                keysym,
                evdev,
                state,
            } => ic_conns
                .ic
                .call::<_, _, bool>("ProcessKeyEvent", &(keysym, evdev, state))
                .map_err(|e| format!("ProcessKeyEvent: {e}"))
                .and_then(|consumed| {
                    let _ = ev_tx.send(FromWorker::KeyReply { seq, consumed });
                    Ok(())
                }),
        };
        if let Err(e) = res {
            ime_log!("[waylandcraft][host_ime][dbus-ibus] 命令执行失败: {e}");
            let _ = ev_tx.send(FromWorker::Fatal(e.clone()));
            return Err(e);
        }
    }
    Ok(())
}

// ── 信号解析（容错式） ──────────────────────────────────────────

/// 从消息体里取出所有字段的动态视图。
type StaticFields = Vec<zbus::zvariant::Value<'static>>;

pub(crate) fn body_fields(msg: &zbus::message::Message) -> Result<StaticFields, String> {
    let body = msg.body();
    // 单参数与多参数两种形态统一成字段列表（全部转为 'static 值）。
    if let Ok(v) = body.deserialize::<zbus::zvariant::OwnedValue>() {
        return Ok(vec![zbus::zvariant::Value::from(v)]);
    }
    if let Ok(fields) = body.deserialize::<Vec<zbus::zvariant::OwnedValue>>() {
        return Ok(fields.into_iter().map(zbus::zvariant::Value::from).collect());
    }
    Err("无法反序列化消息体".into())
}

/// 在字段（或变体内层结构体字段）里找第一个字符串 —— IBusText 的文本位。
pub(crate) fn find_text(values: &[zbus::zvariant::Value<'static>]) -> Option<String> {
    for v in values {
        match v {
            zbus::zvariant::Value::Str(s) => return Some(s.as_str().to_string()),
            zbus::zvariant::Value::Value(inner) => {
                if let Some(t) = find_text(std::slice::from_ref(inner.as_ref())) {
                    return Some(t);
                }
            }
            zbus::zvariant::Value::Structure(s) => {
                if let Some(t) = find_text(s.fields()) {
                    return Some(t);
                }
            }
            _ => {}
        }
    }
    None
}

/// 找第一个整数（i32/u32 归一化）。
pub(crate) fn find_int(values: &[zbus::zvariant::Value<'static>]) -> Option<i64> {
    for v in values {
        match v {
            zbus::zvariant::Value::U8(n) => return Some(*n as i64),
            zbus::zvariant::Value::U16(n) => return Some(*n as i64),
            zbus::zvariant::Value::I16(n) => return Some(*n as i64),
            zbus::zvariant::Value::U32(n) => return Some(*n as i64),
            zbus::zvariant::Value::I32(n) => return Some(*n as i64),
            zbus::zvariant::Value::U64(n) => return Some(*n as i64),
            zbus::zvariant::Value::I64(n) => return Some(*n),
            zbus::zvariant::Value::Value(inner) => {
                if let Some(n) = find_int(std::slice::from_ref(inner.as_ref())) {
                    return Some(n);
                }
            }
            zbus::zvariant::Value::Structure(s) => {
                if let Some(n) = find_int(s.fields()) {
                    return Some(n);
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn find_bool(values: &[zbus::zvariant::Value<'static>]) -> Option<bool> {
    for v in values {
        match v {
            zbus::zvariant::Value::Bool(b) => return Some(*b),
            zbus::zvariant::Value::Value(inner) => {
                if let Some(b) = find_bool(std::slice::from_ref(inner.as_ref())) {
                    return Some(b);
                }
            }
            _ => {}
        }
    }
    None
}

/// 提交一批文本事件并立即补 Done（原子应用单位 = 单条信号）。
pub(crate) fn push_with_done(ev_tx: &Sender<FromWorker>, ev: HostEvent) {
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
        "CommitText" => {
            let text = find_text(&fields)
                .ok_or_else(|| "CommitText 缺少文本字段".to_string())?;
            ime_log!("[waylandcraft][host_ime][dbus-ibus] commit {text:?}");
            push_with_done(ev_tx, HostEvent::CommitString(text));
        }
        "UpdatePreeditText" | "UpdatePreeditTextWithMode" => {
            let text = find_text(&fields).unwrap_or_default();
            let cursor = find_int(&fields).unwrap_or(text.chars().count() as i64) as i32;
            let visible = find_bool(&fields).unwrap_or(!text.is_empty());
            if visible && !text.is_empty() {
                ime_log!(
                    "[waylandcraft][host_ime][dbus-ibus] preedit {text:?} cursor={cursor}"
                );
                push_with_done(
                    ev_tx,
                    HostEvent::PreeditString(text, cursor, cursor),
                );
            } else {
                push_with_done(ev_tx, HostEvent::PreeditString(String::new(), 0, 0));
            }
        }
        "DeleteSurroundingText" => {
            let before = find_int(&fields).unwrap_or(0) as u32;
            push_with_done(
                ev_tx,
                HostEvent::DeleteSurroundingText(before, 0),
            );
        }
        "HidePreeditText" => {
            push_with_done(ev_tx, HostEvent::PreeditString(String::new(), 0, 0));
        }
        "ForwardKeyEvent" => {
            // (keyval, keycode(evdev), state)
            let nums: Vec<i64> =
                fields.iter().filter_map(|v| find_int(std::slice::from_ref(v))).collect();
            let evdev = nums.get(1).copied().unwrap_or(0) as u32;
            let state = nums.get(2).copied().unwrap_or(0) as u32;
            let action = if state & IBUS_RELEASE_MASK != 0 {
                KeyboardAction::Release
            } else {
                KeyboardAction::Press
            };
            let _ = ev_tx.send(FromWorker::Forward(ForwardedKey {
                key: evdev.saturating_add(8),
                action,
            }));
        }
        other => {
            return Err(format!("未注册的信号 {other}"));
        }
    }
    Ok(())
}

// ── HostImBackend 实现 ──────────────────────────────────────────

impl HostImBackend for DbusIbusBackend {
    fn name(&self) -> &'static str {
        "dbus-ibus"
    }

    fn is_ready(&self) -> bool {
        self.ready && self.dead.is_none()
    }

    fn set_active(&mut self, active: bool) {
        if self.want_enabled == active {
            return;
        }
        self.want_enabled = active;
        ime_log!("[waylandcraft][host_ime][dbus-ibus] set_active -> {active}");
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
        if let Some(msg) = self.dead.clone() {
            let _ = msg;
            return;
        }
        loop {
            match self.ev_rx.try_recv() {
                Ok(FromWorker::Ready) => {
                    self.ready = true;
                    ime_log!(
                        "[waylandcraft][host_ime][dbus-ibus] READY (main side)"
                    );
                }
                Ok(FromWorker::KeyReply { seq, consumed }) => {
                    // 队列头必须对应当次 reply；错位说明有丢失，重同步并如实记录。
                    match self.pending.front() {
                        Some(p) if p.seq == seq => {
                            let p = self.pending.pop_front().expect("front checked");
                            if !consumed {
                                self.forwards.push(ForwardedKey {
                                    key: p.key,
                                    action: p.action,
                                });
                            } else {
                                ime_log!(
                                    "[waylandcraft][host_ime][dbus-ibus] key seq={seq} consumed by IME"
                                );
                            }
                        }
                        other => {
                            ime_log!(
                                "[waylandcraft][host_ime][dbus-ibus] KeyReply seq={seq} 错位（队首 {:?}）-> 重同步丢弃",
                                other.map(|p| p.seq)
                            );
                            self.pending.retain(|p| p.seq != seq);
                        }
                    }
                }
                Ok(FromWorker::Ev(e)) => self.events.push(e),
                Ok(FromWorker::Forward(f)) => self.forwards.push(f),
                Ok(FromWorker::Fatal(msg)) => {
                    ime_log!("[waylandcraft][host_ime][dbus-ibus] FATAL: {msg}");
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
            return false; // 未就绪不接管：按键照常直投，绝不丢键
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
    use crate::seat::KeyboardAction;

    /// 主线程侧状态机：submit → KeyReply(consumed=false) → 放行。
    #[test]
    fn key_roundtrip_forward() {
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusIbusBackend::from_parts(cmd_tx, ev_rx);

        assert!(be.submit_key(SubmittedKey {
            seq: 1,
            key: 38,
            keysym: 'a' as u32,
            evdev: 30,
            state: 0,
            action: KeyboardAction::Press,
            mods: (0, 0, 0, 0),
        }));
        assert!(be.take_forwarded_keys().is_empty(), "reply 未到不放行");

        ev_tx.send(FromWorker::KeyReply { seq: 1, consumed: false }).unwrap();
        be.poll();
        let fwd = be.take_forwarded_keys();
        assert_eq!(fwd.len(), 1);
        assert_eq!(fwd[0].key, 38);
        assert_eq!(fwd[0].action, KeyboardAction::Press);
    }

    /// consumed=true 的按键被丢弃（IME 吃掉了）。
    #[test]
    fn key_roundtrip_consumed() {
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusIbusBackend::from_parts(cmd_tx, ev_rx);

        be.submit_key(SubmittedKey {
            seq: 7,
            key: 24,
            keysym: 'q' as u32,
            evdev: 16,
            state: 0,
            action: KeyboardAction::Press,
            mods: (0, 0, 0, 0),
        });
        ev_tx.send(FromWorker::KeyReply { seq: 7, consumed: true }).unwrap();
        be.poll();
        assert!(be.pending.is_empty());
        assert!(be.take_forwarded_keys().is_empty());
    }

    /// 多键乱序到达时按 FIFO 裁决；信号事件带原子 Done。
    #[test]
    fn ordering_and_done_pairing() {
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusIbusBackend::from_parts(cmd_tx, ev_rx);

        for seq in 1..=3u64 {
            be.submit_key(SubmittedKey {
                seq,
                key: 10 + seq as u32,
                keysym: 0x61,
                evdev: seq as u32,
                state: 0,
                action: KeyboardAction::Press,
                mods: (0, 0, 0, 0),
            });
        }
        // reply 乱序回来（2 先到）：FIFO 队首不匹配 → 该 seq 重同步移除。
        ev_tx.send(FromWorker::KeyReply { seq: 2, consumed: false }).unwrap();
        be.poll();
        assert!(be.pending.iter().all(|p| p.seq != 2), "错位 seq 应被移除");

        // 正常路径：1、3 依次放行。
        ev_tx.send(FromWorker::KeyReply { seq: 1, consumed: false }).unwrap();
        ev_tx.send(FromWorker::KeyReply { seq: 3, consumed: false }).unwrap();
        be.poll();
        let fwd = be.take_forwarded_keys();
        assert_eq!(fwd.len(), 2);

        // CommitText → CommitString + Done 成对出现。
        push_with_done(
            &ev_tx,
            HostEvent::CommitString("你好".into()),
        );
        drop(ev_tx); // 关闭通道结束轮询
        be.poll();
        let evs = be.take_events();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0], HostEvent::CommitString("你好".into()));
        assert_eq!(evs[1], HostEvent::Done(0));
    }

    /// 未就绪时 submit_key 不接管（返回 false → 按键直投）。
    #[test]
    fn not_ready_no_grab() {
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (_ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusIbusBackend::from_parts(cmd_tx, ev_rx);
        be.ready = false;
        assert!(!be.submit_key(SubmittedKey {
            seq: 9,
            key: 30,
            keysym: 0x31,
            evdev: 2,
            state: 0,
            action: KeyboardAction::Press,
            mods: (0, 0, 0, 0),
        }));
    }

    /// Fatal 消息置 dead；此后 poll/submit 不再工作。
    #[test]
    fn fatal_marks_dead() {
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusIbusBackend::from_parts(cmd_tx, ev_rx);

        ev_tx.send(FromWorker::Fatal("bus gone".into())).unwrap();
        drop(ev_tx);
        be.poll();
        assert!(HostImBackend::is_dead(&be));

        assert!(!be.submit_key(SubmittedKey {
            seq: 11,
            key: 30,
            keysym: 0x31,
            evdev: 2,
            state: 0,
            action: KeyboardAction::Press,
            mods: (0, 0, 0, 0),
        }));
    }
}
