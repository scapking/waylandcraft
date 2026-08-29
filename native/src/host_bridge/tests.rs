//! host_bridge 集成测试（mock 通道验证多后端统一接口）。

use super::*;
use crate::ime::{Commit, CursorRect, DeleteSurrounding, DownEvent, FocusChange, KeyEvent,
    LookupTable, PreeditUpdate, SurroundingText, UpEvent};
use std::sync::mpsc;

#[test]
fn take_up_events_batched_groups_by_done() {
    // 验证：Done 边界正确分组，preedit+commit 同一批；收尾不丢
    let (cmd_tx, _cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    let inner = Box::new(crate::host_bridge::dbus_ibus::DbusIbusBridge::from_channels(cmd_tx, ev_rx));
    let mut h = HostBridgeHandle::new(inner);

    // 批次 1：preedit
    ev_tx_tx_commit_preedit(&ev_tx, "年", true, false);
    ev_tx_tx_done(&ev_tx);
    // 批次 2：commit
    ev_tx_tx_commit_preedit(&ev_tx, "你", false, true);
    ev_tx_tx_done(&ev_tx);

    let batches = h.take_up_events_batched();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 2); // preedit + done
    assert_eq!(batches[1].len(), 2); // commit + done
}

#[test]
fn batched_handles_no_done() {
    // 没有 Done 标记的尾批不应丢
    let (cmd_tx, _cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    let inner = Box::new(crate::host_bridge::dbus_ibus::DbusIbusBridge::from_channels(cmd_tx, ev_rx));
    let mut h = HostBridgeHandle::new(inner);

    ev_tx_tx_commit_preedit(&ev_tx, "你", false, true);
    // 不发 Done

    let batches = h.take_up_events_batched();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 1); // commit 仍然返回
}

#[test]
fn empty_handle_returns_empty_batches() {
    let (cmd_tx, _cmd_rx) = mpsc::channel();
    let (_ev_tx, ev_rx) = mpsc::channel();
    let inner = Box::new(crate::host_bridge::dbus_ibus::DbusIbusBridge::from_channels(cmd_tx, ev_rx));
    let mut h = HostBridgeHandle::new(inner);
    assert!(h.take_up_events_batched().is_empty());
}

#[test]
fn submit_propagates_to_all_event_types() {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (_ev_tx, ev_rx) = mpsc::channel();
    let inner = Box::new(crate::host_bridge::dbus_ibus::DbusIbusBridge::from_channels(cmd_tx, ev_rx));
    let mut h = HostBridgeHandle::new(inner);

    // 6 种 DownEvent
    h.submit(DownEvent::State(FocusChange::Activate));
    h.submit(DownEvent::Key(KeyEvent {
        keycode: 31,
        action: crate::seat::KeyboardAction::Press,
        mods: (0, 0, 0, 0),
    }));
    h.submit(DownEvent::Surrounding(SurroundingText {
        text: "abc".into(),
        cursor: 3,
        anchor: 3,
    }));
    h.submit(DownEvent::CursorRect(CursorRect {
        x: 1,
        y: 2,
        w: 3,
        h: 4,
    }));
    h.submit(DownEvent::State(FocusChange::Deactivate));

    // 5 条命令已发出
    // 第 1 条命令应是 FocusIn（其它 4 条不检查具体类型）
    let first_cmd = cmd_rx.try_recv().unwrap();
    assert!(
        matches!(first_cmd, crate::host_bridge::dbus_ibus::ToWorker::FocusIn),
        "expected FocusIn, got {first_cmd:?}"
    );
    let _ = cmd_rx.try_recv().unwrap(); // Key
    let _ = cmd_rx.try_recv().unwrap(); // Surrounding
    let _ = cmd_rx.try_recv().unwrap(); // CursorRect
    let _ = cmd_rx.try_recv().unwrap(); // FocusOut
}

// ── 测试辅助 ────────────────────────────────────────────────────

// 简化版的"伪 ibus FromWorker"发射器（用 commit 通道模拟）
fn ev_tx_tx_commit_preedit(
    ev_tx: &mpsc::Sender<crate::host_bridge::dbus_ibus::FromWorker>,
    text: &str,
    is_preedit: bool,
    is_commit: bool,
) {
    if is_preedit {
        let _ = ev_tx.send(crate::host_bridge::dbus_ibus::FromWorker::Preedit {
            text: text.to_string(),
            cursor_begin: text.chars().count() as i32,
            cursor_end: text.chars().count() as i32,
            clear: text.is_empty(),
        });
    }
    if is_commit {
        let _ = ev_tx.send(crate::host_bridge::dbus_ibus::FromWorker::Commit(text.to_string()));
    }
}

fn ev_tx_tx_done(ev_tx: &mpsc::Sender<crate::host_bridge::dbus_ibus::FromWorker>) {
    let _ = ev_tx.send(crate::host_bridge::dbus_ibus::FromWorker::Done(0));
}
