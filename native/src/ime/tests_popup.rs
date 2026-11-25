//! `zwp_input_popup_surface_v2` 线缆级集成测试（Phase 1：角色 + 采集 + 取帧）。
//!
//! 复用 `tests.rs` 的服务端装配（`server_setup`/`editor_connect`），IME 端
//! 换成带 shm + popup + pointer 的"模拟 fcitx5 classicui"客户端：
//!
//! ```text
//! get_input_popup_surface(surface)      ← waylandinputwindow.cpp :195
//!   → 角色登记 + text_input_rectangle 回报
//! shm pool(memfd, ARGB8888) → create_buffer → attach/damage/commit
//!   → CompositorHandler::commit 认领 → 拷帧 → wl_buffer.release
//! Java 轮询 = ImeState::take_popup_snapshot(kb_active)
//!   → PopupSnapshot { w, h, seq, visible, argb }
//! popup.destroy / input_method.destroy / attach(NULL)
//!   → 采集轨道解除（gone_for 按对象 id 匹配）
//! ```
//!
//! 覆盖：建/帧/取/毁 + 可见性门（activate/focus/kb_active）+ 缓冲 release
//! （fcitx5 双缓冲防冻结）+ XRGB8888 强制不透明 + attach(NULL) 隐藏 +
//! 二次赋角色协议错误。

#![cfg(test)]

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::wayland_server::{Client, Display};

use crate::ime::tests::{editor_connect, server_setup, ServerClientData};
use crate::ime::tests::clients::{EditorState, TestClient};
use crate::WLCState;

use wayland_client::{
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_pointer, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};

// ═══════════════════════ 模拟 fcitx5 classicui 客户端 ═══════════════════════

#[derive(Default)]
pub(crate) struct PopupImeState {
    pub im_events: Vec<String>,
    pub rects: Vec<(i32, i32, i32, i32)>,
    /// wl_pointer 事件流水（事件名，Phase 3 断言序列）。
    pub ptr_events: Vec<String>,
    pub buffer_releases: u32,
    pub compositor: Option<wl_compositor::WlCompositor>,
    pub shm: Option<wl_shm::WlShm>,
    pub seat: Option<wl_seat::WlSeat>,
    pub pointer: Option<wl_pointer::WlPointer>,
    pub mgr: Option<ZwpInputMethodManagerV2>,
    pub im: Option<ZwpInputMethodV2>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for PopupImeState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(5), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, version.min(1), qh, ())),
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(8), qh, ())),
                "zwp_input_method_manager_v2" => {
                    state.mgr = Some(registry.bind(name, version.min(1), qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for PopupImeState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_compositor::WlCompositor, ()> for PopupImeState {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_surface::WlSurface, ()> for PopupImeState {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_shm::WlShm, ()> for PopupImeState {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_shm_pool::WlShmPool, ()> for PopupImeState {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_buffer::WlBuffer, ()> for PopupImeState {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.buffer_releases += 1;
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for PopupImeState {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.ptr_events.push(pointer_event_name(&event).to_string());
    }
}
impl Dispatch<ZwpInputMethodManagerV2, ()> for PopupImeState {
    fn event(
        _: &mut Self,
        _: &ZwpInputMethodManagerV2,
        _: zwp_input_method_manager_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<ZwpInputMethodV2, ()> for PopupImeState {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.im_events.push("activate".into()),
            zwp_input_method_v2::Event::Deactivate => state.im_events.push("deactivate".into()),
            zwp_input_method_v2::Event::Done => state.im_events.push("done".into()),
            zwp_input_method_v2::Event::Unavailable => state.im_events.push("unavailable".into()),
            _ => {}
        }
    }
}
impl Dispatch<ZwpInputPopupSurfaceV2, ()> for PopupImeState {
    fn event(
        state: &mut Self,
        _: &ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle { x, y, width, height } =
            event
        {
            state.rects.push((x, y, width, height));
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for PopupImeState {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn pointer_event_name(ev: &wl_pointer::Event) -> &'static str {
    match ev {
        wl_pointer::Event::Enter { .. } => "enter",
        wl_pointer::Event::Leave { .. } => "leave",
        wl_pointer::Event::Motion { .. } => "motion",
        wl_pointer::Event::Button { .. } => "button",
        wl_pointer::Event::Axis { .. } => "axis",
        wl_pointer::Event::Frame => "frame",
        wl_pointer::Event::AxisSource { .. } => "axis_source",
        wl_pointer::Event::AxisDiscrete { .. } => "axis_discrete",
        wl_pointer::Event::AxisStop { .. } => "axis_stop",
        wl_pointer::Event::AxisValue120 { .. } => "axis_value120",
        _ => "other",
    }
}

// ══════════════════════════ 独立 IME 客户端句柄 ══════════════════════════

/// popup 测试专用 IME 客户端（不经 tests.rs::TestClient——协议错误测试需要
/// 拿到 dispatch 的 Err，TestClient 的封装全部 unwrap）。
pub(crate) struct PopupClient {
    pub conn: Connection,
    pub queue: EventQueue<PopupImeState>,
    pub state: PopupImeState,
}

impl PopupClient {
    fn connect(stream: UnixStream) -> Self {
        let conn = Connection::from_socket(stream).unwrap();
        let queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        Self { conn, queue, state: PopupImeState::default() }
    }

    fn flush(&mut self) {
        let _ = self.queue.flush();
    }

    /// 向 server 已关闭的连接写新请求后 flush → 应得到写错误（EPIPE）。
    fn flush_err(&mut self) -> bool {
        self.queue.flush().is_err()
    }

    /// true = 连接仍健康；false = 已收到协议错误（fatal）。
    fn dispatch(&mut self) -> bool {
        if let Some(guard) = self.queue.prepare_read() {
            let _ = guard.read();
        }
        self.queue.dispatch_pending(&mut self.state).is_ok()
    }

    fn qh(&self) -> QueueHandle<PopupImeState> {
        self.queue.handle()
    }
}

// ═══════════════════════════════ 装配 ═══════════════════════════════

pub(crate) struct PopupFixture {
    pub display: Display<WLCState>,
    pub server: WLCState,
    pub editor: TestClient<EditorState>,
    pub ime: PopupClient,
    pub editor_client: Client,
    #[allow(dead_code)]
    pub ime_client: Client,
}

fn setup_popup() -> PopupFixture {
    let (mut display, mut server) = server_setup();

    let (es, ec) = UnixStream::pair().unwrap();
    let (is, ic) = UnixStream::pair().unwrap();
    let editor_client = display.handle().insert_client(es, Arc::new(ServerClientData)).unwrap();
    // popup 客户端走 wl_compositor.create_surface + shm，必须按生产路径挂
    // WLCClient（lib.rs client_compositor_state 要求该 data，否则 unwrap panic，
    // tests_real.rs::Harness 同一约束）。
    let ime_client = display
        .handle()
        .insert_client(is, Arc::new(crate::WLCClient::new()))
        .unwrap();

    let mut editor = editor_connect(ec, &mut display, &mut server);
    let mut ime = PopupClient::connect(ic);
    for _ in 0..4 {
        editor.flush();
        ime.flush();
        display.dispatch_clients(&mut server).unwrap();
        display.flush_clients().unwrap();
        editor.dispatch_pending();
        ime.dispatch();
    }
    let qh = ime.qh();
    let im = ime
        .state
        .mgr
        .as_ref()
        .unwrap()
        .get_input_method(ime.state.seat.as_ref().unwrap(), &qh, ());
    ime.state.im = Some(im);
    // 模拟 classicui WaylandPointer：WaylandUI 在 seat 有 POINTER capability
    // 时 getPointer()（waylandpointer.cpp :20），Phase 3 点选断言依赖。
    ime.state.pointer = Some(ime.state.seat.as_ref().unwrap().get_pointer(&qh, ()));
    PopupFixture { display, server, editor, ime, editor_client, ime_client }
}

impl PopupFixture {
    /// 健康驱动一轮；返回 false = IME client 已被协议错误终结。
    fn drive(&mut self) -> bool {
        self.editor.flush();
        self.ime.flush();
        let dispatched = self.display.dispatch_clients(&mut self.server).is_ok();
        self.display.flush_clients().ok();
        self.editor.dispatch_pending();
        dispatched && self.ime.dispatch()
    }

    /// 编辑器服务端 WlSurface 反查。
    fn editor_server_surface(&self) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        use wayland_client::Proxy;
        let proto = self.editor.state.surface.as_ref().unwrap().id().protocol_id();
        self.editor_client
            .object_from_protocol_id::<
                smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
            >(&self.display.handle(), proto)
            .unwrap()
    }

    /// 编辑器聚焦 + enable：ti3 会话开始 → relay 向 im2 发 activate。
    fn focus_and_enable(&mut self) {
        let surface = self.editor_server_surface();
        let mut ime = std::mem::take(&mut self.server.ime);
        ime.set_focus(&surface, &mut self.server);
        self.server.ime = ime;
        self.server.seat.keyboard_focus(surface);
        let ti = self.editor.state.ti.as_ref().unwrap().clone();
        ti.enable();
        ti.commit();
        self.drive();
    }

    /// 失焦（bridge keyboard_unfocus 同径）。
    fn unfocus(&mut self) {
        let mut ime = std::mem::take(&mut self.server.ime);
        ime.clear_focus(&mut self.server);
        self.server.ime = ime;
        self.drive();
    }
}

// ── shm 缓冲工具（memfd + mmap，模拟 fcitx5 buffer.cpp 的池用法）──

pub(crate) struct ShmBuf {
    pub buffer: wl_buffer::WlBuffer,
    fd: OwnedFd,
    #[allow(dead_code)]
    pool: wl_shm_pool::WlShmPool,
    size: usize,
}

impl ShmBuf {
    /// 新建 size=stride*h 字节的池 + 一个 offset=0 的整池缓冲。
    fn new(
        state: &PopupImeState,
        qh: &QueueHandle<PopupImeState>,
        w: i32,
        h: i32,
        stride: i32,
        format: wl_shm::Format,
    ) -> Self {
        let size = (stride * h) as usize;
        let raw: RawFd = unsafe {
            libc::memfd_create(
                b"wlc-popup-pool\0".as_ptr() as *const libc::c_char,
                1, /* MFD_CLOEXEC */
            )
        };
        assert!(raw >= 0, "memfd_create failed");
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        assert_eq!(unsafe { libc::ftruncate(raw, size as libc::off_t) }, 0, "ftruncate failed");
        let borrowed: BorrowedFd = fd.as_fd();
        let shm = state.shm.as_ref().expect("wl_shm bound");
        let pool = shm.create_pool(borrowed, size as i32, qh, ());
        let buffer = pool.create_buffer(0, w, h, stride, format, qh, ());
        Self { buffer, fd, pool, size }
    }

    /// 自有 mmap 写入整帧像素（与 server 端映射共享同一 memfd 后备）。
    fn write(&self, pixels: &[u8]) {
        assert!(pixels.len() <= self.size);
        unsafe {
            let map = libc::mmap(
                std::ptr::null_mut(),
                self.size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd.as_raw_fd(),
                0,
            );
            assert_ne!(map, libc::MAP_FAILED, "mmap pool");
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), map as *mut u8, pixels.len());
            libc::munmap(map, self.size);
        }
    }
}

/// 像素图案：逐像素确定性 [B^,G^,R^,A=0xFF]（小端 ARGB8888 内存序）。
fn argb_pattern(w: i32, h: i32, seed: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = (x as u8).wrapping_add((y as u8) << 4).wrapping_add(seed);
            out.extend_from_slice(&[v ^ 0x11, v ^ 0x22, v ^ 0x44, 0xffu8]);
        }
    }
    out
}

/// 建 popup + 提交一帧 4x3，返回 fixture 内句柄别名（重复样板收拢）。
fn popup_with_frame(f: &mut PopupFixture, w: i32, h: i32, seed: u8) -> (wl_surface::WlSurface, ZwpInputPopupSurfaceV2, Vec<u8>) {
    let qh = f.ime.qh();
    let surface = f.ime.state.compositor.as_ref().unwrap().create_surface(&qh, ());
    let popup = f.ime.state.im.as_ref().unwrap().get_input_popup_surface(&surface, &qh, ());
    assert!(f.drive());
    let px = argb_pattern(w, h, seed);
    let buf = ShmBuf::new(&f.ime.state, &qh, w, h, w * 4, wl_shm::Format::Argb8888);
    buf.write(&px);
    surface.attach(Some(&buf.buffer), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    surface.commit();
    assert!(f.drive());
    std::mem::forget(buf); // 保持 buffer/fd 存活至测试结束
    (surface, popup, px)
}

// ═══════════════════════════ 场景测试（Phase 1）═══════════════════════════

/// 完整链路：建 popup → attach/commit shm ARGB8888 → 服务端采到帧、
/// Java 轮询可见、缓冲被 release（fcitx5 双缓冲驱动条件）、无变化零拷贝。
#[test]
fn popup_attach_commit_frame_visible_and_released() {
    let mut f = setup_popup();
    f.focus_and_enable();
    assert!(f.server.ime.im2.instance.is_some(), "im2 端点在位");

    let (surface, popup, px) = popup_with_frame(&mut f, 4, 3, 0);
    assert!(f.server.ime.im2.popup.is_some(), "既有协议字段维护不回退");
    assert_eq!(f.ime.state.rects, vec![(0, 0, 0, 0)], "创建即回报占位矩形");

    // 轮询：可见 + 像素逐字节一致 + seq=1。
    let snap = f
        .server
        .ime
        .take_popup_snapshot(true)
        .expect("有新帧状态应交付");
    assert!(snap.visible, "activate+focus+kb → visible=1");
    assert_eq!((snap.width, snap.height), (4, 3));
    assert_eq!(snap.seq, 1);
    assert_eq!(snap.argb.as_ref().expect("可见应有像素"), &px);

    // 再轮询：零拷贝 null。
    assert!(f.server.ime.take_popup_snapshot(true).is_none(), "无变化必须 null");

    // 缓冲必须被 release（否则 fcitx5 双缓冲两帧后冻结）。
    assert!(f.ime.state.buffer_releases >= 1, "commit 的缓冲应收到 wl_buffer.release");

    let _ = (surface, popup);
}

/// 二次 commit 新像素 → seq 推进且内容更新；XRGB8888 → alpha 强制不透明。
#[test]
fn popup_second_frame_updates_seq_and_xrgb_alpha() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let qh = f.ime.qh();
    let surface = f.ime.state.compositor.as_ref().unwrap().create_surface(&qh, ());
    let _popup = f.ime.state.im.as_ref().unwrap().get_input_popup_surface(&surface, &qh, ());
    assert!(f.drive());

    let buf1 = ShmBuf::new(&f.ime.state, &qh, 2, 2, 8, wl_shm::Format::Argb8888);
    let px1 = argb_pattern(2, 2, 0);
    buf1.write(&px1);
    surface.attach(Some(&buf1.buffer), 0, 0);
    surface.damage_buffer(0, 0, 2, 2);
    surface.commit();
    assert!(f.drive());
    let s1 = f.server.ime.take_popup_snapshot(true).expect("第一帧");
    assert_eq!(s1.seq, 1);
    assert_eq!(s1.argb.as_ref().unwrap(), &px1);

    let buf2 = ShmBuf::new(&f.ime.state, &qh, 2, 2, 8, wl_shm::Format::Argb8888);
    let px2 = argb_pattern(2, 2, 7);
    buf2.write(&px2);
    surface.attach(Some(&buf2.buffer), 0, 0);
    surface.damage_buffer(0, 0, 2, 2);
    surface.commit();
    assert!(f.drive());
    let s2 = f.server.ime.take_popup_snapshot(true).expect("第二帧");
    assert_eq!(s2.seq, 2);
    assert_eq!(s2.argb.as_ref().unwrap(), &px2);

    // XRGB8888：alpha 字节全 0，快照里必须强制 0xFF。
    let buf3 = ShmBuf::new(&f.ime.state, &qh, 1, 1, 4, wl_shm::Format::Xrgb8888);
    buf3.write(&[0x11, 0x22, 0x33, 0x00]);
    surface.attach(Some(&buf3.buffer), 0, 0);
    surface.damage_buffer(0, 0, 1, 1);
    surface.commit();
    assert!(f.drive());
    let s3 = f.server.ime.take_popup_snapshot(true).expect("第三帧");
    assert_eq!(s3.argb.as_ref().unwrap().as_slice(), &[0x11, 0x22, 0x33, 0xff]);
}

/// 可见性门：帧先于会话到达 → visible=0；会话激活后可见；失焦→0；
/// 游戏未 focused（kb_active=false）→ 0（协议 activate/deactivate 蕴含
/// popup 显隐的本地落实，v2 无显式 visible 事件）。
#[test]
fn popup_visibility_follows_activate_and_focus() {
    let mut f = setup_popup();
    // 不 focus_and_enable —— 直接建 popup + 提交帧（会话未激活）。
    let (_surface, _popup, px) = popup_with_frame(&mut f, 2, 1, 0);

    // 有帧但无会话 → 交付"不可见"快照（尺寸在、无像素）。
    let s = f.server.ime.take_popup_snapshot(true).expect("帧到达应有状态变化");
    assert!(!s.visible, "无 activate/focus 会话时 popup 不得可见");
    assert_eq!(s.width, 2);
    assert!(s.argb.is_none());
    assert_eq!(s.seq, 1);

    // 会话开始 → Activate（exec_on_im2 touch epoch）→ 再次轮询可见。
    f.focus_and_enable();
    let s = f.server.ime.take_popup_snapshot(true).expect("activate 应推进可见性");
    assert!(s.visible);
    assert_eq!(s.argb.as_ref().unwrap(), &px);

    // 失焦 → clear_focus touch + relay focus_lost Deactivate → 不可见。
    f.unfocus();
    let s = f.server.ime.take_popup_snapshot(true).expect("失焦应推进可见性");
    assert!(!s.visible, "ti3 焦点离开 → 候选窗必须隐藏");

    // 游戏未 focused：重新 focus+enable 后按 keyboard_deactivate 语义
    // （seat.deactivate_keyboard + bridge popup_touch）→ 必须不可见；
    // 重新 activate（bridge keyboard_activate 同径）→ 恢复可见。
    let surface = f.editor_server_surface();
    let mut ime = std::mem::take(&mut f.server.ime);
    ime.set_focus(&surface, &mut f.server);
    f.server.ime = ime;
    f.server.seat.keyboard_focus(surface);
    let ti = f.editor.state.ti.as_ref().unwrap().clone();
    ti.enable();
    ti.commit();
    assert!(f.drive());
    f.server.seat.deactivate_keyboard();
    f.server.ime.popup_touch();
    let s = f.server.ime.take_popup_snapshot(false).expect("deactivate 应推进可见性");
    assert!(!s.visible, "游戏失焦（kb_active=false）→ visible 必须 0");
    f.server.seat.activate_keyboard();
    f.server.ime.popup_touch();
    let s = f.server.ime.take_popup_snapshot(true).expect("重新聚焦应推进可见性");
    assert!(s.visible, "游戏重新 focused → 候选窗恢复可见");
}

/// attach(NULL)（fcitx5 隐藏候选窗第一步 `WaylandShmWindow::hide()`）→
/// 帧清空、Java 侧必须隐藏；之后重新提交 → 恢复可见、seq 继续推进。
#[test]
fn popup_null_attach_hides_and_recommit_returns() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let (surface, _popup, _px) = popup_with_frame(&mut f, 1, 1, 3);

    // 正常帧已交付。
    let s = f.server.ime.take_popup_snapshot(true).expect("首帧");
    assert!(s.visible);

    // attach(NULL) + commit → hide 语义。
    surface.attach(None, 0, 0);
    surface.commit();
    assert!(f.drive());
    let s = f.server.ime.take_popup_snapshot(true).expect("隐藏应推进状态");
    assert!(!s.visible, "attach(NULL) 后候选窗必须隐藏");
    assert_eq!((s.width, s.height), (0, 0));

    // 重新提交 → 恢复。
    let qh = f.ime.qh();
    let buf = ShmBuf::new(&f.ime.state, &qh, 3, 1, 12, wl_shm::Format::Argb8888);
    buf.write(&argb_pattern(3, 1, 9));
    surface.attach(Some(&buf.buffer), 0, 0);
    surface.damage_buffer(0, 0, 3, 1);
    surface.commit();
    assert!(f.drive());
    let s = f.server.ime.take_popup_snapshot(true).expect("重新提交应恢复");
    assert!(s.visible);
    assert_eq!((s.width, s.height), (3, 1));
    assert!(s.seq > 1, "seq 单调推进");
}

/// popup.destroy → 采集轨道解除；之后同 surface 再 commit 不再采集。
#[test]
fn popup_destroy_stops_capture() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let (surface, popup, _px) = popup_with_frame(&mut f, 1, 1, 5);
    assert!(f.server.ime.take_popup_snapshot(true).expect("首帧").visible);

    popup.destroy();
    assert!(f.drive());
    // 销毁交付"消失"状态（不可见、无帧）。
    let s = f.server.ime.take_popup_snapshot(true).expect("销毁应推进状态");
    assert!(!s.visible);
    assert_eq!((s.width, s.height), (0, 0));
    assert!(f.server.ime.im2.popup.is_none(), "既有字段同步清除");

    // 之后同 surface 再 commit（角色已解除）→ 不再采集、状态不变。
    let qh = f.ime.qh();
    let buf = ShmBuf::new(&f.ime.state, &qh, 1, 1, 4, wl_shm::Format::Argb8888);
    buf.write(&[1, 2, 3, 4]);
    surface.attach(Some(&buf.buffer), 0, 0);
    surface.damage_buffer(0, 0, 1, 1);
    surface.commit();
    assert!(f.drive());
    assert!(f.server.ime.take_popup_snapshot(true).is_none(), "销毁后不得再采集");
}

/// input_method 对象销毁（daemon 顶替/断连路径）→ popup 采集一并解除。
#[test]
fn popup_cleared_with_input_method_destroy() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let (_surface, _popup, _px) = popup_with_frame(&mut f, 1, 1, 7);
    assert!(f.server.ime.take_popup_snapshot(true).expect("首帧").visible);

    let im = f.ime.state.im.take().unwrap();
    im.destroy();
    assert!(f.drive());
    // note_im2_gone → popup.cleared → 交付消失状态。
    let s = f.server.ime.take_popup_snapshot(true).expect("断连应推进状态");
    assert!(!s.visible);
    assert!(f.server.ime.im2.instance.is_none(), "im2 实例随 destroy 清空");
}

/// 协议错误：同一 surface 二次赋 input_popup 角色 → 合成器报错、client 连接
/// 终结（服务端断连清理路径同径执行）；首个登记不被静默顶掉。
#[test]
fn popup_double_role_posts_protocol_error() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let (surface, _popup1, _px) = popup_with_frame(&mut f, 1, 1, 9);

    // 第二次：同 surface 再赋角色（模拟违规客户端）。
    let qh = f.ime.qh();
    let _popup2 = f.ime.state.im.as_ref().unwrap().get_input_popup_surface(&surface, &qh, ());
    // 报错与断连传播需要两轮泵（dispatch_pending 故意吞掉 backend 错误，
    // 见 wayland-client dispatching_impl 注释；断连在后续 dispatch 被服务端感知）。
    let _ = f.drive();
    let _ = f.drive();
    assert!(
        f.server.ime.im2.instance.is_none(),
        "协议错误后 client 应终结（im2 断连清理生效）"
    );
    assert!(
        f.server.ime.popup.surface().is_none(),
        "popup 采集轨道随断连解除（不得保留二次登记的顶替态）"
    );

    // client 端：死连接上的新请求 + flush 必须报错（协议错误致命）。
    f.ime.state.im.as_ref().unwrap().commit_string("post-mortem".into());
    assert!(f.ime.flush_err(), "协议错误后 client 连接必须已死");
}
