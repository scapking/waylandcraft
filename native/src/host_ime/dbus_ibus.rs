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
use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

/// [system_ime] 日志宏（与 system_ime.rs 同款，独立复制避免作用域耦合）。
macro_rules! ime_log {
    ($($arg:tt)*) => {
        crate::bridge::ime_log_write(&format!($($arg)*))
    };
}

// ── 入口选择：session bus 上的 ibus portal ──
// ibus 客户端(GTK/Qt)默认连 ibus 私有总线地址(IBUS_ADDRESS 文件)，session bus
// 上同名服务并不实现 IBus 接口 —— 这正是 v0.9.29/30 UnknownMethod 的根因。
// 而 ibus-portal 守护进程在 session bus 上以 org.freedesktop.portal.IBus 名字
// 转发同一套 API(flatpak 应用即走此路径)。Ubuntu/GNOME 默认随 ibus 启动。
// 常量对齐上游 src/ibusshare.h：
//   IBUS_SERVICE_PORTAL="org.freedesktop.portal.IBus"
//   IBUS_PATH_IBUS="/org/freedesktop/IBus"
//   IBUS_INTERFACE_PORTAL="org.freedesktop.IBus.Portal"
//   IC 对象仍用标准 org.freedesktop.IBus.InputContext。
const IBUS_SERVICE: &str = "org.freedesktop.portal.IBus";
const IBUS_FACTORY_PATH: &str = "/org/freedesktop/IBus";
const IBUS_FACTORY_IFACE: &str = "org.freedesktop.IBus.Portal";
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
    "UpdateLookupTable",
    "ShowLookupTable",
    "HideLookupTable",
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
    KeyReply { seq: u64, consumed: bool },
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
    /// P1：被 IME 消费（consumed）的按键；其 release 到达时直接配对吃掉，
    /// 不再提交 ibus（杜绝 preedit 清空后 release 被放行注入应用）。
    ime_consumed: HashSet<u32>,
    /// P1：press 仍在裁决中（未回 KeyReply）时到达的 release，挂起等待裁决。
    release_waiting: VecDeque<u32>,
    forwards: Vec<ForwardedKey>,
    ready: bool,
    dead: Option<String>,
    want_enabled: bool,
    focused: bool,
    last_cursor: Option<(i32, i32, i32, i32)>,
    /// P2：ti3 光标优先（WaylandCraft 世界内窗口如 firefox 的真实光标；
    /// 由 ImeCommand::Activate/PushState 的 st.cursor_rect 驱动）。
    /// true 时 Java update_cursor_rect 不覆盖；false 时只信 Java 上报。
    cursor_prefer_ti3: bool,
}

impl DbusIbusBackend {
    /// P2：ti3 上报的光标矩形（经 relay 的 st.cursor_rect）→ ibus 候选窗锚点。
    /// 日志带来源标记 (ti3)，与 Java 的 (java) 行区分，实机可验证候选窗跟随哪个源。
    fn set_cursor_from_ti3(&mut self, rect: (i32, i32, i32, i32)) {
        if self.last_cursor == Some(rect) {
            return;
        }
        self.last_cursor = Some(rect);
        let (x, y, w, h) = rect;
        ime_log!(
            "[waylandcraft][host_ime][dbus-ibus] SetCursorLocationRelative (ti3) ({x},{y},{w},{h})"
        );
        let _ = self.cmd_tx.send(ToWorker::SetCursorRect(x, y, w, h));
    }

    /// 快速探测 + 启动工作线程组。
    /// 完整初始化（探测 + 真实建 IC）都在调用线程同步完成并分类：
    /// 协议性失败当场 Unsupported（不进入 5 秒重试循环刷屏），
    /// 环境/超时类失败 Transient。成功后把已建连接移交给命令线程。
    pub fn connect() -> ImeInit {
        ime_log!("[waylandcraft][host_ime][dbus-ibus] probing...");
        match probe_service_owner(IBUS_SERVICE) {
            Ok(()) => {}
            Err(ProbeErr::Unsupported(msg)) => {
                ime_log!(
                    "[waylandcraft][host_ime][dbus-ibus] UNSUPPORTED: {msg}"
                );
                return ImeInit::Unsupported(format!("dbus-ibus: {msg}"));
            }
            Err(ProbeErr::Transient(msg)) => {
                ime_log!(
                    "[waylandcraft][host_ime][dbus-ibus] TRANSIENT: {msg}"
                );
                return ImeInit::Transient(format!("dbus-ibus: {msg}"));
            }
        }

        // 真建一次 InputContext：这一步会暴露接口/版本级不兼容。
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        spawn_thread("wc-ibus-init", move || {
            let _ = done_tx.send(connect_input_context());
        });
        let ic_conns = match done_rx.recv_timeout(Duration::from_secs(6)) {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                let cls = classify_init_error(&e);
                ime_log!(
                    "[waylandcraft][host_ime][dbus-ibus] init failed ({cls}): {e}"
                );
                return match cls {
                    "UNSUPPORTED" => {
                        ImeInit::Unsupported(format!("dbus-ibus: {e}"))
                    }
                    _ => ImeInit::Transient(format!("dbus-ibus: {e}")),
                };
            }
            Err(_) => {
                let msg = "init timeout(6s)".to_string();
                ime_log!(
                    "[waylandcraft][host_ime][dbus-ibus] TRANSIENT: {msg}"
                );
                return ImeInit::Transient(format!("dbus-ibus: {msg}"));
            }
        };

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();

        // 命令线程：连接已就绪，直接串行处理 Focus/Cursor/Key + 信号订阅。
        spawn_thread("wc-ibus-cmd", {
            let ev_tx = ev_tx.clone();
            move || {
                if let Err(e) = command_loop(ic_conns, cmd_rx, ev_tx.clone()) {
                    let _ = ev_tx.send(FromWorker::Fatal(e));
                }
            }
        });

        ime_log!(
            "[waylandcraft][host_ime][dbus-ibus] input context READY (pre-connected)"
        );
        ImeInit::Ready(Box::new(Self {
            cmd_tx,
            ev_rx,
            events: Vec::new(),
            pending: VecDeque::new(),
            ime_consumed: HashSet::new(),
            release_waiting: VecDeque::new(),
            forwards: Vec::new(),
            ready: true,
            dead: None,
            want_enabled: false,
            focused: false,
            last_cursor: None,
            cursor_prefer_ti3: false,
        }))
    }

    /// 测试构造器：注入既有通道（不启动任何线程）。
    #[cfg(test)]
    fn from_parts(
        cmd_tx: Sender<ToWorker>,
        ev_rx: Receiver<FromWorker>,
    ) -> Self {
        Self {
            cmd_tx,
            ev_rx,
            events: Vec::new(),
            pending: VecDeque::new(),
            ime_consumed: HashSet::new(),
            release_waiting: VecDeque::new(),
            forwards: Vec::new(),
            ready: true,
            dead: None,
            want_enabled: false,
            focused: false,
            last_cursor: None,
            cursor_prefer_ti3: false,
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
        .inspect_err(|e| {
            eprintln!("[waylandcraft] thread {name} spawn failed: {e}")
        });
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
                    "session bus 上没有 {service}（需要 ibus-portal 进程在运行；名字无主）"
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
    let factory =
        Proxy::new(&conn, IBUS_SERVICE, IBUS_FACTORY_PATH, IBUS_FACTORY_IFACE)
            .map_err(|e| format!("factory proxy: {e}"))?;
    let ic_path: zbus::zvariant::OwnedObjectPath = factory
        .call::<_, _, zbus::zvariant::OwnedObjectPath>(
            "CreateInputContext",
            &("waylandcraft",),
        )
        .map_err(|e| format!("CreateInputContext: {e}"))?;
    // new_owned：得到 'static 的 Proxy（内部克隆连接），可安全移入信号线程。
    let ic: Proxy<'static> =
        Proxy::new_owned(conn.clone(), IBUS_SERVICE, ic_path, IBUS_IC_IFACE)
            .map_err(|e| format!("input context proxy: {e}"))?;
    ic.call::<_, _, ()>("SetCapabilities", &(IC_CAPABILITIES,))
        .map_err(|e| format!("SetCapabilities: {e}"))?;
    Ok(IcConnections { _conn: conn, ic })
}

/// 初始化错误分类：协议/接口级不兼容是确定性的 → UNSUPPORTED；
/// 总线/超时类 → TRANSIENT（允许重试）。
pub(crate) fn classify_init_error(e: &str) -> &'static str {
    if e.contains("UnknownMethod")
        || e.contains("UnknownObject")
        || e.contains("InterfaceNotFound")
        || e.contains("ServiceUnknown")
        || e.contains("NameHasNoOwner")
    {
        "UNSUPPORTED"
    } else {
        "TRANSIENT"
    }
}

fn command_loop(
    ic_conns: IcConnections,
    cmd_rx: Receiver<ToWorker>,
    ev_tx: Sender<FromWorker>,
) -> Result<(), String> {
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
                    let _ = ev_tx.send(FromWorker::Fatal(format!(
                        "signal {name} 流结束"
                    )));
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
            } => {
                // P0 可观测性：记录传给 ibus 的原始键（keysym 名 + press/release），
                // 用于定位「选字数字/空格被放行」时 ibus 实际收到什么键。
                let sym_name = xkbcommon::xkb::keysym_get_name(
                    xkbcommon::xkb::Keysym::new(keysym),
                );
                let action = if state & IBUS_RELEASE_MASK != 0 {
                    "release"
                } else {
                    "press"
                };
                ime_log!(
                    "[waylandcraft][host_ime][dbus-ibus] ProcessKeyEvent seq={seq} keysym={keysym:#x}({sym_name}) evdev={evdev} state={state:#x} action={action}"
                );
                ic_conns
                    .ic
                    .call::<_, _, bool>(
                        "ProcessKeyEvent",
                        &(keysym, evdev, state),
                    )
                    .map_err(|e| format!("ProcessKeyEvent: {e}"))
                    .and_then(|consumed| {
                        let _ =
                            ev_tx.send(FromWorker::KeyReply { seq, consumed });
                        Ok(())
                    })
            }
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

pub(crate) fn body_fields(
    msg: &zbus::message::Message,
) -> Result<StaticFields, String> {
    let body = msg.body();
    // 空 body（无参信号：HidePreeditText / HideLookupTable 等）→ 合法，返回空字段。
    // zbus 对 Unit 签名 body 的 Debug 显示为 `Unit`（空签名），反序列化 Structure
    // 会失败 —— v0.9.33 实测在此丢失全部 hide 事件，导致 preedit/候选状态残留。
    if body.signature() == &zbus::zvariant::Signature::Unit {
        return Ok(Vec::new());
    }
    let to_owned =
        |f: &zbus::zvariant::Value| -> Option<zbus::zvariant::Value<'static>> {
            zbus::zvariant::OwnedValue::try_from(f)
                .ok()
                .map(zbus::zvariant::Value::from)
        };
    // 单参数：整个消息体就是一个值。
    if let Ok(v) = body.deserialize::<zbus::zvariant::OwnedValue>() {
        return Ok(vec![zbus::zvariant::Value::from(v)]);
    }
    // 多参数：DBus 把参数列表编码为结构体 —— 必须按 Structure 解，
    // 不能用 Vec（元组签名不是序列）。（v0.9.31 在此丢失全部 preedit）
    if let Ok(st) = body.deserialize::<zbus::zvariant::Structure>() {
        let fields: Option<Vec<_>> = st.fields().iter().map(to_owned).collect();
        if let Some(fields) = fields {
            return Ok(fields);
        }
    }
    Err(format!("无法反序列化消息体（sig={:?}）", body.signature()))
}

/// 在字段（或变体内层结构体字段）里找第一个字符串 —— IBusText 的文本位。
/// ibus_serializable_serialize_object 会先写 GObject 类型名再写正文字段；
/// 这些名字永远不可能是用户文本（v0.9.31 曾把 "IBusText" 当正文提交）。
fn is_gobject_typename(s: &str) -> bool {
    matches!(
        s,
        "IBusText"
            | "IBusLookupTable"
            | "IBusAttrList"
            | "IBusEngineDesc"
            | "IBusComponent"
            | "IBusConfig"
            | "IBusObject"
            | "IBusSerializable"
            | "IBusInputContext"
            | "IBusObservedPath"
            | "IBusRegistryEntry"
    )
}

pub(crate) fn find_text(
    values: &[zbus::zvariant::Value<'static>],
) -> Option<String> {
    for v in values {
        match v {
            zbus::zvariant::Value::Str(s) => {
                let t = s.as_str();
                if !t.is_empty() && !is_gobject_typename(t) {
                    return Some(t.to_string());
                }
            }
            zbus::zvariant::Value::Value(inner) => {
                if let Some(t) = find_text(std::slice::from_ref(inner.as_ref()))
                {
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
pub(crate) fn find_int(
    values: &[zbus::zvariant::Value<'static>],
) -> Option<i64> {
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
                if let Some(n) = find_int(std::slice::from_ref(inner.as_ref()))
                {
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

/// 解包 variant 拿到内层值。
fn variant_inner<'a>(
    v: &'a zbus::zvariant::Value<'static>,
) -> Option<&'a zbus::zvariant::Value<'static>> {
    match v {
        zbus::zvariant::Value::Value(inner) => Some(inner.as_ref()),
        _ => None,
    }
}

/// 逐层剥 variant，直到拿到非 variant 值（zbus 不同构造/反序列化路径
/// 可能多包一层 variant：`Value::Array` 或 `Value::Value(Box(Array))`）。
fn peel_variant<'a>(
    v: &'a zbus::zvariant::Value<'static>,
) -> &'a zbus::zvariant::Value<'static> {
    let mut cur = v;
    while let zbus::zvariant::Value::Value(inner) = cur {
        cur = inner.as_ref();
    }
    cur
}

/// 在值上找数组（容错：可能被 variant 包裹）。
pub(crate) fn as_array<'a>(
    v: &'a zbus::zvariant::Value<'static>,
) -> Option<&'a zbus::zvariant::Array<'static>> {
    match peel_variant(v) {
        zbus::zvariant::Value::Array(arr) => Some(arr),
        _ => None,
    }
}

/// 在值上找结构体（容错：可能被 variant 包裹）。
pub(crate) fn as_structure<'a>(
    v: &'a zbus::zvariant::Value<'static>,
) -> Option<&'a zbus::zvariant::Structure<'static>> {
    match peel_variant(v) {
        zbus::zvariant::Value::Structure(st) => Some(st),
        _ => None,
    }
}

/// 取字段并剥 variant（zbus 5 的 `Value::new` 对 Value 输入会包 variant；
/// serde 反序列化路径不包。两种都兼容）。
pub(crate) fn field_peeled<'a>(
    fields: &'a [zbus::zvariant::Value<'static>],
    idx: usize,
) -> Option<&'a zbus::zvariant::Value<'static>> {
    fields.get(idx).map(peel_variant)
}

/// IBusLookupTable 的候选/标签元素：`av` 数组里每个元素是
/// variant 包一个 IBusText（`sa{sv}sv`：类型名 + attachments + 正文 + attrs）。
fn lookup_element_text(v: &zbus::zvariant::Value<'static>) -> Option<String> {
    let st = as_structure(v)?;
    let f = st.fields();
    // f[0]=Str(类型名) f[1]=Dict f[2]=Str(正文) —— 结构化解，不漫游（避免抓到 labels/preedit）
    if let Some(zbus::zvariant::Value::Str(s)) = field_peeled(f, 2) {
        if !s.as_str().is_empty() && !is_gobject_typename(s.as_str()) {
            return Some(s.as_str().to_string());
        }
    }
    None
}

/// 从 `UpdateLookupTable (v b)` 消息体里结构化解出候选表。
///
/// IBusLookupTable 序列化 = `( s  a{sv}  u  u  b  b  i  av  av )`：
/// 类型名 / attachments / page_size / cursor_pos / cursor_visible / round /
/// orientation / candidates / labels。每个候选是 variant 包 IBusText。
fn parse_lookup_table(fields: &StaticFields) -> Result<HostEvent, String> {
    let visible = match fields.get(1) {
        Some(zbus::zvariant::Value::Bool(b)) => *b,
        _ => false,
    };
    let table = fields
        .get(0)
        .ok_or_else(|| "UpdateLookupTable 缺表字段".to_string())?;
    let st = as_structure(table)
        .ok_or_else(|| "UpdateLookupTable 表不是结构体".to_string())?;
    let f = st.fields();
    let page_size = match field_peeled(f, 2) {
        Some(zbus::zvariant::Value::U32(n)) => *n,
        _ => 0,
    };
    let cursor_pos = match field_peeled(f, 3) {
        Some(zbus::zvariant::Value::U32(n)) => *n,
        _ => 0,
    };
    let cursor_visible = match field_peeled(f, 4) {
        Some(zbus::zvariant::Value::Bool(b)) => *b,
        _ => false,
    };
    let orientation = match field_peeled(f, 6) {
        Some(zbus::zvariant::Value::I32(n)) => (*n).max(0) as u32,
        _ => 0,
    };

    let mut candidates = Vec::new();
    let mut labels = Vec::new();
    if let Some(arr) = as_array(field_peeled(f, 7).ok_or("缺候选字段")?) {
        for item in arr.iter() {
            if let Some(t) = lookup_element_text(item) {
                candidates.push(t);
            }
        }
    }
    if let Some(arr) = as_array(field_peeled(f, 8).ok_or("缺标签字段")?) {
        for item in arr.iter() {
            if let Some(t) = lookup_element_text(item) {
                labels.push(t);
            }
        }
    }

    Ok(HostEvent::LookupTable {
        candidates,
        labels,
        // 归一化：cursor_pos 统一为【当前页内】下标（ibus 给的是全表绝对下标）。
        cursor_pos: if page_size > 0 {
            cursor_pos % page_size
        } else {
            0
        },
        cursor_visible,
        page_size,
        orientation,
        visible,
    })
}

pub(crate) fn find_bool(
    values: &[zbus::zvariant::Value<'static>],
) -> Option<bool> {
    for v in values {
        match v {
            zbus::zvariant::Value::Bool(b) => return Some(*b),
            zbus::zvariant::Value::Value(inner) => {
                if let Some(b) = find_bool(std::slice::from_ref(inner.as_ref()))
                {
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
            let cursor =
                find_int(&fields).unwrap_or(text.chars().count() as i64) as i32;
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
                push_with_done(
                    ev_tx,
                    HostEvent::PreeditString(String::new(), 0, 0),
                );
            }
        }
        "DeleteSurroundingText" => {
            let before = find_int(&fields).unwrap_or(0) as u32;
            push_with_done(ev_tx, HostEvent::DeleteSurroundingText(before, 0));
        }
        "HidePreeditText" => {
            push_with_done(
                ev_tx,
                HostEvent::PreeditString(String::new(), 0, 0),
            );
        }
        "UpdateLookupTable" => match parse_lookup_table(&fields) {
            Ok(ev) => {
                if let HostEvent::LookupTable {
                    candidates,
                    visible,
                    cursor_pos,
                    ..
                } = &ev
                {
                    ime_log!(
                        "[waylandcraft][host_ime][dbus-ibus] lookup {} visible={} cursor={}",
                        candidates.len(),
                        visible,
                        cursor_pos
                    );
                }
                push_with_done(ev_tx, ev);
            }
            Err(e) => {
                ime_log!(
                    "[waylandcraft][host_ime][dbus-ibus] LookupTable 解析失败: {e}"
                );
            }
        },
        "ShowLookupTable" => {
            push_with_done(
                ev_tx,
                HostEvent::LookupTable {
                    candidates: Vec::new(),
                    labels: Vec::new(),
                    cursor_pos: 0,
                    cursor_visible: true,
                    page_size: 0,
                    orientation: 0,
                    visible: true,
                },
            );
        }
        "HideLookupTable" => {
            push_with_done(
                ev_tx,
                HostEvent::LookupTable {
                    candidates: Vec::new(),
                    labels: Vec::new(),
                    cursor_pos: 0,
                    cursor_visible: false,
                    page_size: 0,
                    orientation: 0,
                    visible: false,
                },
            );
        }
        "ForwardKeyEvent" => {
            // (keyval, keycode(evdev), state)
            let nums: Vec<i64> = fields
                .iter()
                .filter_map(|v| find_int(std::slice::from_ref(v)))
                .collect();
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
        // P2：光标源随 app_active 切换——WaylandCraft 世界内激活（ti3 会话）→
        // ti3 光标优先；回到 MC 原生 UI → 恢复 Java CursorRectReporter 上报。
        self.cursor_prefer_ti3 = active;
        ime_log!(
            "[waylandcraft][host_ime][dbus-ibus] set_active -> {active} (cursor_source={})",
            if active { "ti3" } else { "java" }
        );
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
                    // P2：ti3 会话激活 → 光标源切到 ti3（firefox 等世界内窗口真实光标）。
                    // 判据：WaylandCraft 世界内有 ti3 会话启用（app_active=true）时，
                    // st.cursor_rect 是 firefox 等窗口的实时光标；MC 原生 UI 无 ti3 会话，
                    // 由 Java CursorRectReporter 的 update_cursor_rect 兜底。
                    self.cursor_prefer_ti3 = true;
                    if !self.focused {
                        let _ = self.cmd_tx.send(ToWorker::FocusIn);
                        self.focused = true;
                    }
                    if let Some(rect) = st.cursor_rect {
                        self.set_cursor_from_ti3(rect);
                    }
                }
                ImeCommand::PushState(st) => {
                    // P2：仅 ti3 光标优先态下用 PushState 携带的光标刷新候选窗锚点；
                    // 非 ti3 态（MC 原生）不在此更新，等 Java update_cursor_rect。
                    if self.cursor_prefer_ti3 {
                        if let Some(rect) = st.cursor_rect {
                            self.set_cursor_from_ti3(rect);
                        }
                    }
                }
                ImeCommand::Deactivate => {
                    self.want_enabled = false;
                    self.cursor_prefer_ti3 = false;
                    if self.focused {
                        let _ = self.cmd_tx.send(ToWorker::FocusOut);
                        self.focused = false;
                    }
                }
            }
        }
    }

    fn candidate_nav(&mut self, nav: crate::host_ime::CandidateNav) {
        // ibus portal 接口没有 SelectCandidate/PrevPage/NextPage 方法
        //（调研确认），候选翻页/选字只能靠按键 → ProcessKeyEvent 通路。
        ime_log!(
            "[waylandcraft][host_ime][dbus-ibus] candidate_nav {nav:?} 忽略（ibus portal 无候选方法）"
        );
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
                            let p = self
                                .pending
                                .pop_front()
                                .expect("front checked");
                            if !consumed {
                                ime_log!(
                                    "[waylandcraft][host_ime][dbus-ibus] key seq={seq} NOT consumed -> 放行注入（app 会收到该键）"
                                );
                                self.forwards.push(ForwardedKey {
                                    key: p.key,
                                    action: p.action,
                                });
                                // P1：press 放行 → 挂起的 release 一并补投递（否则应用
                                // 只收到 press 没有 release，键会卡住）。
                                if let Some(pos) = self
                                    .release_waiting
                                    .iter()
                                    .position(|k| *k == p.key)
                                {
                                    self.release_waiting.remove(pos);
                                    self.forwards.push(ForwardedKey {
                                        key: p.key,
                                        action: KeyboardAction::Release,
                                    });
                                    ime_log!(
                                        "[waylandcraft][host_ime][dbus-ibus] release key={} 挂起配对 press(放行) -> 补投递",
                                        p.key
                                    );
                                }
                            } else {
                                ime_log!(
                                    "[waylandcraft][host_ime][dbus-ibus] key seq={seq} consumed by IME"
                                );
                                // P1：press 被 IME 消费 → 其 release 配对吃掉（挂起的
                                // release 丢弃，不再提交 ibus 也就不可能被放行注入）。
                                self.ime_consumed.insert(p.key);
                                if let Some(pos) = self
                                    .release_waiting
                                    .iter()
                                    .position(|k| *k == p.key)
                                {
                                    self.release_waiting.remove(pos);
                                    ime_log!(
                                        "[waylandcraft][host_ime][dbus-ibus] release key={} 挂起配对 press(consumed) -> 丢弃",
                                        p.key
                                    );
                                }
                            }
                        }
                        other => {
                            ime_log!(
                                "[waylandcraft][host_ime][dbus-ibus] KeyReply seq={seq} 错位（队首 {:?}）-> 重同步丢弃",
                                other.map(|p| p.seq)
                            );
                            // 错位兜底：该 seq 对应的 press 裁决丢失 → 挂起的 release
                            // 补投递（避免 press 已注入应用而 release 永远等不到）。
                            let lost_key = self
                                .pending
                                .iter()
                                .find(|p| p.seq == seq)
                                .map(|p| p.key);
                            self.pending.retain(|p| p.seq != seq);
                            if let Some(key) = lost_key
                                && let Some(pos) = self
                                    .release_waiting
                                    .iter()
                                    .position(|k| *k == key)
                            {
                                self.release_waiting.remove(pos);
                                self.forwards.push(ForwardedKey {
                                    key,
                                    action: KeyboardAction::Release,
                                });
                                ime_log!(
                                    "[waylandcraft][host_ime][dbus-ibus] KeyReply 错位 seq={seq} key={key} -> release 补投递（避免卡键）"
                                );
                            }
                        }
                    }
                }
                Ok(FromWorker::Ev(e)) => self.events.push(e),
                Ok(FromWorker::Forward(f)) => self.forwards.push(f),
                Ok(FromWorker::Fatal(msg)) => {
                    ime_log!(
                        "[waylandcraft][host_ime][dbus-ibus] FATAL: {msg}"
                    );
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
        // P1：release 跟随 press 裁决。
        if sk.action == KeyboardAction::Release {
            if self.ime_consumed.contains(&sk.key) {
                // press 已被 IME 消费 → release 直接配对吃掉（不提交 ibus、
                // 不转发；preedit 清空后 release 不可能再被 ibus 放行注入）。
                self.ime_consumed.remove(&sk.key);
                ime_log!(
                    "[waylandcraft][host_ime][dbus-ibus] release key={} 配对 press(consumed) -> 丢弃",
                    sk.key
                );
                return true;
            }
            if self
                .pending
                .iter()
                .any(|p| p.key == sk.key && p.action != KeyboardAction::Release)
            {
                // 该键 press 仍在裁决中（KeyReply 未回）→ release 挂起，
                // 等 press 裁决：consumed → 丢弃；NOT consumed → 补投递。
                self.release_waiting.push_back(sk.key);
                ime_log!(
                    "[waylandcraft][host_ime][dbus-ibus] release key={} press 未裁决 -> 挂起等待",
                    sk.key
                );
                return true;
            }
            // press 已裁决放行（或没有未决 press）→ release 照常提交裁决。
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
        // P2：ti3 优先态下 Java 上报不覆盖（firefox 等世界内窗口场景由
        // st.cursor_rect 驱动）；非 ti3 态（MC 原生 UI）只信 Java。
        if self.cursor_prefer_ti3 {
            ime_log!(
                "[waylandcraft][host_ime][dbus-ibus] Java update_cursor_rect {:?} 忽略（ti3 光标优先）",
                rect
            );
            return;
        }
        if self.last_cursor == Some(rect) {
            return;
        }
        self.last_cursor = Some(rect);
        let (x, y, w, h) = rect;
        ime_log!(
            "[waylandcraft][host_ime][dbus-ibus] SetCursorLocationRelative (java) ({x},{y},{w},{h})"
        );
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

        ev_tx
            .send(FromWorker::KeyReply {
                seq: 1,
                consumed: false,
            })
            .unwrap();
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
        ev_tx
            .send(FromWorker::KeyReply {
                seq: 7,
                consumed: true,
            })
            .unwrap();
        be.poll();
        assert!(be.pending.is_empty());
        assert!(be.take_forwarded_keys().is_empty());
    }

    /// P1：press 被 IME 消费 → 后续 release 直接配对吃掉（不提交、不转发），
    /// 杜绝 preedit 清空后 release 被 ibus 放行注入应用（R1 场景）。
    #[test]
    fn release_pairs_with_consumed_press() {
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusIbusBackend::from_parts(cmd_tx, ev_rx);

        // press（数字选字键 1，scancode=10）：ibus 消费。
        assert!(be.submit_key(SubmittedKey {
            seq: 1,
            key: 10,
            keysym: 0x31,
            evdev: 2,
            state: 0,
            action: KeyboardAction::Press,
            mods: (0, 0, 0, 0),
        }));
        ev_tx
            .send(FromWorker::KeyReply {
                seq: 1,
                consumed: true,
            })
            .unwrap();
        be.poll();
        assert!(be.take_forwarded_keys().is_empty(), "consumed press 不放行");

        // release 到达：必须被吃掉——不产生 pending、不转发、不提交 ibus。
        assert!(be.submit_key(SubmittedKey {
            seq: 2,
            key: 10,
            keysym: 0x31,
            evdev: 2,
            state: 1 << 30,
            action: KeyboardAction::Release,
            mods: (0, 0, 0, 0),
        }));
        be.poll();
        assert!(be.pending.is_empty(), "release 不应进 pending");
        assert!(be.take_forwarded_keys().is_empty(), "release 不应转发");
        assert!(!be.ime_consumed.contains(&10), "配对后消费集合应清空该键");
    }

    /// P1：press 未裁决时 release 挂起；press 裁决 consumed → release 丢弃。
    #[test]
    fn release_waits_then_consumed() {
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
        // press 的 KeyReply 未到，release 先到 → 挂起等待。
        assert!(be.submit_key(SubmittedKey {
            seq: 2,
            key: 38,
            keysym: 'a' as u32,
            evdev: 30,
            state: 1 << 30,
            action: KeyboardAction::Release,
            mods: (0, 0, 0, 0),
        }));
        assert_eq!(be.release_waiting.len(), 1, "release 应挂起");
        assert_eq!(be.pending.len(), 1, "只有 press 进 pending");

        // press 裁决 consumed → release 丢弃。
        ev_tx
            .send(FromWorker::KeyReply {
                seq: 1,
                consumed: true,
            })
            .unwrap();
        be.poll();
        assert!(be.release_waiting.is_empty());
        assert!(be.take_forwarded_keys().is_empty(), "consumed 路径零转发");
    }

    /// P1：press 未裁决时 release 挂起；press 裁决 NOT consumed → release 补投递
    /// （应用收到 press + release 一对，键不会卡住）。
    #[test]
    fn release_waits_then_forwarded() {
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusIbusBackend::from_parts(cmd_tx, ev_rx);

        be.submit_key(SubmittedKey {
            seq: 1,
            key: 38,
            keysym: 'a' as u32,
            evdev: 30,
            state: 0,
            action: KeyboardAction::Press,
            mods: (0, 0, 0, 0),
        });
        be.submit_key(SubmittedKey {
            seq: 2,
            key: 38,
            keysym: 'a' as u32,
            evdev: 30,
            state: 1 << 30,
            action: KeyboardAction::Release,
            mods: (0, 0, 0, 0),
        });
        assert_eq!(be.release_waiting.len(), 1);

        // press 裁决 NOT consumed → press 放行 + 挂起 release 补投递。
        ev_tx
            .send(FromWorker::KeyReply {
                seq: 1,
                consumed: false,
            })
            .unwrap();
        be.poll();
        assert!(be.release_waiting.is_empty());
        let fwd = be.take_forwarded_keys();
        assert_eq!(fwd.len(), 2, "press+release 应成对转发");
        assert_eq!(fwd[0].action, KeyboardAction::Press);
        assert_eq!(fwd[1].action, KeyboardAction::Release);
    }

    /// P1：press 已裁决放行（ime_consumed 无此键）→ release 照常提交裁决。
    #[test]
    fn release_submitted_after_press_forwarded() {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusIbusBackend::from_parts(cmd_tx, ev_rx);

        be.submit_key(SubmittedKey {
            seq: 1,
            key: 38,
            keysym: 'a' as u32,
            evdev: 30,
            state: 0,
            action: KeyboardAction::Press,
            mods: (0, 0, 0, 0),
        });
        ev_tx
            .send(FromWorker::KeyReply {
                seq: 1,
                consumed: false,
            })
            .unwrap();
        be.poll();
        assert!(be.take_forwarded_keys().len() == 1, "press 已放行");
        // 清掉 press 的 Key 命令（seq=1）。
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(ToWorker::Key { seq: 1, .. })
        ));

        // release 现在到达：press 已裁决放行 → release 正常提交给 ibus。
        assert!(be.submit_key(SubmittedKey {
            seq: 3,
            key: 38,
            keysym: 'a' as u32,
            evdev: 30,
            state: 1 << 30,
            action: KeyboardAction::Release,
            mods: (0, 0, 0, 0),
        }));
        assert_eq!(be.pending.len(), 1, "release 应提交裁决");
        // 工作线程侧确实收到 Key 命令。
        match cmd_rx.try_recv() {
            Ok(ToWorker::Key { seq, .. }) => assert_eq!(seq, 3),
            _ => panic!("expected ToWorker::Key seq=3"),
        }
        ev_tx
            .send(FromWorker::KeyReply {
                seq: 3,
                consumed: false,
            })
            .unwrap();
        be.poll();
        let fwd = be.take_forwarded_keys();
        assert_eq!(fwd.len(), 1);
        assert_eq!(fwd[0].action, KeyboardAction::Release);
    }

    /// P2：ti3 会话激活（app_active）→ ti3 光标优先；Java 上报被忽略。
    /// 非 ti3 态（MC 原生）→ Java 上报生效（候选窗锚点跟随 MC 焦点框）。
    #[test]
    fn cursor_source_prefers_ti3_when_active() {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (_ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();
        let mut be = DbusIbusBackend::from_parts(cmd_tx, ev_rx);

        // app_active=true（用户在 WaylandCraft 世界内 firefox 打字）。
        be.set_active(true);

        // Activate 携带 firefox 上报的 ti3 光标 → 先 FocusIn 再 SetCursorLocationRelative。
        let st = crate::ime::AppState::from_pending(
            None,
            0,
            0,
            0,
            0,
            0,
            Some((324, 54, 0, 20)),
        );
        be.execute_commands(vec![ImeCommand::Activate(st)]);
        assert!(matches!(cmd_rx.try_recv(), Ok(ToWorker::FocusIn)));
        match cmd_rx.try_recv() {
            Ok(ToWorker::SetCursorRect(324, 54, 0, 20)) => {}
            _ => panic!("expected SetCursorRect(324,54,0,20) from ti3"),
        }

        // ti3 优先态下 Java 上报被忽略（不产生新的 SetCursorRect 命令）。
        be.update_cursor_rect((96, 244, 2, 9));
        assert!(
            matches!(cmd_rx.try_recv(), Err(_)),
            "ti3 优先时 Java 上报不应发 SetCursorRect"
        );

        // app_active=false（回到 MC 原生 UI）→ Java 上报恢复生效。
        be.set_active(false);
        // set_active(false) 会先发 FocusOut。
        assert!(matches!(cmd_rx.try_recv(), Ok(ToWorker::FocusOut)));
        be.update_cursor_rect((96, 244, 2, 9));
        match cmd_rx.try_recv() {
            Ok(ToWorker::SetCursorRect(96, 244, 2, 9)) => {}
            _ => panic!("expected SetCursorRect(96,244,2,9) from java"),
        }
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
        ev_tx
            .send(FromWorker::KeyReply {
                seq: 2,
                consumed: false,
            })
            .unwrap();
        be.poll();
        assert!(be.pending.iter().all(|p| p.seq != 2), "错位 seq 应被移除");

        // 正常路径：1、3 依次放行。
        ev_tx
            .send(FromWorker::KeyReply {
                seq: 1,
                consumed: false,
            })
            .unwrap();
        ev_tx
            .send(FromWorker::KeyReply {
                seq: 3,
                consumed: false,
            })
            .unwrap();
        be.poll();
        let fwd = be.take_forwarded_keys();
        assert_eq!(fwd.len(), 2);

        // CommitText → CommitString + Done 成对出现。
        push_with_done(&ev_tx, HostEvent::CommitString("你好".into()));
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

    /// UpdateLookupTable 结构化解：IBusLookupTable `(sa{sv}uubbiavav)` +
    /// 候选 IBusText `(sa{sv}sv)`，跳 GObject 类型名，labels/光标/分页全对。
    #[test]
    fn parse_lookup_table_structured() {
        use std::collections::HashMap;
        use zbus::zvariant::{Array, Structure, Value};

        // attachments 空字典 sa{sv}
        let empty_dict =
            || zbus::zvariant::Dict::from(HashMap::<String, Value>::new());
        // IBusText = (Str(类型名), Dict, Str(正文), Variant(attrs))
        let mk_cand = |text: &'static str| {
            Value::Value(Box::new(Value::Structure(Structure::from((
                Value::Str("IBusText".into()),
                Value::Dict(empty_dict()),
                Value::Str(text.into()),
                Value::Value(Box::new(Value::Bool(false))),
            )))))
        };
        let candidates =
            Array::from(vec![mk_cand("你"), mk_cand("泥"), mk_cand("逆")]);
        let labels =
            Array::from(vec![mk_cand("1."), mk_cand("2."), mk_cand("3.")]);

        let table =
            Value::Value(Box::new(Value::Structure(Structure::from((
                Value::Str("IBusLookupTable".into()),
                Value::Dict(empty_dict()),
                Value::U32(3),      // page_size
                Value::U32(1),      // cursor_pos（全表绝对下标）
                Value::Bool(true),  // cursor_visible
                Value::Bool(false), // round
                Value::I32(1),      // orientation=垂直
                Value::Array(candidates),
                Value::Array(labels),
            )))));

        let fields: StaticFields = vec![table, Value::Bool(true)]; // (v b)
        let ev = parse_lookup_table(&fields).expect("parse");
        match ev {
            HostEvent::LookupTable {
                candidates,
                labels,
                cursor_pos,
                cursor_visible,
                page_size,
                orientation,
                visible,
            } => {
                assert_eq!(candidates, vec!["你", "泥", "逆"]);
                assert_eq!(labels, vec!["1.", "2.", "3."]);
                assert_eq!(cursor_pos, 1);
                assert!(cursor_visible);
                assert_eq!(page_size, 3);
                assert_eq!(orientation, 1);
                assert!(visible);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// 空表 + visible=false ≡ 隐藏；解析后 candidates 为空、visible=false。
    #[test]
    fn parse_lookup_table_hidden() {
        use std::collections::HashMap;
        use zbus::zvariant::{Array, Structure, Value};

        let empty_dict =
            || zbus::zvariant::Dict::from(HashMap::<String, Value>::new());
        let table =
            Value::Value(Box::new(Value::Structure(Structure::from((
                Value::Str("IBusLookupTable".into()),
                Value::Dict(empty_dict()),
                Value::U32(9),
                Value::U32(0),
                Value::Bool(false),
                Value::Bool(false),
                Value::I32(0),
                Value::Array(Array::from(Vec::<Value>::new())),
                Value::Array(Array::from(Vec::<Value>::new())),
            )))));

        let fields: StaticFields = vec![table, Value::Bool(false)];
        let ev = parse_lookup_table(&fields).expect("parse");
        match ev {
            HostEvent::LookupTable {
                candidates,
                visible,
                ..
            } => {
                assert!(candidates.is_empty());
                assert!(!visible);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
