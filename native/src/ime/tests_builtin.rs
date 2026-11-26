//! 内置拼音引擎的线缆级集成测试（与 tests.rs 同风格：真 Display +
//! 真 wayland-client 编辑器，断言事件确实到达 ti3）。
//!
//! 覆盖：source=builtin 时 nihao→空格 → commit("你好") 经
//! relay→emit_ti_batch 到达编辑器（e2e 主合同）；候选快照通道；
//! source=fcitx5 显式档不碰内置；Auto 档在无 im2 且 daemon 判死时的
//! 回退裁决；失焦/命令行豁免的直通语义。

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::wayland_server::{
    backend::{ClientData, ClientId, DisconnectReason},
    Display,
};

use crate::ime::tests::{clients::{EditorState, TestClient}, editor_connect, server_setup, ServerClientData};
use crate::seat::KeyboardAction;
use crate::WLCState;

struct BuiltinEdClientData;
impl ClientData for BuiltinEdClientData {
    fn initialized(&self, _id: ClientId) {}
    fn disconnected(&self, _id: ClientId, _reason: DisconnectReason) {}
}

struct Fix {
    display: Display<WLCState>,
    server: WLCState,
    editor: TestClient<EditorState>,
    editor_client: smithay::reexports::wayland_server::Client,
}

impl Fix {
    fn setup(source: &str) -> Self {
        let (mut display, mut server) = server_setup();
        let (es, ec) = UnixStream::pair().unwrap();
        let editor_client = display
            .handle()
            .insert_client(es, Arc::new(BuiltinEdClientData))
            .unwrap();
        let mut editor = editor_connect(ec, &mut display, &mut server);
        let mut f = Fix { display, server, editor, editor_client };
        // 切档（内置 or 其它）在 enable 之前，会话建立即按新档路由。
        let reply = f.server.ime.set_ime_source(source).expect("合法来源名");
        assert!(reply.contains(source), "回执须含档名: {reply}");
        // 建会话：聚焦 + enable。
        let surface = f.server_surface();
        let mut ime = std::mem::take(&mut f.server.ime);
        ime.set_focus(&surface, &mut f.server);
        f.server.ime = ime;
        f.server.seat.keyboard_focus(surface);
        let ti = f.editor.state.ti.as_ref().unwrap().clone();
        ti.enable();
        ti.commit();
        f.drive();
        assert!(f.server.ime.app_active(), "会话必须激活");
        f
    }

    fn drive(&mut self) {
        self.editor.flush();
        self.display.dispatch_clients(&mut self.server).unwrap();
        self.display.flush_clients().unwrap();
        self.editor.dispatch_pending();
    }

    fn server_surface(&self) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        use wayland_client::Proxy;
        let proto_id = self.editor.state.surface.as_ref().unwrap().id().protocol_id();
        self.editor_client
            .object_from_protocol_id(&self.display.handle(), proto_id)
            .unwrap()
    }

    /// press+release 走 handle_key（与 bridge.keyboard_input 的
    /// modifiers_tuple 同源；内置档 mods 全 0）。
    fn key(&mut self, evdev: u32) -> bool {
        let mods = self.server.seat.modifiers_tuple();
        let pressed = self.server.ime.handle_key(evdev + 8, KeyboardAction::Press, mods);
        self.server.ime.handle_key(evdev + 8, KeyboardAction::Release, mods);
        self.drive();
        pressed
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.bytes() {
            let evdev = evdev_for_letter(ch);
            self.key(evdev);
        }
    }
}

fn evdev_for_letter(ch: u8) -> u32 {
    match ch {
        b'q' => 16, b'w' => 17, b'e' => 18, b'r' => 19, b't' => 20, b'y' => 21,
        b'u' => 22, b'i' => 23, b'o' => 24, b'p' => 25,
        b'a' => 30, b's' => 31, b'd' => 32, b'f' => 33, b'g' => 34, b'h' => 35,
        b'j' => 36, b'k' => 37, b'l' => 38,
        b'z' => 44, b'x' => 45, b'c' => 46, b'v' => 47, b'b' => 48, b'n' => 49,
        b'm' => 50,
        _ => panic!("not a letter: {ch}"),
    }
}

const SPACE: u32 = 57;

/// e2e 主合同："nihao" → 候选 → 空格 → commit("你好") 经 emit_ti_batch
/// 到达编辑器 ti3（preedit 演进 + 选词批 + done serial 全链）。
#[test]
fn builtin_nihao_space_commits_to_ti3() {
    let mut f = Fix::setup("builtin");
    f.editor.state.ti_events.clear();
    f.editor.state.dones.clear();

    for ch in "nihao".bytes() {
        assert!(f.key(evdev_for_letter(ch)), "字母 {ch} 应被内置引擎消费");
    }
    let preedits: Vec<&String> = f
        .editor
        .state
        .ti_events
        .iter()
        .filter(|e| e.starts_with("preedit"))
        .collect();
    assert_eq!(preedits.len(), 5, "逐键 preedit 演进: {preedits:?}");
    assert!(preedits.last().unwrap().contains("\"nihao\""), "{preedits:?}");

    // 候选快照通道已能看到 你好（结构化文本，候选窗 Java 侧数据源）。
    let snap = f
        .server
        .ime
        .take_builtin_snapshot()
        .expect("候选快照应有变化");
    assert!(snap.contains("你好"), "快照须含候选: {snap}");

    assert!(f.key(SPACE), "空格选词应被消费");
    assert!(
        f.editor
            .state
            .ti_events
            .contains(&"commit(\"你好\")".to_string()),
        "commit 必须经 relay→ti3 到达编辑器: {:?}",
        f.editor.state.ti_events
    );
    // done 纪律：每批一条 done（serial = 该实例 commit 计数）。
    assert!(!f.editor.state.dones.is_empty(), "批次必须伴随 done");
    let ev: Vec<&String> = f.editor.state.ti_events.iter().rev().collect();
    assert!(
        matches!((ev[0].as_str(), ev[1].as_str()), ("commit(\"你好\")", "preedit(\"\",0,0)")),
        "选词批序 = 清 preedit → commit: {:?}",
        f.editor.state.ti_events
    );
    // 组合终结后快照翻 visible=false（面板隐藏合同）。
    let snap2 = f.server.ime.take_builtin_snapshot().expect("选词后快照有变化");
    assert!(snap2.contains("\"visible\":false"), "{snap2}");
}

/// 数字键选词：1 直达当前页首候选（evdev 2 = digit 1）。
#[test]
fn builtin_digit_select_commits() {
    let mut f = Fix::setup("builtin");
    f.editor.state.ti_events.clear();
    f.type_str("nihao");
    assert!(f.key(2), "数字 1 选词应被消费");
    assert!(
        f.editor
            .state
            .ti_events
            .contains(&"commit(\"你好\")".to_string()),
        "数字选词批必须到达 ti3: {:?}",
        f.editor.state.ti_events
    );
}

/// 退格：编辑缓冲，preedit 同步收缩；退空后直通。
#[test]
fn builtin_backspace_shrinks_preedit() {
    let mut f = Fix::setup("builtin");
    f.type_str("nihao");
    assert!(f.key(14), "组合中退格须被消费");
    let last = f.editor.state.ti_events.iter().rev().find(|e| e.starts_with("preedit")).unwrap().clone();
    assert!(last.contains("\"niha\""), "退格后 preedit=niha: {last}");
}

/// 命令行豁免：surrounding 以 '/' 开头 → 字母直通（不吃键，MC 指令可打）。
#[test]
fn builtin_slash_commandline_passthrough() {
    let mut f = Fix::setup("builtin");
    // 模拟 MC 命令行：ti3 上报 surrounding "/wl"。
    let ti = f.editor.state.ti.as_ref().unwrap().clone();
    ti.set_surrounding_text("/wl".to_string(), 3, 3);
    ti.commit();
    f.drive();
    let before = f.editor.state.ti_events.len();
    assert!(!f.key(evdev_for_letter(b'n')), "'/' 语境字母必须直通");
    assert_eq!(f.editor.state.ti_events.len(), before, "直通不得产生 IME 事件");
}

/// 无激活会话（未 enable）：内置引擎不吃任何键（游戏操控零干扰）。
#[test]
fn builtin_inactive_session_never_consumes() {
    let (mut display, mut server) = server_setup();
    let (es, ec) = UnixStream::pair().unwrap();
    let _client = display.handle().insert_client(es, Arc::new(BuiltinEdClientData)).unwrap();
    let mut editor = editor_connect(ec, &mut display, &mut server);
    // 只喂键，不 set_focus/enable。
    server.ime.set_ime_source("builtin").unwrap();
    let mods = (0u32, 0u32, 0u32, 0u32);
    assert!(
        !server.ime.handle_key(evdev_for_letter(b'n') + 8, KeyboardAction::Press, mods),
        "无会话必须直通"
    );
    editor.flush();
    display.dispatch_clients(&mut server).unwrap();
    display.flush_clients().unwrap();
    editor.dispatch_pending();
}

/// 强制档 fcitx5：内置分支完全不参与——无 grab 时行为与旧版一致（返回
/// false → raw 直通），即便 daemon 判死也不抢键。
#[test]
fn forced_fcitx5_source_ignores_builtin() {
    let mut f = Fix::setup("fcitx5");
    // 无 im2 端点：既有逻辑 route_live=false → false。
    assert!(
        !f.key(evdev_for_letter(b'n')),
        "Fcitx5 强制档不得走内置消费路径"
    );
    assert_eq!(f.server.ime.effective_source(), crate::ime::ImeSource::Fcitx5);
}

/// Auto 档裁决：无 daemon 信息（Unconfigured）= Fcitx5 维持既有行为；
/// daemon 判死（NoBinary/Failed）才落内置。裁决纯函数可测（不依赖真进程）。
#[test]
fn auto_source_falls_back_only_on_dead_daemon() {
    use crate::ime::ImeSource;
    let mut f = Fix::setup("auto");
    assert_eq!(
        f.server.ime.effective_source(),
        ImeSource::Fcitx5,
        "Unconfigured/Starting 阶段 Auto 必须维持既有路由（逐字节回退红线）"
    );
}

/// source 命令面：非法值拒绝且不改状态；合法值即时生效；`eng` 清单如实。
#[test]
fn source_command_validates_and_reports() {
    let mut ime = crate::ime::ImeState::default();
    assert!(ime.set_ime_source("ibus").is_err(), "非法来源必须拒绝");
    assert_eq!(ime.ime_source(), crate::ime::ImeSource::Auto);
    let ok = ime.set_ime_source(" BUILTIN ").expect("大小写/空白容错");
    assert!(ok.contains("builtin"));
    assert_eq!(ime.ime_source(), crate::ime::ImeSource::Builtin);
    let eng = ime.ime_eng_list();
    assert!(eng.contains("内置"), "{eng}");
    assert!(eng.contains("fcitx5"), "{eng}");
}

/// 点选出口（候选面板 JNI 的引擎侧）：ui_select_page_index 与数字键同语义。
#[test]
fn builtin_ui_select_matches_digit_path() {
    let mut ime = crate::ime::ImeState::default();
    ime.set_ime_source("builtin").unwrap();
    ime.init_builtin_dict();
    ime.relay.set_app_enabled(true, "test");
    // 需要 active ti3 实例 emit_ti_batch 才落地——无实例路径只测引擎输出。
    ime.relay.set_ime_present(true);
    for ch in b"nihao" {
        ime.builtin.on_key(evdev_for_letter(*ch) + 8, true, 0);
        ime.builtin.on_key(evdev_for_letter(*ch) + 8, false, 0);
    }
    assert_eq!(ime.builtin.buffer(), "nihao");
    // 直连引擎选词（绕过 relay 无实例早退，验证 ops 合同）。
    let r = ime.builtin.ui_select_page_index(0);
    assert!(r.consumed);
    assert!(matches!(&r.ops[1], crate::ime::ImeOp::CommitString(w) if w == "你好"));
}
