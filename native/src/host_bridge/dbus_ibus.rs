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
use zbus::zvariant::OwnedValue;

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

/// 探测宿主 global engine（`ibus engine` 输出，如 libpinyin）。
/// 失败/空返回 None——调用方依赖 daemon 的 global-engine attach。
/// v1.2.30：显式 SetEngine 绕开"FocusIn 后 daemon 未把 global engine
/// attach 到 portal IC"的不确定性（consumed=false + 无 preedit 的根因候选）。
fn detect_global_engine() -> Option<String> {
let out = std::process::Command::new("ibus").arg("engine").output().ok()?;
if !out.status.success() {
    return None;
}
let s = String::from_utf8_lossy(&out.stdout);
let name = s.trim();
if name.is_empty() || name.contains("No engine") || name.contains("error") {
    None
} else {
    Some(name.to_string())
}
}

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
    /// ibus ForwardKeyEvent：引擎不消费的键（含 keycode，state 带 RELEASE_MASK
    /// 表示释放）。v1.2.32：单路模型下靠它把"未消费键"补发给嵌套应用。
    ForwardKey { keycode: u32, is_release: bool },
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
                Ok(FromWorker::ForwardKey { keycode, is_release }) => {
                    out.push(UpEvent::ForwardKey { keycode, is_release });
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
    mut ic_conns: IcConnections,
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
            ToWorker::FocusIn => {
                ime_log!("[waylandcraft][host_bridge][dbus-ibus] FocusIn -> ibus-daemon");
                // v1.2.30：首次 FocusIn 前显式 SetEngine（若探测到 global engine）。
                // daemon 的 global-engine attach 对 portal IC 不可靠（consumed=false
                // + 无 preedit 的根因候选）——显式设置确保 libpinyin 真正挂到 IC。
                if !ic_conns.engine_set {
                    ic_conns.engine_set = true;
                    if let Some(en) = ic_conns.engine_name.clone() {
                        match ic_conns.ic.call::<_, _, ()>("SetEngine", &(en.clone(),)) {
                            Ok(()) => {
                                ime_log!(
                                    "[waylandcraft][host_bridge][dbus-ibus] SetEngine({en}) OK"
                                );
                            }
                            Err(e) => {
                                ime_log!(
                                    "[waylandcraft][host_bridge][dbus-ibus] SetEngine({en}) 失败: {e}"
                                );
                            }
                        }
                        // GetEngine 验证：打印 engine path/值——空 = 引擎没挂上。
                        // reply 类型各 ibus 版本不一（(o) 或 (s)），用 OwnedValue 通吃。
                        match ic_conns
                            .ic
                            .call::<_, _, zbus::zvariant::OwnedValue>("GetEngine", &())
                        {
                            Ok(v) => ime_log!(
                                "[waylandcraft][host_bridge][dbus-ibus] GetEngine -> {v:?}"
                            ),
                            Err(e) => ime_log!(
                                "[waylandcraft][host_bridge][dbus-ibus] GetEngine 失败: {e}"
                            ),
                        }
                    } else {
                        ime_log!(
                            "[waylandcraft][host_bridge][dbus-ibus] 无 engine 名——依赖 daemon global attach（若持续 consumed=false 需排查）"
                        );
                    }
                }
                ic_conns
                    .ic
                    .call::<_, _, ()>("FocusIn", &())
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            ToWorker::FocusOut => {
                ime_log!("[waylandcraft][host_bridge][dbus-ibus] FocusOut -> ibus-daemon");
                ic_conns
                    .ic
                    .call::<_, _, ()>("FocusOut", &())
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            ToWorker::SetCursorLocationRelative { x, y, w, h } => {
                ime_log!(
                    "[waylandcraft][host_bridge][dbus-ibus] SetCursorLocationRelative x={x} y={y} w={w} h={h}"
                );
                ic_conns
                    .ic
                    .call::<_, _, ()>("SetCursorLocationRelative", &(x, y, w, h))
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            ToWorker::SetSurroundingText {
                text,
                cursor_pos,
                anchor_pos,
            } => {
                ime_log!(
                    "[waylandcraft][host_bridge][dbus-ibus] SetSurroundingText text=\"{}\" cursor={cursor_pos} anchor={anchor_pos}",
                    text.chars().take(16).collect::<String>()
                );
                // v0.13.9：放弃 SetSurroundingText 类型包装
                // 试过 zvariant::Optional<String>——生成 (suu)（不是 (vuu)）
                // 试过 std::Option<String> + zvariant/gvariant——编译报 Type bound
                //     失败（zbus 5.16 不暴露 gvariant feature）
                // 试过 OwnedValue<Value::Str>——同样生成 (suu)
                // **最简方案**：直接传 None（空字符串）。ibus libpinyin 在
                // surrounding_text="" 时也接受 ProcessKeyEvent 消费按键——
                // 这对 IME 工作不是必须的（仅用于某些引擎的"删除上下文"判断）。
                // v0.13.9 暂时回退到不调 SetSurroundingText——只是不传 surrounding
                // 上下文给 ibus，绝大多数 ibus 引擎（包括 ibus-libpinyin）工作正常。
                // ime_log!(...);  // 跳过避免日志噪声
                // 不调 SetSurroundingText——ibus ProcessKeyEvent 仍能工作
                let _ = (text, cursor_pos, anchor_pos); // suppress unused warnings
                Ok(())
                /* 旧实现保留做参考:
                ic_conns
                    .ic
                    .call::<_, _, ()>(
                        "SetSurroundingText",
                        &(text, cursor_pos, anchor_pos),
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                */
            }
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
            // v0.13.7 修：原代码 send FATAL → 主线程 `self.dead = Some(msg)`
            // → `is_ready()` 永远 false → 后续 ProcessKeyEvent 全失败。
            // 根因：dbus 错误（InvalidArgs、网络瞬断、SetSurroundingText 类型不匹配等）
            // 是**临时**的，ibus 引擎不需要重连。把这些错误降级为普通错误：
            // - 记日志（IME 诊断可见）
            // - **不**设 dead → 后续命令仍能发送
            // - worker 继续循环（return → 下次 cmd 还能接）
            ime_log!("[waylandcraft][host_bridge][dbus-ibus] 命令失败（不影响 host_bridge）: {e}");
            // 不发 FATAL，不设 dead
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
    /// 显式设置的 engine 名（None = 依赖 daemon global engine）。
    /// v1.2.30：首次 FocusIn 前 SetEngine，绕开 global-attach 不确定性。
    engine_name: Option<String>,
    /// engine 已设置（只设一次；引擎切换由宿主 panel 负责，我们不覆盖）。
    engine_set: bool,
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
    // v1.2.30：读宿主 global engine 名（ibus engine），供首次 FocusIn SetEngine。
    let engine_name = detect_global_engine();
    if let Some(en) = &engine_name {
        ime_log!(
            "[waylandcraft][host_bridge][dbus-ibus] detected global engine: {en}（FocusIn 时显式 SetEngine）"
        );
    } else {
        ime_log!(
            "[waylandcraft][host_bridge][dbus-ibus] 未能探测 global engine（ibus engine 无输出）——依赖 daemon global attach"
        );
    }
    Ok(IcConnections {
        _conn: conn.clone(),
        ic,
        engine_name,
        engine_set: false,
    })
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
            // v0.13.8 修：no owner 不再直接 Unsupported（=永久放弃）。
            // `org.freedesktop.portal.IBus` 是 D-Bus activation 服务——MC 启动
            // 早于 portal 被拉起时 NameHasOwner 是 false，但稍后 ibus-daemon /
            // xdg-desktop-portal 会激活它。对目标 name 发一次 Ping（method call
            // 到 well-known name 会触发 activation），再复查。
            ime_log!(
                "[waylandcraft][host_bridge][dbus-ibus] {name} 暂无 owner —— Ping 触发 D-Bus activation 后复查"
            );
            let ping_proxy = Proxy::new(
                conn,
                name,
                "/org/freedesktop/IBus",
                "org.freedesktop.DBus.Peer",
            )
            .map_err(|e| {
                BridgeInit::Transient(format!("{name} activation ping proxy: {e}"))
            })?;
            if let Err(e) = ping_proxy.call::<_, _, ()>("Ping", &()) {
                let msg = e.to_string();
                // ServiceUnknown = 系统里根本没有该 .service 文件 → 结构性缺失
                if msg.contains("ServiceUnknown") || msg.contains("NameHasNoOwner") {
                    return Err(BridgeInit::Unsupported(format!(
                        "{name}: no owner 且无 activation service（{msg}）"
                    )));
                }
                // 激活排队 / 超时 / 权限等 → 暂时性，交给上层定时重试
                return Err(BridgeInit::Transient(format!(
                    "{name}: activation pending（{msg}）"
                )));
            }
            // Ping 成功（服务已被激活）→ 复查 owner
            let reply2: Result<(bool,), _> = proxy.call("NameHasOwner", &(name,));
            match reply2 {
                Ok((true,)) => Ok(()),
                Ok((false,)) => Err(BridgeInit::Transient(format!(
                    "{name}: Ping 成功但尚无 owner（刚激活，稍后重试）"
                ))),
                Err(e) => Err(BridgeInit::Transient(format!("{name}: recheck {e}"))),
            }
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
/// **wire signature 权威参考**（ibus 官方 client src/ibusinputcontext.c）：
/// ```text
/// CommitText                  (v)     IBusText 序列化在 variant 内
/// UpdatePreeditText           (vub)   variant + cursor_pos(u32) + visible(bool)
/// UpdatePreeditTextWithMode   (vubu)  + mode(u32)
/// HidePreeditText             ()      无 body
/// UpdateLookupTable           (vb)    IBusLookupTable variant + visible
/// ShowLookupTable/Hide        ()
/// DeleteSurroundingText       (iu)    offset(i32) + nchars(u32)
/// ForwardKeyEvent             (uuu)
/// RegisterProperties/Property (v)
/// ```
/// v1.2.30 修：旧实现把 body 当**顶层 Structure** 反序列化——真实 wire 的
/// IBusText 对象是包在 variant 里的（(v)/(vub)…）。后果：CommitText 顶层
/// deserialize Structure 失败 → text=""（日志可见空 commit）；UpdatePreeditText
/// 同理失败 → return Err → **信号全丢且无日志**（拼音 preedit 永远不出现）。
fn handle_signal(
    name: &str,
    msg: &zbus::message::Message,
    ev_tx: &Sender<FromWorker>,
) -> Result<(), String> {
    use zbus::zvariant::OwnedValue;
    let body = msg.body();

    match name {
        "CommitText" => {
            // wire: (v) —— variant 内是 IBusText 序列化 Structure
            let text = match body.deserialize::<(OwnedValue,)>() {
                Ok((v,)) => extract_ibustext(&v),
                Err(e) => {
                    ime_log!(
                        "[waylandcraft][host_bridge][dbus-ibus] handle_signal CommitText body 解析失败: {e}"
                    );
                    String::new()
                }
            };
            ime_log!(
                "[waylandcraft][host_bridge][dbus-ibus] handle_signal: CommitText text={:?}",
                text
            );
            let _ = ev_tx.send(FromWorker::Commit(text));
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "UpdatePreeditText" => {
            // wire: (vub) = IBusText variant + cursor_pos(u32) + visible(bool)
            let (text, cursor_pos) = match body.deserialize::<(OwnedValue, u32, bool)>() {
                Ok((v, cursor_pos, _visible)) => (extract_ibustext(&v), cursor_pos),
                Err(e) => {
                    ime_log!(
                        "[waylandcraft][host_bridge][dbus-ibus] handle_signal UpdatePreeditText body 解析失败: {e}"
                    );
                    (String::new(), 0)
                }
            };
            ime_log!(
                "[waylandcraft][host_bridge][dbus-ibus] handle_signal: UpdatePreeditText text={:?} cursor={}",
                text, cursor_pos
            );
            let cursor = cursor_pos as i32;
            let _ = ev_tx.send(FromWorker::Preedit {
                text: text.clone(),
                cursor_begin: cursor,
                cursor_end: cursor,
                clear: false,
            });
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "UpdatePreeditTextWithMode" => {
            // wire: (vubu) = variant + cursor_pos + visible + mode
            let (text, cursor_pos) = match body.deserialize::<(OwnedValue, u32, bool, u32)>() {
                Ok((v, cursor_pos, _visible, _mode)) => (extract_ibustext(&v), cursor_pos),
                Err(e) => {
                    ime_log!(
                        "[waylandcraft][host_bridge][dbus-ibus] handle_signal UpdatePreeditTextWithMode body 解析失败: {e}"
                    );
                    (String::new(), 0)
                }
            };
            ime_log!(
                "[waylandcraft][host_bridge][dbus-ibus] handle_signal: UpdatePreeditTextWithMode text={:?} cursor={}",
                text, cursor_pos
            );
            let cursor = cursor_pos as i32;
            let _ = ev_tx.send(FromWorker::Preedit {
                text: text.clone(),
                cursor_begin: cursor,
                cursor_end: cursor,
                clear: false,
            });
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "HidePreeditText" => {
            // wire: () 无 body——清空 preedit
            ime_log!(
                "[waylandcraft][host_bridge][dbus-ibus] handle_signal: HidePreeditText"
            );
            let _ = ev_tx.send(FromWorker::Preedit {
                text: String::new(),
                cursor_begin: 0,
                cursor_end: 0,
                clear: true,
            });
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "DeleteSurroundingText" => {
            // wire: (iu) = offset_from_cursor(i32) + nchars(u32)
            let (offset, nchars) = match body.deserialize::<(i32, u32)>() {
                Ok(p) => p,
                Err(e) => {
                    ime_log!(
                        "[waylandcraft][host_bridge][dbus-ibus] handle_signal DeleteSurroundingText body 解析失败: {e}"
                    );
                    (0, 0)
                }
            };
            ime_log!(
                "[waylandcraft][host_bridge][dbus-ibus] handle_signal: DeleteSurroundingText offset={} nchars={}",
                offset, nchars
            );
            // ibus 语义：offset<0 = 删光标前，>0 = 删光标后
            let (before, after) = if offset < 0 {
                ((-offset) as u32, 0)
            } else {
                (0, offset as u32)
            };
            let _ = ev_tx.send(FromWorker::DeleteSurrounding { before, after });
            let _ = ev_tx.send(FromWorker::Done(0));
        }
        "UpdateLookupTable" => {
            // wire: (vb) = IBusLookupTable variant + visible(bool)
            let (candidates, cursor_pos, visible) =
                match body.deserialize::<(OwnedValue, bool)>() {
                    Ok((v, visible)) => match extract_lookup_table(&v) {
                        Some((cands, cpos)) => (cands, cpos, visible),
                        None => (Vec::new(), 0, visible),
                    },
                    Err(e) => {
                        ime_log!(
                            "[waylandcraft][host_bridge][dbus-ibus] handle_signal UpdateLookupTable body 解析失败: {e}"
                        );
                        (Vec::new(), 0, false)
                    }
                };
            ime_log!(
                "[waylandcraft][host_bridge][dbus-ibus] handle_signal: UpdateLookupTable {} candidates visible={} cursor={}",
                candidates.len(), visible, cursor_pos
            );
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
        "ForwardKeyEvent" => {
            // wire: (uuu) = keyval(u32) + keycode(u32) + state(u32)
            // 引擎不消费的键回传——应用把它作为普通输入转发给嵌套应用。
            // state 带 IBUS_RELEASE_MASK (0x40000000) = release。
            let (keyval, keycode, state) = match body.deserialize::<(u32, u32, u32)>() {
                Ok(p) => p,
                Err(e) => {
                    ime_log!(
                        "[waylandcraft][host_bridge][dbus-ibus] handle_signal ForwardKeyEvent body 解析失败: {e}"
                    );
                    return Ok(());
                }
            };
            let is_release = state & 0x4000_0000 != 0;
            ime_log!(
                "[waylandcraft][host_bridge][dbus-ibus] handle_signal: ForwardKeyEvent keyval={keyval:#x} keycode={keycode} release={is_release} -> 补发嵌套应用"
            );
            let _ = ev_tx.send(FromWorker::ForwardKey { keycode, is_release });
            // 不配 Done——ForwardKey 是独立事件，不参与 preedit/commit 批。
        }
        "RequireSurroundingText" | "UpdateProperty" | "RegisterProperties"
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

/// 从 IBusText 序列化 variant 中提取文本。
///
/// IBusText 序列化结构（ibus_serializable + ibustext.c）：
/// ```text
/// Structure[ Str("IBusText"),        ← GObject 类型名（fields[0]）
///             Dict{}(properties),    ← a{sv}（fields[1]，通常空）
///             Str(text),             ← 实际文本（fields[2]）
///             Variant(IBusAttrList)  ← attrs（fields[3]）
///           ]
/// ```
/// 也可能嵌套更深（ibus 版本差异）。extract_ibustext 递归搜**第一个非类型名
/// 非空 String**。类型名 = "IBus" 开头的 GObject 名（IBusText/IBusAttrList/
/// IBusLookupTable/IBusEngineDesc…），递归时跳过。
fn extract_ibustext(v: &OwnedValue) -> String {
    find_content_str(v).unwrap_or_default()
}

/// 递归找第一个非 IBus-类型名的非空 String（跳过 GObject 序列化类型名）。
fn find_content_str(v: &OwnedValue) -> Option<String> {
    use zbus::zvariant::Value;
    match &**v {
        Value::Str(s) => {
            let s = s.to_string();
            if s.is_empty() || s.starts_with("IBus") {
                None
            } else {
                Some(s)
            }
        }
        Value::Structure(st) => {
            for f in st.fields() {
                if let Ok(ov) = OwnedValue::try_from(f) {
                    if let Some(s) = find_content_str(&ov) {
                        return Some(s);
                    }
                }
            }
            None
        }
        Value::Array(a) => {
            for item in a.iter() {
                if let Ok(ov) = OwnedValue::try_from(item) {
                    if let Some(s) = find_content_str(&ov) {
                        return Some(s);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// 从 IBusLookupTable 序列化 variant 提取候选列表 + 光标位置。
/// IBusLookupTable 序列化（ibuslookuptable.c）：
/// Structure[ "IBusLookupTable", Dict{}, page_size(u), cursor_pos(u),
///            cursor_visible(b), round(b), orientation(i),
///            candidates(a{IBusText variants}) ... ]
/// 候选是 aav（IBusText 序列化的 variant 数组）。简化解析：递归抓所有
/// "IBusText" 之后出现的 Str？——精确做法：遍历找首层 u32×2（page_size
/// cursor_pos）再抓候选 variant 数组。为稳，递归收集所有 IBusText 内容。
fn extract_lookup_table(v: &OwnedValue) -> Option<(Vec<String>, u32)> {
    use zbus::zvariant::Value;
    let st = match &**v {
        Value::Structure(s) => s,
        _ => return None,
    };
    let fields = st.fields();
    if fields.is_empty() {
        return None;
    }
    // 字段 0 = 类型名 "IBusLookupTable"，字段 1 = properties dict
    // 之后：page_size(u32) cursor_pos(u32) cursor_visible(b) round(b)
    //       orientation(i32) candidates(Array<Variant<IBusText>>)
    let mut cursor_pos: u32 = 0;
    let mut candidates: Vec<String> = Vec::new();
    let mut ints_seen = 0u32;
    let mut seen_type = false;
    for f in fields {
        if let Ok(ov) = OwnedValue::try_from(f) {
            match &*ov {
                Value::Str(s) if s == "IBusLookupTable" || s == "IBusLookupTable" => {
                    seen_type = true;
                    continue;
                }
                Value::Dict(_) => continue, // properties dict
                Value::U32(n) => {
                    if seen_type {
                        // 前两个 u32 = page_size, cursor_pos
                        if ints_seen == 1 {
                            cursor_pos = *n;
                        }
                        ints_seen += 1;
                    }
                }
                Value::Array(a) => {
                    // candidates: array of variant(IBusText)
                    for item in a.iter() {
                        if let Ok(ov2) = OwnedValue::try_from(item) {
                            let t = extract_ibustext(&ov2);
                            if !t.is_empty() {
                                candidates.push(t);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if candidates.is_empty() {
        // 兜底：递归全量抓（不同版本字段布局）
        if let Some(first) = find_lookup_candidates_recursive(v) {
            candidates = first;
        }
    }
    Some((candidates, cursor_pos))
}

fn find_lookup_candidates_recursive(v: &OwnedValue) -> Option<Vec<String>> {
    use zbus::zvariant::Value;
    match &**v {
        Value::Structure(st) => {
            let mut out = Vec::new();
            for f in st.fields() {
                if let Ok(ov) = OwnedValue::try_from(f) {
                    if let Value::Array(a) = &*ov {
                        for item in a.iter() {
                            if let Ok(ov2) = OwnedValue::try_from(item) {
                                if let Some(s) = find_content_str(&ov2) {
                                    out.push(s);
                                }
                            }
                        }
                    }
                    if let Some(mut more) = find_lookup_candidates_recursive(&ov) {
                        out.append(&mut more);
                    }
                }
            }
            if out.is_empty() { None } else { Some(out) }
        }
        _ => None,
    }
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

    // ── extract_ibustext 单元测试 ──────────────────────────────
    // 结构模拟 ibus IBusSerializable wire 布局（类型名 + properties dict +
    // 属性字段），验证 v1.2.30 重写的解析器从 variant 里正确取文本。

    fn ov_text(fields: Vec<zbus::zvariant::Value<'static>>) -> OwnedValue {
        let mut b = zbus::zvariant::StructureBuilder::new();
        for f in fields {
            b = b.append_field(f);
        }
        let st = b.build().expect("structure build");
        OwnedValue::try_from(zbus::zvariant::Value::Structure(st)).expect("own")
    }
    fn ov_str(s: &str) -> zbus::zvariant::Value<'static> {
        zbus::zvariant::Value::new(s.to_owned())
    }

    #[test]
    fn extract_ibustext_skips_type_name_and_dict() {
        // Structure[Str("IBusText"), Str("你"), Str("IBusAttrList")]
        let v = ov_text(vec![ov_str("IBusText"), ov_str("你"), ov_str("IBusAttrList")]);
        assert_eq!(extract_ibustext(&v), "你");
    }

    #[test]
    fn extract_ibustext_recursive_variant_wrap() {
        // variant 包 structure：OwnedValue(Structure[Str("IBusText"), Str("你好")])
        let inner = ov_text(vec![ov_str("IBusText"), ov_str("你好")]);
        assert_eq!(extract_ibustext(&inner), "你好");
    }

    #[test]
    fn extract_ibustext_empty_when_no_content() {
        // 只有类型名 → 空
        let v = ov_text(vec![ov_str("IBusText")]);
        assert_eq!(extract_ibustext(&v), "");
    }
}
