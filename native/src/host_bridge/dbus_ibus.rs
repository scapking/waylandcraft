//! dbus-ibus 桥接（C 方案 Layer 3 第一个后端）。
//!
//! ## 协议映射
//!
//! ```text
//! ImeEvent::DownEvent::Key(keycode, action, mods)
//!     → dbus-ibus: ProcessKeyEvent(keysym, evdev, state)  (fire-and-forget)
//! ImeEvent::DownEvent::Surrounding(text, cursor, anchor)
//!     → dbus-ibus: SetSurroundingText(text, cursor_pos, anchor_pos)
//! ImeEvent::DownEvent::CursorRect(x, y, w, h)
//!     → dbus-ibus: SetCursorLocationRelative(x, y, w, h)
//! ImeEvent::DownEvent::State(FocusChange::Activate|Deactivate)
//!     → dbus-ibus: FocusIn / FocusOut
//!
//! dbus-ibus 信号:
//!     CommitText(s)            → UpEvent::Commit(Commit { text: s })
//!     UpdatePreeditText(s, c, v) → UpEvent::PreeditUpdate(...)
//!     HidePreeditText          → UpEvent::PreeditUpdate(PreeditUpdate::clear())
//!     UpdateLookupTable         → UpEvent::LookupTable(...)
//!     HideLookupTable           → UpEvent::LookupTable(visible: false, ...)
//!     ShowLookupTable           → UpEvent::LookupTable(visible: true, ...)
//!     DeleteSurroundingText(b, a) → UpEvent::DeleteSurrounding(...)
//!     任意信号                   → UpEvent::Done(batch_id)
//! ```
//!
//! ## commit 驱动模式（关键决策）
//!
//! **不**等 ProcessKeyEvent reply——v0.9.39 走 hybrid async 等 reply 100% 超时。
//! 改成 fire-and-forget：press 立即发 ProcessKeyEvent，**不**等 consumed。
//! commit 文本由宿主 daemon 异步发回——这才是"按键命运"的真正决定者。
//!
//! 后果：
//! - 应用永远不被 mod 拦截按键（保持 firefox GdkIMContext 独立工作）
//! - preedit/commit 由宿主 daemon 通过 dbus 信号回 → mod 翻译 → 应用 text-input
//! - firefox 文本框里**同时**有字母（firefox GdkIMContext 自己画的）
//!   和 commit 汉字（mod 通过 ti3 推的）——这不是 bug，是双客户端共存的真实表现
//!
//! 用户**接受**这个语义（C 方案架构决策）：mod 不"接管"键盘，只"转发"
//! 按键 + "翻译"信号。

use super::{ime_log, BridgeInit, HostBridge};
use crate::ime::{
    Commit, CursorRect, DeleteSurrounding, DownEvent, KeyEvent, LookupTable, PreeditUpdate,
    SurroundingText, UpEvent,
};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError};
use std::time::Duration;

// ibus-portal 入口（事实 1：session bus 上同名 `org.freedesktop.IBus` 服务
// 不实现 IBus 接口 —— 真正的入口是 `org.freedesktop.portal.IBus` portal 服务）。
// 这正是 flatpak 应用在 GNOME 下打中文的同一条路。
// 参考：8/26 笔记 waylandcraft-ime-fix.md 「v0.9.31 真根因」节。
const IBUS_SERVICE: &str = "org.freedesktop.portal.IBus";
const IBUS_FACTORY_PATH: &str = "/org/freedesktop/IBus";
// ibus-portal 暴露的是 `org.freedesktop.IBus.Portal` 接口
// （不是旧的 `org.freedesktop.IBus.Factory` —— 旧接口只在 ibus 私有总线上）。
const IBUS_FACTORY_IFACE: &str = "org.freedesktop.IBus.Portal";
const IBUS_IC_IFACE: &str = "org.freedesktop.IBus.InputContext";

/// 客户端能力（声明完整支持）。
const IC_CAPABILITIES: u32 = 0x3F;

/// dbus 端进程内部通道。
#[derive(Debug)]
pub(crate) enum ToWorker {
    ProcessKey {
        keysym: u32,
        evdev: u32,
        state: u32,
    },
    SetSurroundingText {
        text: String,
        cursor_pos: u32,
        anchor_pos: u32,
    },
    SetCursorLocationRelative {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    FocusIn,
    FocusOut,
    /// 测试/诊断用的"信号收到"响应（不影响 commit 逻辑）。
    Ping,
}

pub(crate) enum FromWorker {
    Commit(String),
    Preedit {
        text: String,
        cursor_begin: i32,
        cursor_end: i32,
        /// true = 这是 HidePreeditText 信号（清空 preedit）
        clear: bool,
    },
    DeleteSurrounding {
        before: u32,
        after: u32,
    },
    LookupTable {
        candidates: Vec<String>,
        labels: Vec<String>,
        cursor_pos: u32,
        cursor_visible: bool,
        page_size: u32,
        orientation: u32,
        visible: bool,
    },
    /// 任意信号边界：mod 层用这个把同一批 commit/preedit/delete 一起发给 relay。
    /// batch_id 是诊断 ID（递增）。
    Done(u32),
    Fatal(String),
}

/// dbus-ibus 桥接实现。
pub struct DbusIbusBridge {
    /// 提交按键的同步通道（fire-and-forget：主线程不阻塞等 reply）。
    cmd_tx: Sender<ToWorker>,
    /// 上行事件出站通道。
    ev_rx: Receiver<FromWorker>,
    /// 递增 batch_id 计数器。
    next_batch: u32,
    /// 后端是否就绪。
    ready: bool,
    /// 后端是否已死。
    dead: Option<String>,
}

impl DbusIbusBridge {
    /// 探测 + 启动 worker 线程。
    ///
    /// 返回 Ready(Box) 表示就绪；Transient 表示环境问题可重试；Unsupported
    /// 表示协议级不兼容。
    pub fn connect() -> BridgeInit {
        ime_log!("[waylandcraft][host_bridge][dbus-ibus] probing...");

        // 1. 探测 session bus 上是否有 ibus-daemon
        let conn = match zbus::blocking::Connection::session() {
            Ok(c) => c,
            Err(e) => {
                return BridgeInit::Transient(format!("session bus: {e}"));
            }
        };
        // 2. 探测 IBUS_SERVICE 是否有主
        if let Err(e) = probe_service_owner(&conn, IBUS_SERVICE) {
            return e;
        }

        // 3. 建 InputContext
        let ic_conns = match connect_input_context(&conn) {
            Ok(c) => c,
            Err(e) => return classify_init_error(&e),
        };

        // 4. 启动 worker 线程（独占 ic_conns，处理按键 + 接收信号）
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();

        std::thread::Builder::new()
            .name("wc-host-bridge-ibus".into())
            .spawn(move || {
                command_loop(ic_conns, cmd_rx, ev_tx);
            })
            .expect("spawn worker thread");

        ime_log!("[waylandcraft][host_bridge][dbus-ibus] input context READY");
        BridgeInit::Ready(Box::new(Self {
            cmd_tx,
            ev_rx,
            next_batch: 0,
            ready: true,
            dead: None,
        }))
    }

    /// 测试构造器（不启动 worker，仅用于单元测试）。
    #[cfg(test)]
    pub(crate) fn from_channels(cmd_tx: Sender<ToWorker>, ev_rx: Receiver<FromWorker>) -> Self {
        Self {
            cmd_tx,
            ev_rx,
            next_batch: 0,
            ready: true,
            dead: None,
        }
    }
}

impl HostBridge for DbusIbusBridge {
    fn name(&self) -> &'static str {
        "dbus-ibus"
    }

    fn is_ready(&self) -> bool {
        self.ready && self.dead.is_none()
    }

    fn is_dead(&self) -> bool {
        self.dead.is_some()
    }

    fn submit(&mut self, ev: DownEvent) {
        if !self.is_ready() {
            return;
        }
        let cmd = match ev {
            DownEvent::State(crate::ime::FocusChange::Activate) => {
                Some(ToWorker::FocusIn)
            }
            DownEvent::State(crate::ime::FocusChange::Deactivate) => {
                Some(ToWorker::FocusOut)
            }
            DownEvent::Key(KeyEvent { keysym, keycode, action, mods: _ }) => {
                let evdev = keycode.saturating_sub(8);
                let state = match action {
                    crate::seat::KeyboardAction::Press => 0u32,
                    crate::seat::KeyboardAction::Release => 1u32 << 30,
                    crate::seat::KeyboardAction::Repeat => 0u32, // repeat 当 press 处理
                };
                // v0.10.2 修：使用 bridge::keyboard_input 通过 xkb 解码的 keysym
                // （不是 0，不是 evdev keycode——ibus 引擎按 keysym 决定处理）。
                // 之前 v0.10.1 之前传 keysym=0 导致 ibus 引擎不知道按了什么键——
                // 不发回 commit/preedit。这是 v0.9.40 笔记"调用方预解析"从未
                // 实现的根因。
                if keysym == 0 {
                    ime_log!("[waylandcraft][host_bridge][dbus-ibus] submit Key keysym=0 拒绝（防止引擎不识别）");
                    return; // 吞下：不要给 ibus 一个它无法识别的 keysym
                }
                Some(ToWorker::ProcessKey {
                    keysym,
                    evdev,
                    state,
                })
            }
            DownEvent::Surrounding(SurroundingText { text, cursor, anchor }) => {
                Some(ToWorker::SetSurroundingText {
                    text,
                    cursor_pos: cursor,
                    anchor_pos: anchor,
                })
            }
            DownEvent::CursorRect(CursorRect { x, y, w, h }) => {
                Some(ToWorker::SetCursorLocationRelative { x, y, w, h })
            }
        };
        if let Some(cmd) = cmd {
            if self.cmd_tx.send(cmd).is_err() {
                self.dead = Some("worker channel closed".into());
            }
        }
    }

    fn take_up_events(&mut self) -> Vec<UpEvent> {
        if !self.is_ready() {
            return Vec::new();
        }
        let mut out = Vec::new();
        loop {
            match self.ev_rx.try_recv() {
                Ok(FromWorker::Commit(text)) => {
                    out.push(UpEvent::Commit(Commit { text }));
                }
                Ok(FromWorker::Preedit {
                    text,
                    cursor_begin,
                    cursor_end,
                    clear,
                }) => {
                    if clear {
                        out.push(UpEvent::Preedit(PreeditUpdate::clear()));
                    } else {
                        out.push(UpEvent::Preedit(PreeditUpdate::set(
                            text,
                            cursor_begin,
                            cursor_end,
                        )));
                    }
                }
                Ok(FromWorker::DeleteSurrounding { before, after }) => {
                    out.push(UpEvent::DeleteSurrounding(DeleteSurrounding {
                        before_length: before,
                        after_length: after,
                    }));
                }
                Ok(FromWorker::LookupTable {
                    candidates,
                    labels,
                    cursor_pos,
                    cursor_visible,
                    page_size,
                    orientation,
                    visible,
                }) => {
                    out.push(UpEvent::LookupTable(LookupTable {
                        candidates,
                        labels,
                        cursor_pos,
                        cursor_visible,
                        page_size,
                        orientation,
                        visible,
                    }));
                }
                Ok(FromWorker::Done(_)) => {
                    self.next_batch = self.next_batch.wrapping_add(1);
                    out.push(UpEvent::Done(crate::ime::Done {
                        batch_id: self.next_batch,
                    }));
                }
                Ok(FromWorker::Fatal(msg)) => {
                    ime_log!("[waylandcraft][host_bridge][dbus-ibus] FATAL: {msg}");
                    self.dead = Some(msg);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.dead = Some("worker channel closed".into());
                    break;
                }
            }
        }
        out
    }

    fn update_cursor_rect(&mut self, rect: CursorRect) {
        if !self.is_ready() {
            return;
        }
        let _ = self.cmd_tx.send(ToWorker::SetCursorLocationRelative {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
        });
    }
}

/// Worker 线程：独占 dbus 连接 + ic_conns。
fn command_loop(
    ic_conns: IcConnections,
    cmd_rx: Receiver<ToWorker>,
    ev_tx: Sender<FromWorker>,
) {
    use zbus::blocking::Proxy;

    // 启动信号订阅线程
    for sig in WATCHED_SIGNALS {
        let ic = ic_conns.ic.clone();
        let ev_tx = ev_tx.clone();
        let name = (*sig).to_string();
        std::thread::Builder::new()
            .name(format!("wc-host-bridge-ibus-sig-{}", sig))
            .spawn(move || {
                if let Ok(iter) = ic.receive_signal(name.as_str()) {
                    for msg in iter {
                        let _ = handle_signal(&name, &msg, &ev_tx);
                    }
                }
            })
            .expect("spawn signal thread");
    }

    // 命令处理循环
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
            ToWorker::SetCursorLocationRelative { x, y, w, h } => ic_conns
                .ic
                .call::<_, _, ()>("SetCursorLocationRelative", &(x, y, w, h))
                .map(|_| ())
                .map_err(|e| e.to_string()),
            ToWorker::SetSurroundingText {
                text,
                cursor_pos,
                anchor_pos,
            } => ic_conns
                .ic
                .call::<_, _, ()>(
                    "SetSurroundingText",
                    &(text, cursor_pos, anchor_pos),
                )
                .map(|_| ())
                .map_err(|e| e.to_string()),
            ToWorker::ProcessKey { keysym, evdev, state } => {
                // 同步调 ProcessKeyEvent（不等 reply——commit 驱动模式）
                // v0.11.0 修：之前 `let _ = ...` 静默丢弃 zbus 错误——
                // 49 次 submit / 0 ProcessKeyEvent 日志就是这 bug。
                // 现在显式记录 zbus 调用结果（成功 + consumed、失败）。
                let call_result = ic_conns
                    .ic
                    .call::<_, _, bool>("ProcessKeyEvent", &(keysym, evdev, state));
                match call_result {
                    Ok(consumed) => {
                        ime_log!(
                            "[waylandcraft][host_bridge][dbus-ibus] ProcessKeyEvent keysym={keysym:#x} evdev={evdev} state={state:#x} -> consumed={consumed}"
                        );
                    }
                    Err(e) => {
                        ime_log!(
                            "[waylandcraft][host_bridge][dbus-ibus] ProcessKeyEvent 失败 keysym={keysym:#x} evdev={evdev} state={state:#x}: {e}"
                        );
                    }
                }
                Ok(())
            }
            ToWorker::Ping => Ok(()),
        };
        if let Err(e) = res {
            ime_log!("[waylandcraft][host_bridge][dbus-ibus] 命令失败: {e}");
            let _ = ev_tx.send(FromWorker::Fatal(e));
            return;
        }
    }
}

const WATCHED_SIGNALS: &[&str] = &[
    "CommitText",
    "UpdatePreeditText",
    "UpdatePreeditTextWithMode",
    "HidePreeditText",
    "UpdateLookupTable",
    "ShowLookupTable",
    "HideLookupTable",
    "DeleteSurroundingText",
    "ForwardKeyEvent",
    "RequireSurroundingText",
    "UpdateProperty",
    "RegisterProperties",
    "Enabled",
    "Disabled",
];

struct IcConnections {
    _conn: zbus::blocking::Connection,
    ic: zbus::blocking::Proxy<'static>,
}

fn connect_input_context(conn: &zbus::blocking::Connection) -> Result<IcConnections, String> {
    use zbus::blocking::Proxy;
    let factory = Proxy::new(conn, IBUS_SERVICE, IBUS_FACTORY_PATH, IBUS_FACTORY_IFACE)
        .map_err(|e| format!("factory proxy: {e}"))?;
    let ic_path: zbus::zvariant::OwnedObjectPath = factory
        .call::<_, _, zbus::zvariant::OwnedObjectPath>("CreateInputContext", &("waylandcraft",))
        .map_err(|e| format!("CreateInputContext: {e}"))?;
    let ic: Proxy<'static> =
        Proxy::new_owned(conn.clone(), IBUS_SERVICE, ic_path, IBUS_IC_IFACE)
            .map_err(|e| format!("input context proxy: {e}"))?;
    ic.call::<_, _, ()>("SetCapabilities", &(IC_CAPABILITIES,))
        .map_err(|e| format!("SetCapabilities: {e}"))?;
    Ok(IcConnections { _conn: conn.clone(), ic })
}

fn probe_service_owner(
    conn: &zbus::blocking::Connection,
    name: &str,
) -> Result<(), BridgeInit> {
    use zbus::blocking::Proxy;
    // 简化：用 DBus 自身的 ListNames 检测
    let proxy = zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .map_err(|e| {
        BridgeInit::Transient(format!("DBus proxy: {e}"))
    })?;
    // NameHasOwner: 检查服务是否有主
    let reply: Result<(bool,), _> = proxy.call("NameHasOwner", &(name,));
    match reply {
        Ok((true,)) => Ok(()),
        Ok((false,)) => {
            Err(BridgeInit::Unsupported(format!("{name}: no owner")))
        }
        Err(e) => Err(BridgeInit::Transient(format!("{name}: {e}"))),
    }
}

fn classify_init_error(e: &str) -> BridgeInit {
    if e.contains("UnknownMethod")
        || e.contains("UnknownObject")
        || e.contains("InterfaceNotFound")
        || e.contains("ServiceUnknown")
        || e.contains("NameHasNoOwner")
    {
        BridgeInit::Unsupported(e.to_string())
    } else {
        BridgeInit::Transient(e.to_string())
    }
}

/// 把 ibus 信号消息翻译为 FromWorker。
///
/// ibus UpdatePreeditText wire signature: `(vub)` = IBusText(variant), cursor_pos(uint), visible(bool)
/// HidePreeditText / HideLookupTable / ShowLookupTable = Unit signature（无 body）
fn handle_signal(
    name: &str,
    msg: &zbus::message::Message,
    ev_tx: &Sender<FromWorker>,
) -> Result<(), String> {
    use zbus::zvariant::OwnedValue;
    let body = msg.body();
    let sig = body.signature();
    let _ = sig; // 暂存供将来用

    match name {
        "CommitText" => {
            // body 是 IBusText 序列化的 variant（参见 ibus/src/ibusserializable.c
            // ibus_serializable_serialize_object）：
            //   GVariant 是 Tuple (s, ...)  —— 第一个 String 是 GObject 类型名
            //   （如 "IBusText"），后随 IBusText 字段（text: s, attrs: aav...）。
            // v0.10 修法：**跳过第一个 String**（GObject 类型名）——取**第二个**
            // String（真正的 commit 文本）。原 v0.9.45 之前实现错误地取了第一
            // 个 String，所以 commit 文本 = "IBusText"（用户日志可见）。
            let text = if let Ok(s) = body.deserialize::<zbus::zvariant::Structure>() {
                let strs: Vec<String> = s
                    .fields()
                    .iter()
                    .filter_map(|f| match f {
                        zbus::zvariant::Value::Str(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .collect();
                // 第一个是 GObject 类型名（"IBusText"），跳过；第二个是 IBusText.text
                strs.into_iter()
                    .find(|s| s != "IBusText" && !s.is_empty())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let _ = ev_tx.send(FromWorker::Commit(text));
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "UpdatePreeditText" | "UpdatePreeditTextWithMode" => {
            // wire: (vub) 或 (vubu)
            let mut fields_iter = match body.deserialize::<zbus::zvariant::Structure>() {
                Ok(s) => s.fields().to_vec(),
                Err(e) => {
                    // 兜底：作为预编辑文本处理
                    return Err(format!("UpdatePreeditText fields: {e}"));
                }
            };
            // v0.10 修法：IBusText 序列化的 variant 内部结构——
            // GVariant Tuple (s, IBusText fields)。第一个 String 是 GObject 类型名
            // （"IBusText"），后随 IBusText 实际字段（text: s, attrs: aav...）。
            // find_text_in_value 递归抓**任何** String 字段——但跳
            // 过类型名。
            let text = if !fields_iter.is_empty() {
                OwnedValue::try_from(&fields_iter[0])
                    .ok()
                    .and_then(|v| find_text_in_value(&v))
                    .filter(|s| s != "IBusText" && !s.is_empty())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            // wire (vub)：variant(text, cursor: u32, visible: bool)；
            // 或 (vubu)：+ mode: u32。cursor 在 variant 之外的字段。
            // v0.10：cursor_pos 直接取 wire 第 2 个字段（如果存在）。
            let cursor_begin = if fields_iter.len() > 1 {
                find_int_in_value(&fields_iter[1]).unwrap_or(text.chars().count() as i64) as i32
            } else {
                text.chars().count() as i32
            };
            let _ = ev_tx.send(FromWorker::Preedit {
                text,
                cursor_begin,
                cursor_end: cursor_begin,
                clear: false,
            });
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "HidePreeditText" => {
            let _ = ev_tx.send(FromWorker::Preedit {
                text: String::new(),
                cursor_begin: 0,
                cursor_end: 0,
                clear: true,
            });
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "DeleteSurroundingText" => {
            // wire: (iu) = offset(int), n_chars(uint)
            if let Ok(s) = body.deserialize::<zbus::zvariant::Structure>() {
                let fields = s.fields();
                let before = if fields.len() > 0 {
                    find_int_in_value(&fields[0]).unwrap_or(0).max(0) as u32
                } else {
                    0
                };
                let after = if fields.len() > 1 {
                    find_int_in_value(&fields[1]).unwrap_or(0).max(0) as u32
                } else {
                    0
                };
                let _ = ev_tx.send(FromWorker::DeleteSurrounding { before, after });
                let _ = ev_tx.send(FromWorker::Done(0));
            }
        }
        "UpdateLookupTable" => {
            // wire: (vbiavav) = IBusLookupTable, visible, ..., cursor_pos
            // 简化：先解析出 candidates 列表，其他字段用默认
            if let Ok(s) = body.deserialize::<zbus::zvariant::Structure>() {
                let fields = s.fields();
                // 第 1 字段（index 0）是 IBusLookupTable
                let parsed = if !fields.is_empty() {
                    OwnedValue::try_from(&fields[0])
                        .ok()
                        .and_then(|v| parse_lookup_table_v(&v))
                        .unwrap_or_else(|| (Vec::new(), 0))
                } else {
                    (Vec::new(), 0)
                };
                let (candidates, cursor_pos) = parsed;
                // 第 5 字段是 visible (bool)
                let visible = if fields.len() > 1 {
                    find_bool_in_value(&fields[1]).unwrap_or(!candidates.is_empty())
                } else {
                    !candidates.is_empty()
                };
                let page_size = candidates.len() as u32;
                let _ = ev_tx.send(FromWorker::LookupTable {
                    candidates,
                    labels: Vec::new(),
                    cursor_pos,
                    cursor_visible: true,
                    page_size,
                    orientation: 0,
                    visible,
                });
                let _ = ev_tx.send(FromWorker::Done(0));
            }
        }
        "ShowLookupTable" => {
            let _ = ev_tx.send(FromWorker::LookupTable {
                candidates: Vec::new(),
                labels: Vec::new(),
                cursor_pos: 0,
                cursor_visible: true,
                page_size: 0,
                orientation: 0,
                visible: true,
            });
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "HideLookupTable" => {
            let _ = ev_tx.send(FromWorker::LookupTable {
                candidates: Vec::new(),
                labels: Vec::new(),
                cursor_pos: 0,
                cursor_visible: false,
                page_size: 0,
                orientation: 0,
                visible: false,
            });
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "ForwardKeyEvent" | "RequireSurroundingText" | "UpdateProperty" | "RegisterProperties"
        | "Enabled" | "Disabled" => {
            // 不需要处理（mod 不拦截按键；RegisterProperties/UpdateProperty
            // 由 kimpanel 拉取，mod 不消费）
        }
        _ => {
            return Err(format!("unknown signal {name}"));
        }
    }
    Ok(())
}

/// 在 zvariant::Value 里找字符串。
/// 递归在 zvariant::Value 中找 String。
///
/// IBusText 序列化的 variant 内部可能是嵌套 Structure。v0.10 改：递归
/// 搜所有 String 字段，调用方过滤 GObject 类型名。
///
/// zbus 0.32 Value 不暴露 Variant 变体——所以直接用 Structure 递归。
fn find_text_in_value(v: &zbus::zvariant::Value<'_>) -> Option<String> {
    use zbus::zvariant::Value;
    match v {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        Value::Structure(s) => {
            for f in s.fields() {
                if let Some(s) = find_text_in_value(f) {
                    return Some(s);
                }
            }
            None
        }
        Value::Array(a) => {
            for item in a.iter() {
                if let Some(s) = find_text_in_value(item) {
                    return Some(s);
                }
            }
            None
        }
        _ => None,
    }
}

fn find_int_in_value(v: &zbus::zvariant::Value<'_>) -> Option<i64> {
    use zbus::zvariant::Value;
    match v {
        Value::U8(n) => Some(*n as i64),
        Value::U16(n) => Some(*n as i64),
        Value::U32(n) => Some(*n as i64),
        Value::U64(n) => Some(*n as i64),
        Value::I16(n) => Some(*n as i64),
        Value::I32(n) => Some(*n as i64),
        Value::I64(n) => Some(*n),
        _ => None,
    }
}

fn find_bool_in_value(v: &zbus::zvariant::Value<'_>) -> Option<bool> {
    use zbus::zvariant::Value;
    match v {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

/// 解析 IBusLookupTable 序列化的 variant 字段。
///
/// IBusLookupTable 序列化（参见 ibus/src/ibuslookuptable.c）：
///   1. parent class serialize 加 (s, "IBusLookupTable")
///   2. (u, page_size), (u, cursor_pos), (b, cursor_visible), (b, round)
///   3. (i, orientation)
///   4. (aav, candidates) —— 每个 candidate 是 IBusText 序列化的 variant
///
/// v0.10 修法：递归 find_text_in_value 抓所有 String 字段，**跳过
/// GObject 类型名**（"IBusLookupTable" / "IBusText"）——这些是序列化
/// 协议要求，不是用户内容。
fn parse_lookup_table_v(
    v: &zbus::zvariant::Value<'_>,
) -> Option<(Vec<String>, u32)> {
    use zbus::zvariant::Value;
    let s = match v {
        Value::Structure(s) => s,
        _ => return None,
    };
    let fields = s.fields();
    if fields.is_empty() {
        return Some((Vec::new(), 0));
    }
    // 抓所有 String 字段（递归），过滤掉 GObject 类型名
    let all_strs: Vec<String> = fields
        .iter()
        .filter_map(find_text_in_value)
        .filter(|s| s != "IBusLookupTable" && s != "IBusText" && !s.is_empty())
        .collect();
    // 前 10 个当 candidates
    let candidates: Vec<String> = all_strs.into_iter().take(10).collect();
    // cursor_pos：u32 字段之一（page_size, cursor_pos, ...）
    let cursor_pos = fields
        .iter()
        .find_map(find_int_in_value)
        .unwrap_or(0) as u32;
    Some((candidates, cursor_pos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ime::FocusChange;
    use std::sync::mpsc;

    #[test]
    fn name_is_dbus_ibus() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        assert_eq!(b.name(), "dbus-ibus");
    }

    #[test]
    fn fresh_bridge_is_ready_not_dead() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        assert!(b.is_ready());
        assert!(!b.is_dead());
    }

    #[test]
    fn submit_state_passes_through() {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        b.submit(DownEvent::State(FocusChange::Activate));
        match cmd_rx.recv().unwrap() {
            ToWorker::FocusIn => {}
            other => panic!("expected FocusIn, got {other:?}"),
        }
    }

    #[test]
    fn submit_surrounding_passes_text_and_cursor() {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        b.submit(DownEvent::Surrounding(SurroundingText {
            text: "hello".into(),
            cursor: 5,
            anchor: 5,
        }));
        match cmd_rx.recv().unwrap() {
            ToWorker::SetSurroundingText { text, cursor_pos, anchor_pos } => {
                assert_eq!(text, "hello");
                assert_eq!(cursor_pos, 5);
                assert_eq!(anchor_pos, 5);
            }
            other => panic!("expected SetSurroundingText, got {other:?}"),
        }
    }

    #[test]
    fn submit_cursor_rect_passes() {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        b.submit(DownEvent::CursorRect(CursorRect {
            x: 100,
            y: 200,
            w: 10,
            h: 20,
        }));
        match cmd_rx.recv().unwrap() {
            ToWorker::SetCursorLocationRelative { x, y, w, h } => {
                assert_eq!((x, y, w, h), (100, 200, 10, 20));
            }
            other => panic!("expected SetCursorLocationRelative, got {other:?}"),
        }
    }

    #[test]
    fn take_up_drains_commit() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        ev_tx.send(FromWorker::Commit("你".into())).unwrap();
        ev_tx.send(FromWorker::Done(0)).unwrap();
        let events = b.take_up_events();
        assert_eq!(events.len(), 2);
        match &events[0] {
            UpEvent::Commit(c) => assert_eq!(c.text, "你"),
            _ => panic!("expected Commit, got {:?}", events[0]),
        }
        match &events[1] {
            UpEvent::Done(d) => assert!(d.batch_id > 0),
            _ => panic!("expected Done, got {:?}", events[1]),
        }
    }

    #[test]
    fn take_up_drains_preedit_clear() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        ev_tx
            .send(FromWorker::Preedit {
                text: String::new(),
                cursor_begin: 0,
                cursor_end: 0,
                clear: true,
            })
            .unwrap();
        ev_tx.send(FromWorker::Done(0)).unwrap();
        let events = b.take_up_events();
        assert_eq!(events.len(), 2);
        match &events[0] {
            UpEvent::Preedit(p) => assert!(p.text.is_empty()),
            _ => panic!(),
        }
    }

    #[test]
    fn take_up_drains_lookup() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        ev_tx
            .send(FromWorker::LookupTable {
                candidates: vec!["一".into(), "二".into()],
                labels: Vec::new(),
                cursor_pos: 0,
                cursor_visible: true,
                page_size: 9,
                orientation: 0,
                visible: true,
            })
            .unwrap();
        ev_tx.send(FromWorker::Done(0)).unwrap();
        let events = b.take_up_events();
        assert_eq!(events.len(), 2);
        match &events[0] {
            UpEvent::LookupTable(lt) => {
                assert_eq!(lt.candidates, vec!["一", "二"]);
                assert!(lt.visible);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn take_up_returns_empty_when_no_events() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        assert!(b.take_up_events().is_empty());
    }

    #[test]
    fn dead_bridge_drops_submit() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusIbusBridge::from_channels(cmd_tx, ev_rx);
        b.dead = Some("test".into());
        // 不应 panic，也不应发 cmd
        b.submit(DownEvent::State(FocusChange::Activate));
        assert!(!b.is_ready());
    }
}
