//! 输入法子系统集成测试 —— 真·Wayland 线缆级验证。
//!
//! 服务端跑完整的 [`crate::WLCState`]（无 GPU 模式，跳过 dmabuf），
//! 客户端用真实 `wayland-client` 通过 Unix socket 连接：
//! - **editor**：wl_compositor + surface + zwp_text_input_v3（模拟 GTK/Qt 应用）
//! - **ime**：zwp_input_method_v2（模拟 fcitx5）
//!
//! 覆盖场景：enable 激活、拼音逐键组合、退格、候选选定提交、
//! 选区删除重组、过期 serial 丢弃、焦点切换、enable/disable 循环、
//! 键盘 grab 路由、穿透入站事件应用。

#![cfg(test)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::wayland_server::{
    backend::{ClientData, ClientId, DisconnectReason},
    Display,
};

use crate::seat::KeyboardAction;
// use crate::system_ime::HostEvent; // C 方案：穿透已删
use crate::WLCState;

// ═══════════════════════════ 服务端 ═══════════════════════════

struct ServerClientData;

impl ClientData for ServerClientData {
    fn initialized(&self, _id: ClientId) {}
    fn disconnected(&self, _id: ClientId, _reason: DisconnectReason) {}
}

fn server_setup() -> (Display<WLCState>, WLCState) {
    let display: Display<WLCState> = Display::new().unwrap();
    // 无 GPU：EGL 传 None，dmabuf 全局跳过（这正是 Option 化的目的）。
    let mut state = WLCState::new(display.handle(), None);
    state.socket = "ime-test".into();
    (display, state)
}

// ═══════════════════════════ 客户端 ═══════════════════════════

mod clients {
    use std::os::unix::net::UnixStream;
    use wayland_client::{
        delegate_noop,
        protocol::{wl_compositor, wl_registry, wl_seat, wl_surface},
        Connection, Dispatch, Proxy, QueueHandle,
    };
    use wayland_protocols::wp::text_input::zv3::client::{
        zwp_text_input_manager_v3::{self as mgr3c, ZwpTextInputManagerV3},
        zwp_text_input_v3::{self as ti3c, ZwpTextInputV3},
    };
    use wayland_protocols_misc::zwp_input_method_v2::client::{
        zwp_input_method_keyboard_grab_v2::{self as grabc, ZwpInputMethodKeyboardGrabV2},
        zwp_input_method_manager_v2::{self as mgr2c, ZwpInputMethodManagerV2},
        zwp_input_method_v2::{self as im2c, ZwpInputMethodV2},
    };

    /// 编辑器类客户端状态（text-input-v3 消费方）。
    pub struct EditorState {
        pub ti_events: Vec<String>,
        /// 收到的 done serial 列表。
        pub dones: Vec<u32>,
        pub key_events: Vec<(u32, u8)>,
        pub compositor: Option<wl_compositor::WlCompositor>,
        pub seat: Option<wl_seat::WlSeat>,
        pub surface: Option<wl_surface::WlSurface>,
        pub ti: Option<ZwpTextInputV3>,
        pub manager: Option<ZwpTextInputManagerV3>,
        _keyboard: Option<wayland_client::protocol::wl_keyboard::WlKeyboard>,
    }

    impl Default for EditorState {
        fn default() -> Self {
            Self {
                ti_events: vec![],
                dones: vec![],
                key_events: vec![],
                compositor: None,
                seat: None,
                surface: None,
                ti: None,
                manager: None,
                _keyboard: None,
            }
        }
    }

    impl EditorState {
        /// 把 ti3 事件序列化成便于断言的标记串。
        fn record(&mut self, ev: &ti3c::Event) {
            match ev {
                ti3c::Event::Enter { .. } => self.ti_events.push("enter".into()),
                ti3c::Event::Leave { .. } => self.ti_events.push("leave".into()),
                ti3c::Event::PreeditString { text, cursor_begin, cursor_end } => self
                    .ti_events
                    .push(format!(
                        "preedit({:?},{cursor_begin},{cursor_end})",
                        text.clone().unwrap_or_default()
                    )),
                ti3c::Event::DeleteSurroundingText { .. } => {
                    self.ti_events.push("delete".into())
                }
                ti3c::Event::CommitString { text } => {
                    self.ti_events
                        .push(format!("commit({:?})", text.clone().unwrap_or_default()))
                }
                ti3c::Event::Done { serial } => self.dones.push(*serial),
                _ => {}
            }
        }

    }

    impl Dispatch<wl_registry::WlRegistry, ()> for EditorState {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _data: &(),
            _conn: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { name, interface, version } = event {
                match interface.as_str() {
                    "wl_compositor" => {
                        state.compositor =
                            Some(registry.bind(name, version.min(5), qh, ()));
                    }
                    "wl_seat" => {
                        state.seat = Some(registry.bind(name, version.min(8), qh, ()));
                    }
                    "zwp_text_input_manager_v3" => {
                        state.manager =
                            Some(registry.bind(name, version.min(1), qh, ()));
                    }
                    _ => {}
                }
            }
        }
    }

    impl Dispatch<wl_compositor::WlCompositor, ()> for EditorState {
        fn event(
            _state: &mut Self,
            _p: &wl_compositor::WlCompositor,
            _e: wl_compositor::Event,
            _d: &(),
            _c: &Connection,
            _q: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_surface::WlSurface, ()> for EditorState {
        fn event(
            _state: &mut Self,
            _p: &wl_surface::WlSurface,
            _e: wl_surface::Event,
            _d: &(),
            _c: &Connection,
            _q: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_seat::WlSeat, ()> for EditorState {
        fn event(
            _state: &mut Self,
            _p: &wl_seat::WlSeat,
            _e: wl_seat::Event,
            _d: &(),
            _c: &Connection,
            _q: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwpTextInputManagerV3, ()> for EditorState {
        fn event(
            _s: &mut Self, _p: &ZwpTextInputManagerV3, _e: mgr3c::Event,
            _d: &(), _c: &Connection, _q: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwpTextInputV3, ()> for EditorState {
        fn event(
            state: &mut Self,
            _ti: &ZwpTextInputV3,
            event: ti3c::Event,
            _d: &(),
            _c: &Connection,
            _q: &QueueHandle<Self>,
        ) {
            state.record(&event);
        }
    }

    impl Dispatch<wayland_client::protocol::wl_keyboard::WlKeyboard, ()> for EditorState {
        fn event(
            state: &mut Self,
            _kb: &wayland_client::protocol::wl_keyboard::WlKeyboard,
            event: wayland_client::protocol::wl_keyboard::Event,
            _d: &(),
            _c: &Connection,
            _q: &QueueHandle<Self>,
        ) {
            if let wayland_client::protocol::wl_keyboard::Event::Key { key, state: ks, .. } = event
            {
                if let wayland_client::WEnum::Value(v) = ks {
                    let pressed = v == wayland_client::protocol::wl_keyboard::KeyState::Pressed;
                    state.key_events.push((key, pressed as u8));
                }
            }
        }
    }

    // ── 输入法客户端（模拟 fcitx5）──

    pub struct ImeClientState {
        pub events: Vec<String>,
        pub grab_keys: Vec<(u32, u8)>,
        pub manager: Option<ZwpInputMethodManagerV2>,
        pub seat: Option<wl_seat::WlSeat>,
        pub im: Option<ZwpInputMethodV2>,
        pub _grab: Option<ZwpInputMethodKeyboardGrabV2>,
    }

    impl Default for ImeClientState {
        fn default() -> Self {
            Self {
                events: vec![],
                grab_keys: vec![],
                manager: None,
                seat: None,
                im: None,
                _grab: None,
            }
        }
    }

    impl ImeClientState {
        fn record(&mut self, ev: &im2c::Event) {
            match ev {
                im2c::Event::Activate => self.events.push("activate".into()),
                im2c::Event::Deactivate => self.events.push("deactivate".into()),
                im2c::Event::SurroundingText { text, cursor, anchor } => self
                    .events
                    .push(format!("surrounding({text:?},{cursor},{anchor})")),
                im2c::Event::TextChangeCause { cause } => {
                    self.events.push(format!("cause({cause:?})"))
                }
                im2c::Event::ContentType { hint, purpose } => {
                    self.events.push(format!("content({hint:?},{purpose:?})"))
                }
                im2c::Event::Done => self.events.push("done".into()),
                im2c::Event::Unavailable => self.events.push("unavailable".into()),
                _ => {}
            }
        }
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for ImeClientState {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _data: &(),
            _conn: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { name, interface, version } = event {
                match interface.as_str() {
                    "wl_seat" => {
                        state.seat = Some(registry.bind(name, version.min(8), qh, ()));
                    }
                    "zwp_input_method_manager_v2" => {
                        state.manager =
                            Some(registry.bind(name, version.min(1), qh, ()));
                    }
                    _ => {}
                }
            }
        }
    }

    impl Dispatch<ZwpInputMethodManagerV2, ()> for ImeClientState {
        fn event(
            _s: &mut Self, _p: &ZwpInputMethodManagerV2, _e: mgr2c::Event,
            _d: &(), _c: &Connection, _q: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwpInputMethodV2, ()> for ImeClientState {
        fn event(
            state: &mut Self,
            _im: &ZwpInputMethodV2,
            event: im2c::Event,
            _d: &(),
            _c: &Connection,
            _q: &QueueHandle<Self>,
        ) {
            state.record(&event);
        }
    }

    impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for ImeClientState {
        fn event(
            state: &mut Self,
            _g: &ZwpInputMethodKeyboardGrabV2,
            event: grabc::Event,
            _d: &(),
            _c: &Connection,
            _q: &QueueHandle<Self>,
        ) {
            match event {
                grabc::Event::Keymap { .. } => {}
                grabc::Event::Key { key, state: ks, .. } => {
                    let pressed = matches!(ks, wayland_client::WEnum::Value(v) if
                        v == wayland_client::protocol::wl_keyboard::KeyState::Pressed);
                    state.grab_keys.push((key, pressed as u8));
                }
                grabc::Event::Modifiers { .. } => {}
                grabc::Event::RepeatInfo { .. } => {}
                _ => {}
            }
        }
    }

    impl Dispatch<wl_seat::WlSeat, ()> for ImeClientState {
        fn event(
            _s: &mut Self, _p: &wl_seat::WlSeat, _e: wl_seat::Event,
            _d: &(), _c: &Connection, _q: &QueueHandle<Self>,
        ) {
        }
    }

    /// 测试端客户端句柄。
    pub struct TestClient<S> {
        pub conn: Connection,
        queue: wayland_client::EventQueue<S>,
        pub state: S,
    }

    impl<S: 'static> TestClient<S>
    where
        S: Dispatch<wl_registry::WlRegistry, ()>
            + Dispatch<wl_seat::WlSeat, ()>
            + Default,
    {
        pub fn connect(stream: UnixStream) -> Self {
            let conn = Connection::from_socket(stream).unwrap();
            let queue = conn.new_event_queue();
            let qh = queue.handle();
            conn.display().get_registry(&qh, ());
            Self {
                conn,
                queue,
                state: S::default(),
            }
        }

        pub fn flush(&self) {
            self.queue.flush().unwrap();
        }

        pub fn dispatch_pending(&mut self) {
            if let Some(guard) = self.queue.prepare_read() {
                let _ = guard.read();
            }
            self.queue.dispatch_pending(&mut self.state).unwrap();
        }

        pub fn roundtrip(&mut self) {
            self.queue.roundtrip(&mut self.state).unwrap();
        }

        pub fn qh(&self) -> QueueHandle<S> {
            self.queue.handle()
        }
    }
}

use clients::{EditorState, ImeClientState, TestClient};

/// 编辑器客户端的初始化流程：bind → 建 surface → 建 text_input。
/// 注意：roundtrip 会永久阻塞（服务端由本测试手动驱动），全程用交替驱动。
fn editor_connect(
    stream: UnixStream,
    display: &mut Display<WLCState>,
    server: &mut WLCState,
) -> TestClient<EditorState> {
    let mut c: TestClient<EditorState> = TestClient::connect(stream);
    // 阶段 1：完成 registry/bind 握手。
    for _ in 0..3 {
        c.flush();
        display.dispatch_clients(server).unwrap();
        display.flush_clients().unwrap();
        c.dispatch_pending();
    }
    let qh = c.qh();
    let surface = c.state.compositor.as_ref().unwrap().create_surface(&qh, ());
    c.state.surface = Some(surface);
    let ti = c
        .state
        .manager
        .as_ref()
        .unwrap()
        .get_text_input(c.state.seat.as_ref().unwrap(), &qh, ());
    c.state.ti = Some(ti);
    c.flush();
    display.dispatch_clients(server).unwrap();
    display.flush_clients().unwrap();
    c.dispatch_pending();
    c
}

/// 驱动一轮完整的双向事件交换。
fn drive(
    display: &mut Display<WLCState>,
    server: &mut WLCState,
    editor: &mut TestClient<EditorState>,
    ime: &mut TestClient<ImeClientState>,
) {
    editor.flush();
    ime.flush();
    display.dispatch_clients(server).unwrap();
    display.flush_clients().unwrap();
    editor.dispatch_pending();
    ime.dispatch_pending();
}

struct Fixture {
    display: Display<WLCState>,
    server: WLCState,
    editor: TestClient<EditorState>,
    ime: TestClient<ImeClientState>,
    /// 编辑器客户端在服务端的 Client 句柄（用于反查服务端 WlSurface 对象）。
    editor_client: smithay::reexports::wayland_server::Client,
}

fn setup() -> Fixture {
    setup_with_ime(true)
}

/// `with_ime=false` 时不创建游戏内 input_method 对象 —— 用于穿透端点测试
/// （InProcess 端点优先级高于穿透，二者并存时命令走 im2）。
fn setup_with_ime(with_ime: bool) -> Fixture {
    let (mut display, mut server) = server_setup();

    let (es, ec) = UnixStream::pair().unwrap();
    let (is, ic) = UnixStream::pair().unwrap();
    let editor_client = display
        .handle()
        .insert_client(es, Arc::new(ServerClientData))
        .unwrap();
    display
        .handle()
        .insert_client(is, Arc::new(ServerClientData))
        .unwrap();

    let mut editor = editor_connect(ec, &mut display, &mut server);
    let mut ime: TestClient<ImeClientState> = TestClient::connect(ic);

    // 握手：让双方完成 registry/bind。
    for _ in 0..4 {
        editor.flush();
        ime.flush();
        display.dispatch_clients(&mut server).unwrap();
        display.flush_clients().unwrap();
        editor.dispatch_pending();
        ime.dispatch_pending();
    }

    // ime 创建 input_method 对象。
    if with_ime {
        let qh = ime.qh();
        let im = ime
            .state
            .manager
            .as_ref()
            .unwrap()
            .get_input_method(ime.state.seat.as_ref().unwrap(), &qh, ());
        ime.state.im = Some(im);
        editor.flush();
        ime.flush();
        display.dispatch_clients(&mut server).unwrap();
        display.flush_clients().unwrap();
        editor.dispatch_pending();
        ime.dispatch_pending();
    }

    Fixture {
        display,
        server,
        editor,
        ime,
        editor_client,
    }
}

impl Fixture {
    fn drive(&mut self) {
        drive(&mut self.display, &mut self.server, &mut self.editor, &mut self.ime);
    }

    /// 服务端侧反查编辑器客户端的 WlSurface 对象。
    fn server_surface(&self) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        use wayland_client::Proxy;
        let proto_id = self.editor.state.surface.as_ref().unwrap().id().protocol_id();
        self.server_surface_n(proto_id)
    }

    /// 编辑器聚焦 + enable（标准会话开始）。
    fn focus_and_enable(&mut self) {
        let surface = self.server_surface();
        self.server.ime.set_focus(&surface);
        self.server.seat.keyboard_focus(surface);
        let ti = self.editor.state.ti.as_ref().unwrap();
        ti.enable();
        ti.commit();
        self.drive();
    }

    fn state_ti_for_surface_b(
        &self,
        qh: &wayland_client::QueueHandle<EditorState>,
        _surface: wayland_client::protocol::wl_surface::WlSurface,
    ) -> wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::ZwpTextInputV3 {
        self.editor
            .state
            .manager
            .as_ref()
            .unwrap()
            .get_text_input(self.editor.state.seat.as_ref().unwrap(), qh, ())
    }

    /// 服务端侧反查编辑器客户端的第 n 个 surface.
    fn server_surface_n(&self, proto_id: u32) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
        self.editor_client
            .object_from_protocol_id::<WlSurface>(&self.display.handle(), proto_id)
            .unwrap()
    }
}

// ═══════════════════════════ 场景测试 ═══════════════════════════

/// enable → IME 收到 activate + done；编辑器随后的 done serial 正确。
#[test]
fn test_enable_activates_ime() {
    let mut f = setup();
    f.focus_and_enable();

    assert_eq!(
        f.ime.state.events,
        vec!["activate", "done"],
        "IME 应收到一次 activate 和一次 done"
    );
    assert!(f.editor.state.ti_events.contains(&"enter".to_string()));
}

/// 拼音逐键组合：preedit 多步演进 → 选定候选 → commit「你好」。
#[test]
fn test_pinyin_composition_commit() {
    let mut f = setup();
    f.focus_and_enable();
    f.ime.state.events.clear();

    let im = f.ime.state.im.as_ref().unwrap().clone();
    // 组合期间合成器不发新 done → IME 回填 serial=1（Activate 后计数）。
    for s in ["n", "ni", "nih", "niha", "nihao"] {
        im.set_preedit_string(s.to_string(), s.len() as i32, s.len() as i32);
        im.commit(1);
        f.drive();
    }
    // 每步编辑器都应看到完整批次：preedit + done。
    assert_eq!(
        f.editor.state.ti_events,
        vec![
            "enter",
            "preedit(\"n\",1,1)",
            "preedit(\"ni\",2,2)",
            "preedit(\"nih\",3,3)",
            "preedit(\"niha\",4,4)",
            "preedit(\"nihao\",5,5)",
        ]
    );
    // done 的 serial = 编辑器发出的 commit 请求数（enable 时 1 次）。
    assert!(f.editor.state.dones.iter().all(|&s| s == 1));

    // 选定候选「你好」：清空 preedit + 提交字符串同批原子落地。
    f.editor.state.ti_events.clear();
    im.set_preedit_string(String::new(), 0, 0);
    im.commit_string("你好".to_string());
    im.commit(1);
    f.drive();
    assert_eq!(
        f.editor.state.ti_events,
        vec!["preedit(\"\",0,0)", "commit(\"你好\")"],
        "同一批次的 preedit 清空与 commit 必须一起到达"
    );
    assert_eq!(f.editor.state.dones.len(), 6);
}

/// 组合中退格：preedit 从「niha」缩回「nih」。
#[test]
fn test_backspace_during_composition() {
    let mut f = setup();
    f.focus_and_enable();
    let im = f.ime.state.im.as_ref().unwrap().clone();

    im.set_preedit_string("niha".into(), 4, 4);
    im.commit(1);
    f.drive();

    f.editor.state.ti_events.clear();
    im.set_preedit_string("nih".into(), 3, 3);
    im.commit(1);
    f.drive();

    assert_eq!(
        f.editor.state.ti_events,
        vec!["preedit(\"nih\",3,3)"],
        "退格应体现为缩短后的新 preedit"
    );
}

/// 组合期间移动光标：preedit 光标偏移变化透传。
#[test]
fn test_cursor_move_within_preedit() {
    let mut f = setup();
    f.focus_and_enable();
    let im = f.ime.state.im.as_ref().unwrap().clone();

    im.set_preedit_string("nihao".into(), 2, 2);
    im.commit(1);
    f.drive();
    assert!(
        f.editor.state.ti_events.contains(&"preedit(\"nihao\",2,2)".to_string()),
        "preedit 内光标位置必须透传"
    );

    // 移到中间某处再选区一段（cursor_begin != cursor_end）
    f.editor.state.ti_events.clear();
    im.set_preedit_string("nihao".into(), 1, 4);
    im.commit(1);
    f.drive();
    assert!(f
        .editor
        .state
        .ti_events
        .contains(&"preedit(\"nihao\",1,4)".to_string()));
}

/// 选区重组：delete_surrounding + commit 同批保序到达。
#[test]
fn test_delete_surrounding_then_commit_order() {
    let mut f = setup();
    f.focus_and_enable();
    let im = f.ime.state.im.as_ref().unwrap().clone();

    // App 先上报 surrounding（选中文本「世界」）。
    let ti = f.editor.state.ti.clone().unwrap();
    ti.set_surrounding_text("你好世界！".into(), 6, 6);
    ti.commit();
    f.drive();
    // 状态推送：IME 应收到 surrounding + content/done 批次。
    assert!(
        f.ime
            .state
            .events
            .iter()
            .any(|e| e.starts_with("surrounding(")),
        "app 状态应反向同步给输入法"
    );

    // IME 请求删除前 2 字符并替换为新文本。
    // 注意：上面的 app 状态推送发过一次 done（Activate=1 + PushState=2），
    // 因此本次提交回填 serial=2。
    f.editor.state.ti_events.clear();
    im.delete_surrounding_text(2, 0);
    im.commit_string("世界".to_string());
    im.commit(2);
    f.drive();
    let idx_delete = f
        .editor
        .state
        .ti_events
        .iter()
        .position(|e| e == "delete")
        .expect("应有 delete_surrounding");
    let idx_commit = f
        .editor
        .state
        .ti_events
        .iter()
        .position(|e| e == "commit(\"世界\")")
        .expect("应有 commit");
    assert!(
        idx_delete < idx_commit,
        "delete 必须先于 commit 到达（协议固定应用次序）"
    );
}

/// 过期 serial：IME 用错误 serial 提交 → 整批丢弃，编辑器零事件。
#[test]
fn test_stale_serial_discards_batch() {
    let mut f = setup();
    f.focus_and_enable();
    let im = f.ime.state.im.as_ref().unwrap().clone();

    f.editor.state.ti_events.clear();
    f.editor.state.dones.clear();
    // 当前计数是 1（Activate 发过一次 done），用 99 显然过期。
    im.commit_string("不应出现".to_string());
    im.set_preedit_string("ghost".into(), 0, 5);
    im.commit(99);
    f.drive();

    assert!(
        f.editor.state.ti_events.is_empty(),
        "丢弃批次的任何事件都不允许到达 app"
    );
    assert!(f.editor.state.dones.is_empty());

    // IME 以正确 serial 重发 → 正常应用（协议：照常处理后续请求）。
    im.commit_string("你好".to_string());
    im.commit(1);
    f.drive();
    assert_eq!(f.editor.state.ti_events, vec!["commit(\"你好\")"]);
}

/// 焦点切换：A surface → 无焦点 → deactivate + leave；重新聚焦可恢复。
#[test]
fn test_focus_transitions() {
    let mut f = setup();
    f.focus_and_enable();

    // 失焦 → IME deactivate、编辑器 leave。
    f.server.ime.clear_focus();
    f.server.seat.keyboard_unfocus();
    f.drive();
    assert!(
        f.ime.state.events.contains(&"deactivate".to_string()),
        "失焦必须通知输入法"
    );
    assert!(f.editor.state.ti_events.contains(&"leave".to_string()));

    // 失焦后 IME 再发操作 → 不允许落到任何 app。
    let before = f.editor.state.ti_events.len();
    let im = f.ime.state.im.as_ref().unwrap().clone();
    im.commit_string("迟到的".to_string());
    im.commit(1);
    f.drive();
    assert_eq!(
        f.editor.state.ti_events.len(),
        before,
        "失焦后 IME 操作必须被拒绝"
    );

    // 重新聚焦 + enable → 新会话正常工作。
    f.ime.state.events.clear();
    f.focus_and_enable();
    assert!(f.ime.state.events.contains(&"activate".to_string()));
}

/// enable → disable → enable 循环，done 计数持续正确递增。
#[test]
fn test_enable_disable_enable_cycle() {
    let mut f = setup();
    f.focus_and_enable();

    let ti = f.editor.state.ti.clone().unwrap();
    ti.disable();
    ti.commit();
    f.drive();
    assert!(f.ime.state.events.contains(&"deactivate".to_string()));

    f.ime.state.events.clear();
    ti.enable();
    ti.commit();
    f.drive();
    assert_eq!(
        f.ime.state.events.first().map(String::as_str),
        Some("activate"),
        "重新 enable 后再次激活"
    );

    // 第二个会话里的组合仍要正常落地（serial 基准已随 Deactivate done 递增）。
    let im = f.ime.state.im.as_ref().unwrap().clone();
    // 计数轨迹：Activate=1 → Deactivate=2 → Activate=3。IME 回填最新值 3。
    im.commit_string("好".to_string());
    im.commit(3);
    f.drive();
    assert!(
        f.editor.state.ti_events.contains(&"commit(\"好\")".to_string()),
        "循环后的组合输入必须正常"
    );
}

/// 键盘 grab：grab 期间原始按键只发给 IME；释放后恢复普通路由。
#[test]
fn test_keyboard_grab_routing() {
    let mut f = setup();
    f.focus_and_enable();

    // IME 抓键盘。
    let im = f.ime.state.im.as_ref().unwrap().clone();
    let qh = f.ime.qh();
    let grab = im.grab_keyboard(&qh, ());
    f.ime.state._grab = Some(grab);
    f.drive();

    // grab 存在时按键改道 IME。
    let mods = f.server.seat.modifiers_tuple();
    let handled = f.server.ime.handle_key(30, KeyboardAction::Press, mods);
    assert!(handled, "grab 存在时 handle_key 必须消费按键");
    f.drive();
    assert_eq!(
        f.ime.state.grab_keys,
        vec![(22, 1)],
        "IME grab 应收到 evdev 键码 30-8=22 的按下事件"
    );

    // 释放 grab → 按键恢复普通路径。
    // 协议细节：grab 对象的销毁必须显式发 release（destructor）；
    // 客户端 drop proxy 只销毁本地代理，服务端对象仍在。
    if let Some(g) = f.ime.state._grab.take() {
        g.release();
    }
    f.drive();
    let handled = f.server.ime.handle_key(30, KeyboardAction::Press, mods);
    assert!(!handled, "grab 释放后 handle_key 应返回未消费");
}

/// 穿透入站：宿主事件（含 delete+commit 保序批次与 done）正确应用到编辑器。
/// 焦点在两个输入框间直接切换（A→B，不经过空焦点）：
/// 旧会话必须终结，B 的 enable 必须重新激活 IME，组合落在 B 上。
#[test]
fn test_direct_focus_switch_a_to_b() {
    let mut f = setup();

    // 编辑器创建第二个 surface（输入框 B），并为其创建独立 text_input 实例。
    let qh = f.editor.qh();
    let surface_b = f
        .editor
        .state
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let ti_b = f
        .state_ti_for_surface_b(&qh, surface_b.clone());
    f.drive();

    // ── 输入框 A：聚焦 + enable + 组合 ──
    f.focus_and_enable();
    let im = f.ime.state.im.as_ref().unwrap().clone();
    im.set_preedit_string("ni".into(), 2, 2);
    im.commit(1);
    f.drive();
    assert!(f.editor.state.ti_events.contains(&"preedit(\"ni\",2,2)".to_string()));

    // ── 直接切到输入框 B：不经过空焦点 ──
    f.ime.state.events.clear();
    use wayland_client::Proxy;
    let b_proto = surface_b.id().protocol_id();
    let sb = f.server_surface_n(b_proto);
    f.server.ime.set_focus(&sb);
    f.server.seat.keyboard_focus(sb);
    f.drive();

    // B 上 enable：必须触发 IME 重新激活（deactivate → activate）。
    ti_b.enable();
    ti_b.commit();
    f.drive();

    let evs = &f.ime.state.events;
    let pos_de = evs.iter().position(|e| e == "deactivate")
        .expect("A 的会话应先被终结");
    let pos_act = evs.iter().position(|e| e == "activate")
        .expect("B 的 enable 应重新激活 IME");
    assert!(
        pos_de < pos_act,
        "必须先 deactivate 旧会话再 activate 新会话，实际: {evs:?}"
    );

    // 组合落到 B：IME 正常提交，B 的 text_input 收到事件。
    f.editor.state.ti_events.clear();
    // 计数轨迹：Activate=1 → Deactivate=2 → Activate=3。
    im.set_preedit_string("hao".into(), 3, 3);
    im.commit(3);
    f.drive();
    assert!(
        f.editor.state.ti_events.contains(&"preedit(\"hao\",3,3)".to_string()),
        "切换后组合应落在新的激活实例上"
    );
}

/// host_bridge 上行 UpEvent 灌入 relay：commit 推到嵌套应用 ti3。
#[test]
fn host_bridge_commit_propagates_to_ti3() {
    use crate::ime::{Commit, Done, UpEvent};
    let mut f = setup_with_ime(false); // 无 im2 grab
    f.focus_and_enable();

    // 模拟 host_bridge 上行：commit "你" + Done
    f.server
        .ime
        .apply_up_events(vec![UpEvent::Commit(Commit { text: "你".into() }), UpEvent::Done(Done { batch_id: 1 })]);

    f.display.flush_clients().unwrap();
    f.editor.dispatch_pending();

    assert!(
        f.editor.state.ti_events.contains(&"commit(\"你\")".to_string()),
        "commit 应推到 firefox ti3 text_input；实际: {:?}",
        f.editor.state.ti_events
    );
}

/// host_bridge 上行 Preedit + Done 推到 ti3（preedit 显示在 firefox 文本框）。
#[test]
fn host_bridge_preedit_propagates_to_ti3() {
    use crate::ime::{Done, PreeditUpdate, UpEvent};
    let mut f = setup_with_ime(false);
    f.focus_and_enable();

    f.server.ime.apply_up_events(vec![
        UpEvent::Preedit(PreeditUpdate::set("年", 0, 1)),
        UpEvent::Done(Done { batch_id: 1 }),
    ]);

    f.display.flush_clients().unwrap();
    f.editor.dispatch_pending();

    assert!(
        f.editor.state.ti_events.contains(&"preedit(\"年\",0,1)".to_string()),
        "preedit 应推到 ti3；实际: {:?}",
        f.editor.state.ti_events
    );
}

/// LookupTable 在 host_bridge 路径上**被忽略**（mod 不自绘候选窗）。
/// 这是 C 方案决策：候选窗由宿主 IME 框架（kimpanel/ibus panel/GNOME）画。
#[test]
fn host_bridge_lookup_table_is_ignored() {
    use crate::ime::{Done, LookupTable, UpEvent};
    let mut f = setup_with_ime(false);
    f.focus_and_enable();
    let baseline = f.editor.state.ti_events.len();

    f.server.ime.apply_up_events(vec![
        UpEvent::LookupTable(LookupTable {
            candidates: vec!["一".into()],
            labels: vec![],
            cursor_pos: 0,
            cursor_visible: true,
            page_size: 9,
            orientation: 0,
            visible: true,
        }),
        UpEvent::Done(Done { batch_id: 1 }),
    ]);

    f.display.flush_clients().unwrap();
    f.editor.dispatch_pending();

    // LookupTable 不应产生任何**新** wire 事件（相对于 baseline）
    let new_events: Vec<_> = f.editor.state.ti_events.iter().skip(baseline).collect();
    assert!(
        new_events.is_empty(),
        "LookupTable 在 host_bridge 路径不应推送到 ti3；新增: {new_events:?}"
    );
}

/// host_bridge 在 app_active=false 时收到事件 → flush NOT applied（保留缓冲）。
#[test]
fn host_bridge_events_buffered_when_app_inactive() {
    use crate::ime::{Commit, Done, UpEvent};
    let mut f = setup_with_ime(false);
    // 不调 focus_and_enable —— app_active=false

    f.server
        .ime
        .apply_up_events(vec![UpEvent::Commit(Commit { text: "你".into() }), UpEvent::Done(Done { batch_id: 1 })]);

    // Done 触发 flush，但 app_active=false → 不应用
    // buffered 保留
    f.display.flush_clients().unwrap();
    f.editor.dispatch_pending();
    assert!(
        f.editor.state.ti_events.is_empty(),
        "app_active=false 时 commit 不应推到 ti3"
    );
}
