//! dbus-fcitx5 桥接（C 方案 Layer 3 第二个后端）。
//!
//! 与 dbus-ibus 共享 ImeEvent 接口，仅 wire protocol 不同：
//! - ibus:    ProcessKeyEvent(keysym, evdev, state) -> bool
//!            CommitText, UpdatePreeditText, HidePreeditText, ...
//! - fcitx5:  ProcessKeyEvent(keysym, evdev, state, release, time) -> bool
//!            CommitString, UpdateFormattedPreedit, UpdateClientSideUI, ...
//!
//! 本文件结构是 dbus-ibus 的精简版——实现最小协议子集就够用。

use super::{ime_log, BridgeInit, HostBridge};
use crate::ime::{
    Commit, CursorRect, DeleteSurrounding, DownEvent, KeyEvent, LookupTable, PreeditUpdate,
    SurroundingText, UpEvent,
};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

const FCITX_SERVICE: &str = "org.freedesktop.portal.Fcitx";
const FCITX_IM_PATH: &str = "/org/freedesktop/portal/inputmethod";
const FCITX_IM_IFACE: &str = "org.fcitx.Fcitx.InputMethod1";

pub struct DbusFcitx5Bridge {
    cmd_tx: Sender<ToWorker>,
    ev_rx: Receiver<FromWorker>,
    ready: bool,
    dead: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ToWorker {
    ProcessKey { keysym: u32, evdev: u32, state: u32, release: bool },
    SetSurrounding { text: String, cursor: u32, anchor: u32 },
    SetCursorRect { x: i32, y: i32, w: i32, h: i32 },
    FocusIn,
    FocusOut,
}

#[derive(Debug)]
pub(crate) enum FromWorker {
    Commit(String),
    Preedit { text: String, cursor: i32 },
    DeleteSurrounding { before: u32, after: u32 },
    LookupTable { candidates: Vec<String>, cursor_pos: u32, visible: bool },
    Done,
    Fatal(String),
}

impl DbusFcitx5Bridge {
    pub fn connect() -> BridgeInit {
        ime_log!("[waylandcraft][host_bridge][dbus-fcitx5] probing...");
        // fcitx5 走 portal 接口（XDG Desktop Portal）
        let conn = match zbus::blocking::Connection::session() {
            Ok(c) => c,
            Err(e) => return BridgeInit::Transient(format!("session bus: {e}")),
        };
        // 探测 fcitx5 portal 服务
        if let Err(e) = probe_service_owner(&conn, FCITX_SERVICE) {
            return e;
        }
        // 建 InputContext
        let ic_conns = match connect_input_context(&conn) {
            Ok(c) => c,
            Err(e) => {
                return if e.contains("UnknownMethod") || e.contains("UnknownObject") {
                    BridgeInit::Unsupported(e)
                } else {
                    BridgeInit::Transient(e)
                };
            }
        };

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<ToWorker>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<FromWorker>();

        std::thread::Builder::new()
            .name("wc-host-bridge-fcitx5".into())
            .spawn(move || {
                command_loop(ic_conns, cmd_rx, ev_tx);
            })
            .expect("spawn worker thread");

        ime_log!("[waylandcraft][host_bridge][dbus-fcitx5] input context READY");
        BridgeInit::Ready(Box::new(Self {
            cmd_tx,
            ev_rx,
            ready: true,
            dead: None,
        }))
    }

    #[cfg(test)]
    pub(crate) fn from_channels(cmd_tx: Sender<ToWorker>, ev_rx: Receiver<FromWorker>) -> Self {
        Self {
            cmd_tx,
            ev_rx,
            ready: true,
            dead: None,
        }
    }
}

impl HostBridge for DbusFcitx5Bridge {
    fn name(&self) -> &'static str {
        "dbus-fcitx5"
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
            DownEvent::State(crate::ime::FocusChange::Activate) => Some(ToWorker::FocusIn),
            DownEvent::State(crate::ime::FocusChange::Deactivate) => Some(ToWorker::FocusOut),
            DownEvent::Key(KeyEvent { keycode, action, mods: _ }) => {
                let evdev = keycode.saturating_sub(8);
                let release = matches!(action, crate::seat::KeyboardAction::Release);
                let state = if release { 1u32 << 30 } else { 0 };
                Some(ToWorker::ProcessKey { keysym: 0, evdev, state, release })
            }
            DownEvent::Surrounding(SurroundingText { text, cursor, anchor }) => {
                Some(ToWorker::SetSurrounding { text, cursor, anchor })
            }
            DownEvent::CursorRect(CursorRect { x, y, w, h }) => {
                Some(ToWorker::SetCursorRect { x, y, w, h })
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
                Ok(FromWorker::Preedit { text, cursor }) => {
                    if text.is_empty() {
                        out.push(UpEvent::Preedit(PreeditUpdate::clear()));
                    } else {
                        out.push(UpEvent::Preedit(PreeditUpdate::set(text, cursor, cursor)));
                    }
                }
                Ok(FromWorker::DeleteSurrounding { before, after }) => {
                    out.push(UpEvent::DeleteSurrounding(DeleteSurrounding {
                        before_length: before,
                        after_length: after,
                    }));
                }
                Ok(FromWorker::LookupTable { candidates, cursor_pos, visible }) => {
                    out.push(UpEvent::LookupTable(LookupTable {
                        candidates,
                        labels: Vec::new(),
                        cursor_pos,
                        cursor_visible: true,
                        page_size: 10,
                        orientation: 0,
                        visible,
                    }));
                }
                Ok(FromWorker::Done) => {
                    out.push(UpEvent::Done(crate::ime::Done { batch_id: 0 }));
                }
                Ok(FromWorker::Fatal(msg)) => {
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
        let _ = self.cmd_tx.send(ToWorker::SetCursorRect {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
        });
    }
}

fn command_loop(
    ic_conns: IcConnections,
    cmd_rx: Receiver<ToWorker>,
    ev_tx: Sender<FromWorker>,
) {
    use zbus::blocking::Proxy;
    for sig in WATCHED_SIGNALS {
        let ic = ic_conns.ic.clone();
        let ev_tx = ev_tx.clone();
        let name = (*sig).to_string();
        std::thread::Builder::new()
            .name(format!("wc-fcitx5-sig-{sig}"))
            .spawn(move || {
                if let Ok(iter) = ic.receive_signal(name.as_str()) {
                    for msg in iter {
                        let _ = handle_signal(&name, &msg, &ev_tx);
                    }
                }
            })
            .expect("spawn signal thread");
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
            ToWorker::SetCursorRect { x, y, w, h } => ic_conns
                .ic
                .call::<_, _, ()>("SetCursorRect", &(x, y, w, h))
                .map(|_| ())
                .map_err(|e| e.to_string()),
            ToWorker::SetSurrounding { text, cursor, anchor } => ic_conns
                .ic
                .call::<_, _, ()>("SetSurroundingText", &(text, cursor, anchor))
                .map(|_| ())
                .map_err(|e| e.to_string()),
            ToWorker::ProcessKey { keysym, evdev, state, release } => {
                let time_ms: u32 = 0;
                let _ = ic_conns.ic.call::<_, _, bool>(
                    "ProcessKeyEvent",
                    &(keysym, evdev, state, release, time_ms),
                );
                Ok(())
            }
        };
        if let Err(e) = res {
            ime_log!("[waylandcraft][host_bridge][dbus-fcitx5] 命令失败: {e}");
            let _ = ev_tx.send(FromWorker::Fatal(e));
            return;
        }
    }
}

const WATCHED_SIGNALS: &[&str] = &[
    "CommitString",
    "UpdateFormattedPreedit",
    "UpdateClientSideUI",
    "DeleteSurroundingText",
    "ForwardKey",
];

struct IcConnections {
    _conn: zbus::blocking::Connection,
    ic: zbus::blocking::Proxy<'static>,
}

fn connect_input_context(conn: &zbus::blocking::Connection) -> Result<IcConnections, String> {
    use zbus::blocking::Proxy;
    let ic: Proxy<'static> =
        Proxy::new_owned(conn.clone(), FCITX_SERVICE, FCITX_IM_PATH, FCITX_IM_IFACE)
            .map_err(|e| format!("input context proxy: {e}"))?;
    Ok(IcConnections { _conn: conn.clone(), ic })
}

fn probe_service_owner(
    conn: &zbus::blocking::Connection,
    name: &str,
) -> Result<(), BridgeInit> {
    use zbus::blocking::Proxy;
    let proxy = zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .map_err(|e| BridgeInit::Transient(format!("DBus proxy: {e}")))?;
    let reply: Result<(bool,), _> = proxy.call("NameHasOwner", &(name,));
    match reply {
        Ok((true,)) => Ok(()),
        Ok((false,)) => Err(BridgeInit::Unsupported(format!("{name}: no owner"))),
        Err(e) => Err(BridgeInit::Transient(format!("{name}: {e}"))),
    }
}

fn handle_signal(
    name: &str,
    msg: &zbus::message::Message,
    ev_tx: &Sender<FromWorker>,
) -> Result<(), String> {
    let body = msg.body();
    match name {
        "CommitString" => {
            if let Ok(s) = body.deserialize::<zbus::zvariant::Structure>() {
                let text = s
                    .fields()
                    .iter()
                    .find_map(|f| match f {
                        zbus::zvariant::Value::Str(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let _ = ev_tx.send(FromWorker::Commit(text));
                let _ = ev_tx.send(FromWorker::Done);
            }
        }
        "UpdateFormattedPreedit" => {
            if let Ok(s) = body.deserialize::<zbus::zvariant::Structure>() {
                let mut text = String::new();
                let mut cursor: i32 = 0;
                for f in s.fields() {
                    if let zbus::zvariant::Value::Str(s) = f {
                        text.push_str(&s.to_string());
                    } else if let zbus::zvariant::Value::I32(c) = f {
                        cursor = *c;
                    }
                }
                let _ = ev_tx.send(FromWorker::Preedit { text, cursor });
                let _ = ev_tx.send(FromWorker::Done);
            }
        }
        "UpdateClientSideUI" => {
            if let Ok(s) = body.deserialize::<zbus::zvariant::Structure>() {
                let mut candidates = Vec::new();
                let mut visible = false;
                for f in s.fields() {
                    if let zbus::zvariant::Value::Array(a) = f {
                        for item in a.iter() {
                            if let zbus::zvariant::Value::Str(s) = item {
                                if candidates.len() < 10 {
                                    candidates.push(s.to_string());
                                }
                            }
                        }
                    } else if let zbus::zvariant::Value::Bool(b) = f {
                        visible = *b;
                    }
                }
                let _ = ev_tx.send(FromWorker::LookupTable {
                    candidates,
                    cursor_pos: 0,
                    visible,
                });
                let _ = ev_tx.send(FromWorker::Done);
            }
        }
        "DeleteSurroundingText" => {
            if let Ok(s) = body.deserialize::<zbus::zvariant::Structure>() {
                let mut before: u32 = 0;
                let mut after: u32 = 0;
                let mut iter = s.fields().iter();
                if let Some(f) = iter.next() {
                    if let zbus::zvariant::Value::I32(n) = f {
                        before = (*n).max(0) as u32;
                    }
                }
                if let Some(f) = iter.next() {
                    if let zbus::zvariant::Value::U32(n) = f {
                        after = *n;
                    }
                }
                let _ = ev_tx.send(FromWorker::DeleteSurrounding { before, after });
                let _ = ev_tx.send(FromWorker::Done);
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ime::FocusChange;
    use std::sync::mpsc;

    #[test]
    fn name_is_dbus_fcitx5() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let b = DbusFcitx5Bridge::from_channels(cmd_tx, ev_rx);
        assert_eq!(b.name(), "dbus-fcitx5");
    }

    #[test]
    fn fresh_bridge_is_ready() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let b = DbusFcitx5Bridge::from_channels(cmd_tx, ev_rx);
        assert!(b.is_ready());
        assert!(!b.is_dead());
    }

    #[test]
    fn submit_focus_in() {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusFcitx5Bridge::from_channels(cmd_tx, ev_rx);
        b.submit(DownEvent::State(FocusChange::Activate));
        match cmd_rx.recv().unwrap() {
            ToWorker::FocusIn => {}
            other => panic!("expected FocusIn, got {other:?}"),
        }
    }

    #[test]
    fn submit_process_key() {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (_ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusFcitx5Bridge::from_channels(cmd_tx, ev_rx);
        b.submit(DownEvent::Key(KeyEvent {
            keycode: 31, // i = evdev 23 + 8
            action: crate::seat::KeyboardAction::Press,
            mods: (0, 0, 0, 0),
        }));
        match cmd_rx.recv().unwrap() {
            ToWorker::ProcessKey { evdev, release, .. } => {
                assert_eq!(evdev, 23);
                assert!(!release);
            }
            other => panic!("expected ProcessKey, got {other:?}"),
        }
    }

    #[test]
    fn take_up_drains_commit() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusFcitx5Bridge::from_channels(cmd_tx, ev_rx);
        ev_tx.send(FromWorker::Commit("好".into())).unwrap();
        ev_tx.send(FromWorker::Done).unwrap();
        let events = b.take_up_events();
        assert_eq!(events.len(), 2);
        match &events[0] {
            UpEvent::Commit(c) => assert_eq!(c.text, "好"),
            _ => panic!(),
        }
    }

    #[test]
    fn take_up_drains_preedit_set() {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let mut b = DbusFcitx5Bridge::from_channels(cmd_tx, ev_rx);
        ev_tx
            .send(FromWorker::Preedit {
                text: "年".into(),
                cursor: 1,
            })
            .unwrap();
        ev_tx.send(FromWorker::Done).unwrap();
        let events = b.take_up_events();
        match &events[0] {
            UpEvent::Preedit(p) => {
                assert_eq!(p.text, "年");
                assert_eq!(p.cursor_begin, 1);
            }
            _ => panic!(),
        }
    }
}
