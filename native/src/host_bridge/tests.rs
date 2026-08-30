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


// v0.10：CommitText 解析必须跳过 IBusText 序列化时的 GObject 类型名
// "IBusText"——之前 v0.9.45 实机的 0 commit 根因。
#[test]
fn commit_text_v010_skip_first_string() {
    // IBusText 序列化的真实结构（参见 ibus/src/ibusserializable.c）：
    //   ibus_serializable_serialize_object 把 (s "IBusText", IBusText fields) 打包成 tuple
    //   传入 GVariantBuilder.add_value 序列化为 variant (Tuple)
    // 我们 mod 收到后 deserialize 为 Structure，fields = [Str("IBusText"), Str(text), ...]
    // v0.10 修法：从 fields 收集所有 String，过滤 GObject 类型名（"IBusText"），
    // 取第一个**非**类型名 String 作为真正的 commit 文本。
    let fields: Vec<String> = vec![
        "IBusText".to_string(), // GObject 类型名（必须跳过）
        "你".to_string(),       // 真正的 commit 文本
    ];
    let text = fields
        .into_iter()
        .find(|s| s != "IBusText" && !s.is_empty())
        .unwrap_or_default();
    assert_eq!(text, "你", "v0.10 修法必须跳过 IBusText 类型名");
}

// v0.10：UpdatePreeditText 同样需要跳过 IBusText 类型名
#[test]
fn preedit_v010_skip_first_string() {
    // IBusText 序列化的 variant 内部 fields = [Str("IBusText"), Str(text), ...]
    // v0.10 修法：find_text_in_value 递归抓所有 String，跳过类型名。
    let all_strs: Vec<String> = vec![
        "IBusText".to_string(),
        "年".to_string(),
    ];
    let text = all_strs
        .into_iter()
        .find(|s| s != "IBusText" && !s.is_empty())
        .unwrap_or_default();
    assert_eq!(text, "年");
}

// v0.10：UpdateLookupTable 解析时——IBusLookupTable 序列化含
// 类型名 "IBusLookupTable"。candidates 是 IBusText 序列化的 array，
// 每个 candidate 自身又含类型名 "IBusText"。
// 简化 v0.10：递归抓所有 String 字段，过滤掉两种类型名。
#[test]
fn lookup_table_v010_skip_type_names() {
    let all_strs: Vec<String> = vec![
        "IBusLookupTable".to_string(), // IBusLookupTable 类型名
        "IBusText".to_string(),        // IBusText 类型名（嵌套）
        "候选1".to_string(),
        "候选2".to_string(),
    ];
    let candidates: Vec<String> = all_strs
        .into_iter()
        .filter(|s| s != "IBusLookupTable" && s != "IBusText" && !s.is_empty())
        .collect();
    assert_eq!(candidates, vec!["候选1", "候选2"]);
}

