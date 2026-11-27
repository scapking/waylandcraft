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
//! Java 轮询 = ImeState::take_popup_snapshot()
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

use crate::ime::input_popup::POPUP_HOLD_MS;
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
    /// wl_callback.done 流水（v1.4.4 帧回调保命线断言用：commit 即兑现）。
    pub callback_done: Vec<u32>,
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
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { callback_data } = event {
            state.callback_done.push(callback_data);
        }
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

    /// 服务端反查 IM 客户端某 surface 的 WlSurface（seat pointer 通道入参）。
    fn server_surface_of_ime(
        &self,
        client_surface: &wl_surface::WlSurface,
    ) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        use wayland_client::Proxy;
        let proto = client_surface.id().protocol_id();
        self.ime_client
            .object_from_protocol_id::<
                smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
            >(&self.display.handle(), proto)
            .unwrap()
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

/// v1.4.5 红线（用户实测"窗口键盘可用但模拟鼠标几乎不可用"）：
/// pointer_motion_focus 必须把 enter(+frame)+motion(+frame) 送达同 client
/// 的已绑定 wl_pointer——这是 button 能被应用接受的前提（button 只在
/// pointer 有 focus 时发出）。链路任何一环静默丢失都会复现该症状。
#[test]
fn pointer_motion_focus_delivers_enter_and_motion() {
    use wayland_client::Proxy;
    let mut f = setup_popup();
    let qh = f.ime.qh();
    // ime 客户端自己的 surface（与其 pointer 同 client，enter 合法）
    let csurface = f.ime.state.compositor.as_ref().unwrap().create_surface(&qh, ());
    csurface.commit();
    assert!(f.drive());
    let proto = csurface.id().protocol_id();
    let srv_surface = f
        .ime_client
        .object_from_protocol_id::<
            smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        >(&f.display.handle(), proto)
        .expect("服务端可见该 surface");

    f.server.seat.pointer_motion_focus(Some(&srv_surface), 12.0, 7.0);
    assert!(f.drive());
    assert!(f.drive());

    let evs = &f.ime.state.ptr_events;
    let enter_at = evs.iter().position(|e| e == "enter").expect("必须送达 enter");
    let motion_at = evs.iter().position(|e| e == "motion").expect("必须送达 motion");
    assert!(enter_at < motion_at, "enter 必须先于 motion: {evs:?}");
    // wl_pointer v5+：enter/motion 各自必须带 frame 定界（Chromium 依赖）
    assert!(evs.iter().filter(|e| *e == "frame").count() >= 2, "enter/motion 后都要有 frame: {evs:?}");
}

/// v1.4.4 协议保命线红线（用户实测病灶：launch 应用后"第一帧即永久冻结、
/// mapped:0、物品无图标"——报文实证 Chrome 已 attach+commit 但
/// wl_surface.frame 回调零 done，Chromium dmabuf 池 2-3 槽饿死）：
/// frame 回调必须在**服务端 commit 分发时**兑现，与 Java 轮询链的
/// sendFrame 完全解耦——Java 侧树被瞬时异常打断也不许冻结客户端。
/// 本 fixture 全程不调 sendFrame：done 计数 >0 即证明解耦成立。
#[test]
fn commit_drains_frame_callbacks_without_java_polling() {
    let mut f = setup_popup();
    let qh = f.ime.qh();
    let surface = f.ime.state.compositor.as_ref().unwrap().create_surface(&qh, ());
    let cb = surface.frame(&qh, ());
    surface.commit();
    assert!(f.drive(), "commit 不应引发协议错误");
    assert_eq!(
        f.ime.state.callback_done.len(),
        1,
        "commit 必须当场兑现 frame done（不经 Java sendFrame）"
    );
    drop(cb);
    // 每帧语义：第二个 frame 请求同样在下一 commit 兑现。
    let cb2 = surface.frame(&qh, ());
    surface.commit();
    assert!(f.drive());
    assert_eq!(f.ime.state.callback_done.len(), 2, "frame done 必须逐次兑现，非一次性");
    drop(cb2);
}

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
        .take_popup_snapshot()
        .expect("有新帧状态应交付");
    assert!(snap.visible, "activate+focus+kb → visible=1");
    assert_eq!((snap.width, snap.height), (4, 3));
    assert_eq!(snap.seq, 1);
    assert_eq!(snap.argb.as_ref().expect("可见应有像素"), &px);

    // 再轮询：零拷贝 null。
    assert!(f.server.ime.take_popup_snapshot().is_none(), "无变化必须 null");

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
    let s1 = f.server.ime.take_popup_snapshot().expect("第一帧");
    assert_eq!(s1.seq, 1);
    assert_eq!(s1.argb.as_ref().unwrap(), &px1);

    let buf2 = ShmBuf::new(&f.ime.state, &qh, 2, 2, 8, wl_shm::Format::Argb8888);
    let px2 = argb_pattern(2, 2, 7);
    buf2.write(&px2);
    surface.attach(Some(&buf2.buffer), 0, 0);
    surface.damage_buffer(0, 0, 2, 2);
    surface.commit();
    assert!(f.drive());
    let s2 = f.server.ime.take_popup_snapshot().expect("第二帧");
    assert_eq!(s2.seq, 2);
    assert_eq!(s2.argb.as_ref().unwrap(), &px2);

    // XRGB8888：alpha 字节全 0，快照里必须强制 0xFF。
    let buf3 = ShmBuf::new(&f.ime.state, &qh, 1, 1, 4, wl_shm::Format::Xrgb8888);
    buf3.write(&[0x11, 0x22, 0x33, 0x00]);
    surface.attach(Some(&buf3.buffer), 0, 0);
    surface.damage_buffer(0, 0, 1, 1);
    surface.commit();
    assert!(f.drive());
    let s3 = f.server.ime.take_popup_snapshot().expect("第三帧");
    assert_eq!(s3.argb.as_ref().unwrap().as_slice(), &[0x11, 0x22, 0x33, 0xff]);
}

/// 可见性门：帧先于会话到达 → visible=0；会话激活后可见；会话终结→0。
/// 显隐跟随 IME 会话（popup_ime_active），**与窗口捕获键盘 kb_active 无关**
/// （聊天打字 kb_active 恒 false 仍须可见——sync11 根因回归）。
#[test]
fn popup_visibility_follows_activate_and_focus() {
    let mut f = setup_popup();
    // 不 focus_and_enable —— 直接建 popup + 提交帧（会话未激活）。
    let (_surface, _popup, px) = popup_with_frame(&mut f, 2, 1, 0);

    // 有帧但无会话 → 交付"不可见"快照（尺寸在、无像素）。
    let s = f.server.ime.take_popup_snapshot().expect("帧到达应有状态变化");
    assert!(!s.visible, "无 activate/focus 会话时 popup 不得可见");
    assert_eq!(s.width, 2);
    assert!(s.argb.is_none());
    assert_eq!(s.seq, 1);

    // 会话开始 → Activate（exec_on_im2 touch epoch）→ 再次轮询可见。
    f.focus_and_enable();
    let s = f.server.ime.take_popup_snapshot().expect("activate 应推进可见性");
    assert!(s.visible);
    assert_eq!(s.argb.as_ref().unwrap(), &px);

    // 失焦 → clear_focus touch + relay focus_lost Deactivate → 不可见。
    f.unfocus();
    let s = f.server.ime.take_popup_snapshot().expect("失焦应推进可见性");
    assert!(!s.visible, "ti3 焦点离开 → 候选窗必须隐藏");

    // sync11 生产根因回归：候选窗显隐跟随 **IME 会话**，与窗口捕获键盘
    // 状态（seat.kb_active）无关。聊天框打字时没有 native 窗口被聚焦
    // （focusSurface 只接受 ToplevelSurface），kb_active 恒 false——旧门
    // 误用它（连同 has_focus）做可见性条件，导致聊天场景候选窗永不显示
    // （用户四轮"采到帧但候选窗从不显示"的真正根因）。
    let surface = f.editor_server_surface();
    let mut ime = std::mem::take(&mut f.server.ime);
    ime.set_focus(&surface, &mut f.server);
    f.server.ime = ime;
    f.server.seat.keyboard_focus(surface);
    let ti = f.editor.state.ti.as_ref().unwrap().clone();
    ti.enable();
    ti.commit();
    assert!(f.drive());
    // 窗口捕获键盘关闭（聊天形态 kb_active=false）但会话仍激活 → 必须仍可见。
    f.server.seat.deactivate_keyboard();
    f.server.ime.popup_touch();
    let s = f.server.ime.take_popup_snapshot().expect("kb_active 变化应推进 epoch");
    assert!(s.visible, "聊天形态 kb_active=false 但会话激活 → 候选窗必须可见（sync11 根因）");
    // 会话终结（clear_focus → relay focus_lost Deactivate）才隐藏。
    f.unfocus();
    let s = f.server.ime.take_popup_snapshot().expect("会话终结应推进可见性");
    assert!(!s.visible, "IME 会话 deactivate → 候选窗隐藏（与键盘捕获无关）");
}

/// attach(NULL)（fcitx5 隐藏候选窗第一步 `WaylandShmWindow::hide()`）→
/// 帧清空、Java 侧必须隐藏；之后重新提交 → 恢复可见、seq 继续推进。
#[test]
fn popup_null_attach_hides_and_recommit_returns() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let (surface, _popup, _px) = popup_with_frame(&mut f, 1, 1, 3);

    // 正常帧已交付。
    let s = f.server.ime.take_popup_snapshot().expect("首帧");
    assert!(s.visible);

    // attach(NULL) + commit → hide 语义。
    surface.attach(None, 0, 0);
    surface.commit();
    assert!(f.drive());
    let s = f.server.ime.take_popup_snapshot().expect("隐藏应推进状态");
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
    let s = f.server.ime.take_popup_snapshot().expect("重新提交应恢复");
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
    assert!(f.server.ime.take_popup_snapshot().expect("首帧").visible);

    popup.destroy();
    assert!(f.drive());
    // 销毁交付"消失"状态（不可见、无帧）。
    let s = f.server.ime.take_popup_snapshot().expect("销毁应推进状态");
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
    assert!(f.server.ime.take_popup_snapshot().is_none(), "销毁后不得再采集");
}

/// input_method 对象销毁（daemon 顶替/断连路径）→ popup 采集一并解除。
#[test]
fn popup_cleared_with_input_method_destroy() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let (_surface, _popup, _px) = popup_with_frame(&mut f, 1, 1, 7);
    assert!(f.server.ime.take_popup_snapshot().expect("首帧").visible);

    let im = f.ime.state.im.take().unwrap();
    im.destroy();
    assert!(f.drive());
    // note_im2_gone → popup.cleared → 交付消失状态。
    let s = f.server.ime.take_popup_snapshot().expect("断连应推进状态");
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

// ═══════════ 回合内竞态回归（sync9 生产实锤：候选窗永远不可见）═══════════

/// 生产故障形态的回归测试（sync9 观测网实锤）：fcitx5 classicui 在**同一个
/// wayland 事件循环回合**里完成"attach 内容帧 commit → attach(NULL) commit
/// → popup.destroy →（下一候选）新角色登记"。Java 只在回合结束后轮询一次，
/// 修复前的快照序列：note_frame 采到帧（epoch+1）→ 同回合 attach(NULL) 清帧
/// （epoch+1）→ 轮询拿到 changed=true 但 has_frame=false → visible=false——
/// 帧内容生命周期 < 轮询间隔 → 候选窗永远不可见（尽管帧真实采到过）。
///
/// 本测试把三个请求（内容 commit、NULL commit、destroy+重新登记）压进
/// **一次 drive()**（dispatch_clients 按请求到达序在同一回合全部消化），
/// 然后**只轮询一次**，断言修复后必须拿到 visible=true 的完整像素帧
/// （display hold：采到但从未交付过的帧在 hold 窗口内继续交付）。
/// 修复前此断言为红（visible=false），作为回归门槛锁死。
#[test]
fn popup_same_round_attach_null_race_still_delivers_visible_frame() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let qh = f.ime.qh();
    let surface = f.ime.state.compositor.as_ref().unwrap().create_surface(&qh, ());
    let popup = f.ime.state.im.as_ref().unwrap().get_input_popup_surface(&surface, &qh, ());
    assert!(f.drive());
    // 角色登记轮消耗 epoch（Java tick 已见不可见交付）。
    let _ = f.server.ime.take_popup_snapshot();

    // ── 同一回合：attach 内容 → commit → attach(NULL) → commit → destroy → 新角色 ──
    let px = argb_pattern(4, 3, 0);
    let buf = ShmBuf::new(&f.ime.state, &qh, 4, 3, 16, wl_shm::Format::Argb8888);
    buf.write(&px);
    surface.attach(Some(&buf.buffer), 0, 0);
    surface.damage_buffer(0, 0, 4, 3);
    surface.commit();
    surface.attach(None, 0, 0);
    surface.commit();
    popup.destroy();
    let surface2 = f.ime.state.compositor.as_ref().unwrap().create_surface(&qh, ());
    let _popup2 = f.ime.state.im.as_ref().unwrap().get_input_popup_surface(&surface2, &qh, ());
    std::mem::forget(buf);
    // 一次 pump = 一个 wayland 事件循环回合（修复前后的差别全在本回合内）。
    assert!(f.drive());

    // ── Java 只轮询一次：必须看到本回合采到过的帧（display hold 交付）──
    let snap = f
        .server
        .ime
        .take_popup_snapshot()
        .expect("回合内帧+清除应推进 epoch");
    assert!(snap.visible, "回合内 attach(NULL) 竞态不得吞掉从未交付的帧");
    assert_eq!((snap.width, snap.height), (4, 3));
    assert_eq!(snap.argb.as_ref().expect("hold 交付应携带完整像素"), &px);
    assert!(snap.seq >= 1, "seq 必须是采到的那一帧的序号");
}

/// hold 到期（假时钟推进）→ 快照必须回落不可见（真隐藏不被无限延长）；
/// 到期前新帧到达 → 正常重排续期。e2e 级生命周期收口（单测细节见
/// input_popup::tests 的 hold_* 用例）。
#[test]
fn popup_race_frame_expires_to_hidden_after_hold_window() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let qh = f.ime.qh();
    let surface = f.ime.state.compositor.as_ref().unwrap().create_surface(&qh, ());
    let _popup = f.ime.state.im.as_ref().unwrap().get_input_popup_surface(&surface, &qh, ());
    assert!(f.drive());
    let _ = f.server.ime.take_popup_snapshot();

    // 同回合竞态：内容帧 + attach(NULL)。
    let px = argb_pattern(2, 2, 5);
    let buf = ShmBuf::new(&f.ime.state, &qh, 2, 2, 8, wl_shm::Format::Argb8888);
    buf.write(&px);
    surface.attach(Some(&buf.buffer), 0, 0);
    surface.damage_buffer(0, 0, 2, 2);
    surface.commit();
    surface.attach(None, 0, 0);
    surface.commit();
    std::mem::forget(buf);
    assert!(f.drive());

    // 第一次轮询：hold 交付可见帧。
    let s = f.server.ime.take_popup_snapshot().expect("竞态帧应交付");
    assert!(s.visible);
    assert_eq!(s.argb.as_ref().unwrap(), &px);

    // 假时钟推进过 hold 窗口（无新帧）→ 下一次轮询必须隐藏（真清状态）。
    let t = f.server.ime.popup.now();
    f.server.ime.popup.set_clock(t + std::time::Duration::from_millis(401));
    let s = f.server.ime.take_popup_snapshot().expect("hold 到期应推进状态");
    assert!(!s.visible, "hold 到期且无新帧 → 候选窗必须隐藏");
    assert!(s.argb.is_none());
    // 再轮询零拷贝。
    assert!(f.server.ime.take_popup_snapshot().is_none());
}

// ═══════════════ Java 轮询 × JNI 编码合同（sync8 静默故障复盘）═══════════════

/// Java 每 tick 轮询的 native 半程复刻：`ImeCandidateOverlay.tick()` 每帧
/// 调 `take_popup_snapshot()` 再按 `bridge::encode_popup_snapshot`
/// 编码——**这是编码合同测试**：若这里断，根因在 native；全绿则根因锁定
/// Java 运行时链（tick→upload→extract）。断言：
/// - 角色登记轮：13 字节不可见帧（epoch 由 created 推进）；
/// - 提交帧轮：`13 + w*h*4`、头 `[w i32][h i32][seq u32][visible u8]` 逐字段、
///   像素与 shm 图案逐字节，且与 Java 测试固化的 golden hexdump 全等；
/// - 无变化连续 60 轮：全部 null（零拷贝——Java 靠保留 current 维持显示）；
/// - attach(NULL) 隐藏轮：13 字节 visible=0；
/// - 观测网统计：frames_captured/deliveries/last_visible 与轮次一致。
#[test]
fn popup_java_polling_sequence_matches_wire_encoding_contract() {
    let mut f = setup_popup();
    f.focus_and_enable();

    // 1) popup 角色登记（尚无帧）→ tick 应见 13 字节不可见交付。
    let qh = f.ime.qh();
    let surface = f.ime.state.compositor.as_ref().unwrap().create_surface(&qh, ());
    let _popup = f.ime.state.im.as_ref().unwrap().get_input_popup_surface(&surface, &qh, ());
    assert!(f.drive());
    let raw = poll_java_tick(&mut f).expect("角色登记推进 epoch，tick 应有交付");
    assert_eq!(raw.len(), 13, "无帧交付必须恰为 13 字节头");
    assert_eq!(raw[12], 0, "无帧时 visible=0");

    // 2) attach/damage/commit 一帧 4x3（经典 fcitx5 classicui shm ARGB8888）。
    let px = argb_pattern(4, 3, 0);
    let buf = ShmBuf::new(&f.ime.state, &qh, 4, 3, 16, wl_shm::Format::Argb8888);
    buf.write(&px);
    surface.attach(Some(&buf.buffer), 0, 0);
    surface.damage_buffer(0, 0, 4, 3);
    surface.commit();
    assert!(f.drive());
    let raw = poll_java_tick(&mut f).expect("新帧必须在下一 tick 交付");
    assert_eq!(raw.len(), 13 + 4 * 3 * 4, "编码总长 = 13 + w*h*4");
    assert_eq!(&raw[0..4], &4i32.to_le_bytes(), "w");
    assert_eq!(&raw[4..8], &3i32.to_le_bytes(), "h");
    assert_eq!(&raw[8..12], &1i32.to_le_bytes(), "seq（首帧=1）");
    assert_eq!(raw[12], 1, "四门全开（frame/im2+active/focus/kb）→ visible=1");
    assert_eq!(&raw[13..], &px[..], "像素逐字节直通 shm ARGB8888 小端图案");

    // Golden fixture：与 Java ImeCandidateLayoutTest.nativeGoldenFrameMatchesBridgeEncoding
    // 固化的 hexdump 逐字节对齐（跨语言合同——编码改了两侧一起红）。
    const GOLDEN_HEX: &str = "04000000030000000100000001112244ff102345ff132046ff122147ff\
        013254ff003355ff033056ff023157ff310264ff300365ff330066ff320167ff";
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, GOLDEN_HEX, "JNI 字节流必须与 Java 侧解码 fixture 全等");

    // 3) 无变化连续轮询（模拟 Java 60 tick）→ 全部 null（零拷贝路径）。
    for i in 0..60 {
        assert!(poll_java_tick(&mut f).is_none(), "第 {i} 轮无变化必须 null");
    }

    // 4) attach(NULL)（WaylandShmWindow::hide 第一步）→ 13 字节 visible=0。
    surface.attach(None, 0, 0);
    surface.commit();
    assert!(f.drive());
    let raw = poll_java_tick(&mut f).expect("隐藏应交付状态");
    assert_eq!(raw.len(), 13);
    assert_eq!(raw[12], 0);

    // 5) 观测网统计与轮次一致（/wl status ime_candidate_popup 原料）。
    assert_eq!(f.server.ime.popup.frames_captured(), 1, "采集帧计数");
    assert_eq!(f.server.ime.popup.deliveries(), 3, "13B登记 + 可见帧 + 隐藏 = 3 次交付");
    assert!(f.server.ime.popup.last_visible_delivery_ago_secs().is_some(), "visible 交付时刻已记账");
    assert_eq!(f.server.ime.popup.epoch(), f.server.ime.popup.delivered(), "轮询耗尽后 epoch==delivered");
}

/// Java tick 的 native 半程：take_popup_snapshot + bridge 编码（共用生产
/// 纯函数，非复刻实现——编码漂移这里直接红）。
fn poll_java_tick(f: &mut PopupFixture) -> Option<Vec<u8>> {
    let snap = f.server.ime.take_popup_snapshot()?;
    Some(crate::bridge::encode_popup_snapshot(&snap))
}

// ═══════════ 角标工作态（sync9 生产形态：弹窗模式预编辑流证据）═══════════

/// workState 生产根因回归（用户日志 workState=false 而 commit 正常）：
/// 弹窗模式下 fcitx5 的 clientPreedit 恒空——预编辑像素走 popup 采集轨道、
/// 面板更新走**无内容 commit(serial) 心跳**（waylandimserverv2.cpp
/// updatePreeditDelegate），到达 emit_ti_batch 的只有选字上屏的
/// commit_string 批次。旧实现每批无条件 `flag = any(非空 preedit)`，
/// 这些"输出批次"把角标原料反复清零 → 组字期间角标永不出现。
///
/// 本测试按生产事件序列逐步断言三种置位证据与两种终结信号：
/// 采帧置位 → commit 批次不清除 → 真隐藏（已交付帧 attach(NULL)）清零 →
/// 竞态 hold 期间保持 → hold 到期清零。
#[test]
fn popup_work_state_production_shapes() {
    let mut f = setup_popup();
    f.focus_and_enable();
    assert!(
        !f.server.ime.popup_work_state(),
        "刚激活、零证据 → 角标不得虚位"
    );

    let qh = f.ime.qh();
    let im = f.ime.state.im.as_ref().unwrap().clone();

    // 证据 1：弹窗模式心跳——无内容 commit(serial)（clientPreedit 空的
    // updatePreeditDelegate 形态）。
    im.commit(f.server.ime.im2.done_count());
    assert!(f.drive());
    assert!(f.server.ime.popup_work_state(), "面板更新心跳 = 组字中 → 角标必须置位");

    // 采一帧并交付（overlay 显示候选窗）。
    let (surface, _popup, _px) = popup_with_frame(&mut f, 6, 2, 1);
    let s = f.server.ime.take_popup_snapshot().expect("首帧").visible;
    assert!(s);

    // 证据 2 + 旧 bug 锁定：选字上屏 = 纯 commit_string 批次。新语义
    // 不得清除已有证据（组字可能在 commit 后继续；终结另有显式信号）。
    im.commit_string("你".into());
    im.commit(f.server.ime.im2.done_count());
    assert!(f.drive());
    assert!(
        f.server.ime.popup_work_state(),
        "commit-only 批次不得翻转工作态（sync9 角标根因）"
    );

    // 终结 1：真实隐藏——已交付的帧被 attach(NULL)（弹窗收起 = 组字结束）。
    surface.attach(None, 0, 0);
    surface.commit();
    assert!(f.drive());
    assert!(
        !f.server.ime.popup_work_state(),
        "已交付帧真隐藏 → 候选窗内容终结 → 角标落"
    );

    // 终结 2 / 保帧：同回合竞态（采到未交付 → attach(NULL)）期间 hold
    // 交付保持可见，组字证据不被误终结；到期后才落。
    let px2 = argb_pattern(4, 2, 4);
    let buf = ShmBuf::new(&f.ime.state, &qh, 4, 2, 16, wl_shm::Format::Argb8888);
    buf.write(&px2);
    surface.attach(Some(&buf.buffer), 0, 0);
    surface.damage_buffer(0, 0, 4, 2);
    surface.commit();
    surface.attach(None, 0, 0);
    surface.commit();
    std::mem::forget(buf);
    assert!(f.drive());
    assert!(f.server.ime.popup_work_state(), "采到新帧 = 组字证据置位");
    let s = f.server.ime.take_popup_snapshot().expect("hold 交付");
    assert!(s.visible && s.argb.as_deref() == Some(&px2[..]));
    assert!(f.server.ime.popup_work_state(), "hold 交付期间证据保持");
    // 假时钟推过 hold 窗口 → 轮询到期 → 可见性与角标同步回落。
    let t = f.server.ime.popup.now();
    f.server.ime.popup.set_clock(t + std::time::Duration::from_millis(POPUP_HOLD_MS + 1));
    let s = f.server.ime.take_popup_snapshot().expect("到期应推进");
    assert!(!s.visible);
    assert!(!f.server.ime.popup_work_state(), "hold 到期无新帧 → 角标终结");
}

// ═══════════════════════ Phase 3：pointer 点选回传 ═══════════════════════

/// overlay 区域点击 → native 向 popup surface 发 enter→(motion)→button
/// （+frame），进帧序列符合 wl_pointer 惯例；第二次不同坐标点击补 motion；
/// release 只发 button 不改 focus。断言 IM 客户端 wl_pointer 收到正确事件。
#[test]
fn popup_pointer_click_sends_enter_motion_button() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let (surface, _popup, _px) = popup_with_frame(&mut f, 40, 12, 0);
    // 交付首帧，让 overlay 侧"已显示"。
    assert!(f.server.ime.take_popup_snapshot().expect("首帧").visible);

    let srv = f.server_surface_of_ime(&surface);
    // 按下（候选区某点）。
    f.server.seat.popup_pointer_click(&srv, 5.0, 3.0, 0x110, true);
    f.drive();
    f.ime.dispatch();

    let evs = &f.ime.state.ptr_events;
    let enter = evs.iter().position(|e| e == "enter").expect("应有 enter");
    let button = evs.iter().position(|e| e == "button").expect("应有 button");
    // enter 携带坐标即已定位，首次按下可不发独立 motion；button 必在 enter 之后。
    assert!(enter < button, "button 必须晚于 enter: {:?}", evs);
    assert!(evs.iter().any(|e| e == "frame"), "每事件批次应补 frame");

    // 换坐标再按（同 surface 已聚焦 → 应补 motion 后再 button）。
    let n_before = f.ime.state.ptr_events.len();
    f.server.seat.popup_pointer_click(&srv, 30.0, 9.0, 0x110, true);
    f.drive();
    f.ime.dispatch();
    let new_evs = &f.ime.state.ptr_events[n_before..];
    let motion = new_evs.iter().position(|e| e == "motion").expect("换坐标应补 motion");
    let button2 = new_evs.iter().position(|e| e == "button").expect("应再次 button");
    assert!(motion < button2, "motion 必须早于 button: {:?}", new_evs);

    // 释放。
    f.server.seat.popup_pointer_click(&srv, 30.0, 9.0, 0x110, false);
    f.drive();
    f.ime.dispatch();
    assert!(f.ime.state.ptr_events.iter().any(|e| e == "button"));

    // popup 销毁 → 对聚焦过的 pointer 补 leave。
    _popup.destroy();
    f.drive();
    f.ime.dispatch();
    assert!(
        f.ime.state.ptr_events.iter().any(|e| e == "leave"),
        "popup 销毁应对 pointer 补 leave: {:?}",
        f.ime.state.ptr_events
    );
}

/// overlay 区域滚轮 → enter/motion + axis 批（vertical，含 source/value120）。
#[test]
fn popup_pointer_axis_sends_wheel() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let (surface, _popup, _px) = popup_with_frame(&mut f, 40, 12, 0);
    assert!(f.server.ime.take_popup_snapshot().expect("首帧").visible);
    let srv = f.server_surface_of_ime(&surface);

    f.server.seat.popup_pointer_axis(&srv, 20.0, 6.0, 1.0);
    f.drive();
    f.ime.dispatch();

    let evs = &f.ime.state.ptr_events;
    assert!(evs.iter().any(|e| e == "enter"), "滚轮前应 ensure 聚焦: {:?}", evs);
    assert!(evs.iter().any(|e| e == "axis"), "应收到 axis 事件: {:?}", evs);
    // pointer v8：应带 axis_source + axis_value120。
    assert!(evs.iter().any(|e| e == "axis_source"), "{:?}", evs);
    assert!(evs.iter().any(|e| e == "axis_value120"), "{:?}", evs);
}

// ═══════════ sync15 交付去重（"每 tick 全量重发"病灶回归）═══════════

/// 生产病灶复刻 + 修复门槛（用户实测"输入法的时候特别卡"主通路之一）：
/// Java 渲染循环每帧 focusSurface → keyboard_focus → ime.set_focus。
/// 修复前该路径**无条件** popup_touch 推 epoch，配合交付闸门（旧）把同一
/// `(seq, visible)` 每帧全量 clone+encode+JNI 重发（真机 bench50 实测：
/// 12-36 个真实帧被交付 1423-1473 次 / 13-40MB JNI 字节 / 9.4s）。
/// 修复后两层闸：
/// 1. set_focus 幂等（焦点原料未变化不推 epoch）→ churn 轮询直接 None；
/// 2. 内容指纹去重（其余触点如 keyboard_activate/会话抖动推进 epoch 时，
///    (seq,visible) 不变 → None + dup_skipped 计数）；
/// 3. 真实状态转移（隐藏/恢复/新帧）不受影响，照常完整交付。
#[test]
fn popup_tick_churn_same_frame_delivered_once() {
    let mut f = setup_popup();
    f.focus_and_enable();
    let (_surface, _popup, px) = popup_with_frame(&mut f, 4, 3, 2);
    let first = f
        .server
        .ime
        .take_popup_snapshot()
        .expect("新帧必须交付");
    assert!(first.visible);
    assert_eq!(first.argb.as_ref().unwrap(), &px);
    let d0 = f.server.ime.popup.deliveries();

    // 120 帧生产节拍 churn：set_focus(同一 surface) + 轮询。
    let srv = f.editor_server_surface();
    for i in 0..120 {
        let mut ime = std::mem::take(&mut f.server.ime);
        ime.set_focus(&srv, &mut f.server);
        f.server.ime = ime;
        assert!(
            f.server.ime.take_popup_snapshot().is_none(),
            "第 {i} 帧幂等 set_focus 不得产生任何交付（epoch 都不应推进）"
        );
    }
    assert_eq!(f.server.ime.popup.deliveries(), d0, "deliveries 只数真实状态转移");
    assert_eq!(f.server.ime.metrics.popup_dup_skipped, 0, "幂等层在前，去重层不该被触达");

    // 去重层：其余合法触点（keyboard_activate / ti3 抖动 Activate/Deactivate
    // 等）推进 epoch 但指纹未变 → None 消费 + 计数（不再重发像素）。
    for i in 0..5 {
        f.server.ime.popup_touch();
        assert!(
            f.server.ime.take_popup_snapshot().is_none(),
            "touch 后同 (seq,visible) 必须去重（第 {i} 轮）"
        );
    }
    assert_eq!(f.server.ime.metrics.popup_dup_skipped, 5);

    // 真实转移必须照常交付：会话终结 → 不可见（fingerprint visible 位翻转）。
    f.unfocus();
    let hide = f.server.ime.take_popup_snapshot().expect("会话终结必须交付隐藏");
    assert!(!hide.visible);
    // 重新激活 → 可见恢复（同帧同 seq 但 visible 位再次翻转，须重交付）。
    f.focus_and_enable();
    let s = f.server.ime.take_popup_snapshot().expect("重新激活必须交付可见");
    assert!(s.visible);
    assert_eq!(s.argb.as_ref().unwrap(), &px, "恢复交付携带完整像素");
}

// ═══════════════ 真 fcitx5 端到端（#[ignore]，本机跑，取证日志落盘）═══════════════

/// 隔离 profile/config：与 tests_real 同一取证结论（ActiveByDefault 种子），
/// 独立复制以免跨文件耦合（tests_real.rs 由并行 agent 维护，不动它）。
const R_SEED_PROFILE: &str = r#"[Groups/0]
# Group Name
Name=default
# Layout
Default Layout=us
# Default Input Method
DefaultIM=pinyin

[Groups/0/Items/0]
# Name
Name=keyboard-us
# Layout
Layout=

[Groups/0/Items/1]
# Name
Name=pinyin
# Layout
Layout=

[GroupOrder]
0=default
"#;
const R_SEED_CONFIG: &str = "[Behavior]\nActiveByDefault=True\n";

/// 真 fcitx5 popup 端到端（验收链 2）：隔离 HOME 起真 fcitx5 → editor
/// 聚焦 enable → 打 "niha" → **候选窗 popup surface 必须出现且采集到
/// 非空帧**（尺寸合理、字节数=w*h*4、含非零像素=引擎真画了字形）→ 点击
/// 候选区中心回传 → commit 出现；若 QML/主题布局导致中心未命中候选，
/// 退而断言 button 事件到达 client（WAYLAND_DEBUG 取证）+ SPACE 兜底
/// commit（规格允许的如实降级）。
#[test]
#[ignore = "真 fcitx5 端到端（本机跑）：popup 非空帧 + 点击回传 + commit"]
fn real_fcitx5_popup_frame_capture_and_click() {
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let base = PathBuf::from(format!("/tmp/wlc-f5popup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let runtime = base.join("runtime");
    let conf = home.join("config/fcitx5");
    for d in [&home, &conf, &runtime, &home.join("data"), &home.join("cache")] {
        std::fs::create_dir_all(d).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(conf.join("profile"), R_SEED_PROFILE).unwrap();
    std::fs::write(conf.join("config"), R_SEED_CONFIG).unwrap();

    let (mut display, mut server) = server_setup();
    let socket_name = "wayland-f5popup";
    let listener = UnixListener::bind(runtime.join(socket_name)).expect("bind");
    listener.set_nonblocking(true).unwrap();

    let (es, ec) = UnixStream::pair().unwrap();
    let editor_client = display
        .handle()
        .insert_client(es, Arc::new(ServerClientData))
        .unwrap();
    let mut editor: TestClient<EditorState> = editor_connect(ec, &mut display, &mut server);

    let log_path = base.join("fcitx5.log");
    let mut cmd = Command::new("fcitx5");
    cmd.args(["-D", "--disable=dbus"])
        .arg("--verbose")
        .arg("default=3,waylandim=5")
        .env("WAYLAND_DISPLAY", socket_name)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("WAYLAND_DEBUG", "client")
        .env("LANG", "C.UTF-8")
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap();
    let log_err = log.try_clone().unwrap();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    let mut fcitx = cmd.spawn().expect("spawn fcitx5");

    // ── 泵（同 tests_real::Harness 纪律：WLCClient + 手动交替）──
    let pump = |display: &mut Display<WLCState>,
                server: &mut WLCState,
                editor: &mut TestClient<EditorState>,
                listener: &UnixListener| {
        editor.flush();
        while let Ok((stream, _)) = listener.accept() {
            let _ = display
                .handle()
                .insert_client(stream, Arc::new(crate::WLCClient::new()));
        }
        let _ = display.dispatch_clients(server);
        let _ = display.flush_clients();
        editor.dispatch_pending();
    };
    let burst = |dur: std::time::Duration,
                 display: &mut Display<WLCState>,
                 server: &mut WLCState,
                 editor: &mut TestClient<EditorState>,
                 listener: &UnixListener| {
        let t0 = Instant::now();
        while t0.elapsed() < dur {
            pump(display, server, editor, listener);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };

    // 1) 等 im2 bind（冷启动词典 ~6s，给 90s 上限）
    let t0 = Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(90) {
        pump(&mut display, &mut server, &mut editor, &listener);
        if server.ime.im2.instance.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let bound = server.ime.im2.instance.is_some();

    let mut popup_seen = false;
    let mut frame = (0i32, 0i32, 0usize, 0usize); // w,h,argb_len,非零字节数
    let mut click_committed = false;
    let mut button_reached_client = false;
    let mut ti3: Vec<String> = Vec::new();

    if bound {
        // 2) editor 聚焦 + enable → activate；3) 等 grab
        {
            use wayland_client::Proxy;
            let proto = editor.state.surface.as_ref().unwrap().id().protocol_id();
            let surface = editor_client
                .object_from_protocol_id::<
                    smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                >(&display.handle(), proto)
                .unwrap();
            let mut ime = std::mem::take(&mut server.ime);
            ime.set_focus(&surface, &mut server);
            server.ime = ime;
            server.seat.keyboard_focus(surface);
        }
        let ti = editor.state.ti.as_ref().unwrap().clone();
        ti.enable();
        ti.commit();
        burst(std::time::Duration::from_millis(400), &mut display, &mut server, &mut editor, &listener);
        let t0 = Instant::now();
        while t0.elapsed() < std::time::Duration::from_secs(15) {
            pump(&mut display, &mut server, &mut editor, &listener);
            if server.ime.im2.grab.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let grabbed = server.ime.im2.grab.is_some();

        if grabbed {
            // 4) 打 "niha"（不空格：候选窗应挂在 preedit 展示期间）。
            //    本修复的最终门槛（sync9 真机验证）：真 fcitx5 classicui 的
            //    "同回合 attach 内容 → attach(NULL)（常伴角色更替）"竞态
            //    形态下，按 Java overlay 的 16ms 节奏模拟轮询，统计
            //    visible=true 且带完整像素的交付次数——修复前该计数恒 0
            //    （帧生命周期 < 轮询间隔，快照永远 has_frame=false），
            //    修复后 display hold 保证每个真实采到的帧至少交付一次。
            let mut visible_polls = 0u32;
            let mut snap_opt: Option<crate::ime::PopupSnapshot> = None;
            {
                let mut poll_tick = |display: &mut Display<WLCState>,
                                     server: &mut WLCState,
                                     editor: &mut TestClient<EditorState>,
                                     listener: &std::os::unix::net::UnixListener,
                                     dur: std::time::Duration| {
                    let t = Instant::now();
                    while t.elapsed() < dur {
                        pump(display, server, editor, listener);
                        if server.ime.popup_surface().is_some() {
                            popup_seen = true;
                        }
                        if let Some(s) = server.ime.take_popup_snapshot() {
                            let full_pixels = s
                                .argb
                                .as_ref()
                                .is_some_and(|a| a.len() == (s.width * s.height * 4) as usize);
                            if s.visible && full_pixels {
                                visible_polls += 1;
                                if snap_opt.is_none() {
                                    snap_opt = Some(s);
                                }
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(16));
                    }
                };
                for evdev in [49u32, 23, 35, 30] {
                    let mods = server.seat.modifiers_tuple();
                    server.ime.handle_key(evdev + 8, crate::seat::KeyboardAction::Press, mods);
                    poll_tick(&mut display, &mut server, &mut editor, &listener, std::time::Duration::from_millis(220));
                    server.ime.handle_key(evdev + 8, crate::seat::KeyboardAction::Release, mods);
                    poll_tick(&mut display, &mut server, &mut editor, &listener, std::time::Duration::from_millis(120));
                }
            }
            eprintln!("=== [f5popup] 打字期间 16ms 轮询 visible 交付次数: {visible_polls}");
            // display hold 修复门槛：真 fcitx5 竞态形态下至少 3 次带像素的
            // 可见交付（每键 1 帧 + 候选刷新，生产观测同会话 19 帧）。
            assert!(
                visible_polls >= 3,
                "真 fcitx5 打字期间按 16ms 轮询必须至少 3 次拿到 visible=true 完整像素帧（display hold 门槛），实际 {visible_polls}"
            );

            // 5) 等 popup 角色出现 + 非空帧（15s；第 4 步已见帧则跳过）。
            let t0 = Instant::now();
            while snap_opt.is_none() && t0.elapsed() < std::time::Duration::from_secs(15) {
                pump(&mut display, &mut server, &mut editor, &listener);
                popup_seen = server.ime.popup_surface().is_some();
                if popup_seen {
                    if let Some(s) = server.ime.take_popup_snapshot() {
                        if s.visible {
                            snap_opt = Some(s);
                            break;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if let Some(s) = &snap_opt {
                let nonz = s.argb.as_ref().map(|a| a.iter().filter(|b| **b != 0).count()).unwrap_or(0);
                frame = (s.width, s.height, s.argb.as_ref().map(|a| a.len()).unwrap_or(0), nonz);
                // 6) 点击候选区中心（回传 popup surface）
                if let Some(surf) = server.ime.popup_surface() {
                    let cx = (s.width.max(2) / 2) as f64;
                    let cy = (s.height.max(2) / 2) as f64;
                    server.seat.popup_pointer_click(&surf, cx, cy, 0x110, true);
                    burst(std::time::Duration::from_millis(150), &mut display, &mut server, &mut editor, &listener);
                    server.seat.popup_pointer_click(&surf, cx, cy, 0x110, false);
                    burst(std::time::Duration::from_millis(150), &mut display, &mut server, &mut editor, &listener);
                    // 7) 等点击驱动 commit（8s）
                    let t1 = Instant::now();
                    while t1.elapsed() < std::time::Duration::from_secs(8) {
                        pump(&mut display, &mut server, &mut editor, &listener);
                        if editor
                            .state
                            .ti_events
                            .iter()
                            .any(|e| e.starts_with("commit(") && e.chars().skip(7).any(|c| !c.is_ascii()))
                        {
                            click_committed = true;
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
            // 8) button 到达 client？（WAYLAND_DEBUG=client 取证 button( 行）
            if let Ok(logs) = std::fs::read_to_string(&log_path) {
                button_reached_client = logs.contains("wl_pointer")
                    && logs.lines().any(|l| l.contains("button(") && l.contains("272"));
            }
            // 9) 点击未直接出 commit → SPACE 兜底选首选词
            if !click_committed {
                let mods = server.seat.modifiers_tuple();
                server.ime.handle_key(57 + 8, crate::seat::KeyboardAction::Press, mods);
                burst(std::time::Duration::from_millis(150), &mut display, &mut server, &mut editor, &listener);
                server.ime.handle_key(57 + 8, crate::seat::KeyboardAction::Release, mods);
                let t1 = Instant::now();
                while t1.elapsed() < std::time::Duration::from_secs(10) {
                    pump(&mut display, &mut server, &mut editor, &listener);
                    if editor
                        .state
                        .ti_events
                        .iter()
                        .any(|e| e.starts_with("commit(") && e.chars().skip(7).any(|c| !c.is_ascii()))
                    {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
        burst(std::time::Duration::from_millis(300), &mut display, &mut server, &mut editor, &listener);
        ti3 = editor.state.ti_events.clone();
    }

    let _ = fcitx.kill();
    let _ = fcitx.wait();
    let _ = std::fs::remove_file(runtime.join(socket_name));

    let commits: Vec<String> = ti3
        .iter()
        .filter(|e| e.starts_with("commit(") && e.chars().skip(7).any(|c| !c.is_ascii()))
        .cloned()
        .collect();
    eprintln!("=== [f5popup] bound={bound} popup_surface={popup_seen} frame={frame:?} (w,h,argb_bytes,非零字节)");
    eprintln!("=== [f5popup] click_committed={click_committed} button_reached_client={button_reached_client} commits={commits:?}");
    eprintln!("=== [f5popup] ti3 events: {ti3:?}");
    eprintln!("=== [f5popup] 日志取证目录: {}（保留）", base.display());

    assert!(bound, "真 fcitx5 应 bind im2");
    assert!(popup_seen, "niha 组合期间应出现 input_popup 角色（classicui 候选窗）");
    let (w, h, alen, nonz) = frame;
    assert!(w >= 16 && h >= 8, "popup 帧尺寸应合理，实际 {w}x{h}");
    assert_eq!(alen, (w as usize) * (h as usize) * 4, "帧字节数必须 = w*h*4");
    assert!(nonz > alen / 100, "帧应含实际绘制内容（非空白），非零字节 {nonz}/{alen}");
    assert!(
        !commits.is_empty(),
        "最终必须有汉字 commit（click_committed={click_committed} button_reached_client={button_reached_client}）；ti3: {ti3:?}"
    );
    if !click_committed {
        assert!(
            button_reached_client,
            "点击未直接出 commit 时，必须能证明 button 事件到达 fcitx5 client（WAYLAND_DEBUG 日志无 wl_pointer.button）"
        );
    }
}
