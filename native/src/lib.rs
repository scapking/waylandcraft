use crate::bridge::BridgeState;
use crate::ddm::WLCDataState;
use crate::egl::EGLHelper;
use crate::ime::ImeState;
use crate::output::WLCOutput;
use crate::satellite::SatelliteState;
use crate::seat::WLCSeatState;
use crate::xdg_spec::XDGSpecHelper;
use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    delegate_compositor, delegate_dmabuf, delegate_shm,
    delegate_single_pixel_buffer, delegate_viewporter, delegate_xdg_shell,
    reexports::{
        calloop::{self, EventLoop, generic::Generic as GenericEvent},
        wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
        wayland_server::{
            self, Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{
                wl_buffer::WlBuffer, wl_output::WlOutput, wl_seat::WlSeat,
                wl_surface::WlSurface,
            },
        },
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState,
        },
        dmabuf::{
            DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState,
            ImportNotifier,
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler,
            XdgShellState,
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
    },
};
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

mod bridge;
mod ddm;
mod desktop_windows;
mod portal_capture;
mod audio_capture;
mod egl;
mod host_bridge;
mod ime;
mod java_types;
mod output;
mod process;
mod satellite;
mod seat;
mod svg;
mod utils;
mod xdg_spec;

pub(crate) struct WaylandCraft<'a> {
    pub state: WLCState,
    pub event_loop: EventLoop<'a, WLCState>,
    pub bridge: BridgeState,
    pub egl: EGLHelper,
    pub xdg: XDGSpecHelper,
    /// 宿主 IME 桥接（dbus-ibus / dbus-fcitx5）。
    /// 启动时探测一次；连接断开后由 update() 重建。
    /// 当前用途：C 方案 Layer 3 等待 XIM server 上线后启用。
    pub host_bridge: Option<crate::host_bridge::HostBridgeHandle>,
}

pub struct WLCState {
    pub display_handle: DisplayHandle,
    pub socket: OsString,
    pub satellite: Option<SatelliteState>,
    pub compositor_state: CompositorState,
    pub shm_state: ShmState,
    pub xdg_state: XdgShellState,
    pub viewporter_state: ViewporterState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub dmabuf_state: DmabufState,
    pub requests: WindowRequests,
    pub seat: WLCSeatState,
    /// dmabuf 共享全局；无可用渲染节点时为 None（客户端自动回退 shm 路径）。
    pub dmabuf_global: Option<DmabufGlobal>,
    /// 宿主 IME 桥接（v0.9.45+：让 apply_ti3_outcome / lib.rs::update
    /// 都能访问；之前只在 WaylandCraft 上有，但 Dispatch 路径拿不到）。
    pub host_bridge: Option<crate::host_bridge::HostBridgeHandle>,
    pub ime: ImeState,
    pub data: WLCDataState,
    pub output: WLCOutput,
}

#[derive(Default)]
pub struct WindowRequests {
    pub minimize: Vec<ToplevelSurface>,
    pub maximize: Vec<ToplevelSurface>,
    pub unmaximize: Vec<ToplevelSurface>,
    pub fullscreen: Vec<ToplevelSurface>,
    pub unfullscreen: Vec<ToplevelSurface>,
    pub move_interactive: Vec<Serial>,
    pub resize_interactive: Vec<(Serial, ResizeEdge)>,
}

impl WLCState {
    /// `egl` 传 None 用于无 GPU 环境（单元测试 / 软件渲染回退）：
    /// 此时跳过 dmabuf 全局，客户端走 shm 缓冲。
    fn new(disp: DisplayHandle, egl: Option<&EGLHelper>) -> Self {
        let compositor_state = CompositorState::new::<WLCState>(&disp);
        let shm_state = ShmState::new::<WLCState>(&disp, vec![]);
        let xdg_state = XdgShellState::new::<WLCState>(&disp);
        let viewporter_state = ViewporterState::new::<WLCState>(&disp);
        let single_pixel_buffer_state =
            SinglePixelBufferState::new::<WLCState>(&disp);

        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = match egl.and_then(|e| e.get_render_node().ok()) {
            Some(node) => Some(init_dmabuf(&disp, &mut dmabuf_state, egl.unwrap(), &node)),
            None => {
                eprintln!(
                    "[waylandcraft] no usable render node -> dmabuf sharing disabled (shm fallback)"
                );
                None
            }
        };

        let seat = WLCSeatState::new();
        seat.create_globals(&disp);

        let ime = ImeState::default();
        ime.create_globals(&disp);

        let data = WLCDataState::new(&disp);
        data.create_global();

        let output = WLCOutput::new(&disp);
        output.create_global();

        Self {
            display_handle: disp.clone(),
            socket: OsString::new(),
            satellite: None,
            compositor_state,
            shm_state,
            xdg_state,
            viewporter_state,
            single_pixel_buffer_state,
            dmabuf_state,
            dmabuf_global,
            requests: WindowRequests::default(),
            seat,
            ime,
            data,
            output,
            host_bridge: None, // WaylandCraft::update() 每帧同步（避免双 owner）
        }
    }
}

fn init_dmabuf(
    disp: &DisplayHandle,
    state: &mut DmabufState,
    egl: &EGLHelper,
    render_node: &smithay::backend::drm::DrmNode,
) -> DmabufGlobal {
    let render_node_id = render_node.dev_id();
    let formats = egl.query_dmabuf_formats();

    let feedback = DmabufFeedbackBuilder::new(render_node_id, formats)
        .build()
        .unwrap();

    state.create_global_with_default_feedback::<WLCState>(disp, &feedback)
}

impl CompositorHandler for WLCState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(
        &self,
        client: &'a wayland_server::Client,
    ) -> &'a CompositorClientState {
        &client.get_data::<WLCClient>().unwrap().compositor_state
    }

    fn commit(&mut self, _surface: &WlSurface) {}
}

impl BufferHandler for WLCState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for WLCState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for WLCState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let _ = notifier.successful::<WLCState>();
    }
}

impl XdgShellHandler for WLCState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.send_configure();
    }

    fn new_popup(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_configure().expect("popup initial configure");
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.requests.minimize.push(surface);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.requests.maximize.push(surface);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.requests.unmaximize.push(surface);
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<WlOutput>,
    ) {
        self.requests.fullscreen.push(surface);
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.requests.unfullscreen.push(surface);
    }

    fn move_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
    ) {
        self.requests.move_interactive.push(serial);
    }

    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        edges: ResizeEdge,
    ) {
        self.requests.resize_interactive.push((serial, edges));
    }
}

pub(crate) struct WLCClient {
    compositor_state: CompositorClientState,
}

impl WLCClient {
    fn new() -> Self {
        Self {
            compositor_state: CompositorClientState::default(),
        }
    }
}

impl ClientData for WLCClient {
    fn initialized(&self, _id: ClientId) {}

    fn disconnected(&self, _id: ClientId, _reason: DisconnectReason) {}
}

pub(crate) fn wlc_init(
    egl: EGLHelper,
    wayland_display: usize,
) -> Result<WaylandCraft<'static>, Box<dyn std::error::Error>> {
    let event_loop: EventLoop<WLCState> = EventLoop::try_new()?;
    let display: Display<WLCState> = Display::new()?;
    let socket = ListeningSocketSource::new_auto()?;

    let mut state = WLCState::new(display.handle(), Some(&egl));
    state.socket = socket.socket_name().to_os_string();

    // 系统桌面输入法穿透：按探测顺序选择宿主后端
    // （wayland-ti3 → dbus-ibus → …）。全部不可用为结构性不支持（不再重试）；
    // 暂时性失败（连接/总线问题）会在 update() 里自动重试。
    let mut ime_ready = false;
    // C 方案：探测宿主 IME daemon（dbus-ibus / dbus-fcitx5）。
    // 启动成功仅记录日志——当前 firefox 通过 GdkIMContext 直通宿主，
    // 不需要 mod 介入。XIM server 上线后 host_bridge 才真正被使用。
    let host_bridge = match crate::host_bridge::probe() {
        crate::host_bridge::BridgeInit::Ready(b) => {
            eprintln!(
                "[waylandcraft][host_bridge] OK -> {} (C 方案 Layer 3)",
                b.name()
            );
            ime_ready = true;
            Some(crate::host_bridge::HostBridgeHandle::new(b))
        }
        crate::host_bridge::BridgeInit::Transient(msg) => {
            eprintln!(
                "[waylandcraft][host_bridge] TRANSIENT: {msg}（无宿主 IME daemon；XIM server 上线后将不可用）"
            );
            None
        }
        crate::host_bridge::BridgeInit::Unsupported(msg) => {
            eprintln!(
                "[waylandcraft][host_bridge] UNSUPPORTED: {msg}（无 ibus/fcitx5 守护进程）"
            );
            None
        }
    };

    // Start xwayland-satellite to provide an X11 display for X11-only apps
    match satellite::start_satellite(&state.socket) {
        Ok(s) => {
            state.satellite = Some(s);
            eprintln!("[waylandcraft] xwayland-satellite started: DISPLAY={}", state.satellite.as_ref().unwrap().get_display());
        }
        Err(e) => eprintln!("[waylandcraft] Failed to start xwayland-satellite! Error: {e}"),
    }

    let ev_handle = event_loop.handle();

    ev_handle
        .insert_source(socket, |stream, _, state| {
            let client = WLCClient::new();
            state
                .display_handle
                .insert_client(stream, Arc::new(client))
                .unwrap();
        })
        .unwrap();

    let display_source = GenericEvent::new(
        display,
        calloop::Interest::READ,
        calloop::Mode::Level,
    );
    ev_handle
        .insert_source(display_source, |_, display_io, state| {
            unsafe {
                display_io.get_mut().dispatch_clients(state).unwrap();
            }
            Ok(calloop::PostAction::Continue)
        })
        .unwrap();

    let xdg = XDGSpecHelper::init();

    let _ = ime_ready; // 抑制 unused warning；未来 XIM server/im1 global 加回时用

    let instance = WaylandCraft {
        state,
        event_loop,
        bridge: BridgeState::new(),
        egl,
        xdg,
        host_bridge,
    };
    Ok(instance)
}

impl<'a> WaylandCraft<'a> {
    pub fn update(&mut self) {
        // v0.9.45：把 host_bridge 句柄共享给 state（让 apply_ti3_outcome
        // 也能访问——ti3 enter/leave 时需要给 host_bridge 发 FocusIn/FocusOut）。
        // 用 std::mem::take 避免双 &mut self 借用冲突：
        //   1. 从 self.host_bridge 取出 Option
        //   2. 把它移到 self.state.host_bridge
        //   3. dispatch 期间 dispatch 路径可能调用 apply_ti3_outcome（state borrow）
        //   4. dispatch 完后把它从 state 拿回 self.host_bridge
        // （更干净的做法是用 Rc<RefCell<Option<...>>>——但当前路径已经正确工作）
        let hb = self.host_bridge.take();
        if hb.is_some() {
            self.state.host_bridge = hb;
        }

        // 嵌套合成器更新：嵌套应用通过原生 IME 协议（XIM / im2 / im1）
        // 直接连宿主 IME daemon。mod 不参与 IME 桥接。
        // 待办：实现 XIM server（X11 应用）+ im1 global（ibus-wayland）
        //       + host_bridge 跟 im2 grab / XIM / im1 三路对接

        // host_bridge 每帧 drain 上行事件（commit/preedit/delete/lookup），
        // 灌入 relay → 原子推到 firefox 等嵌套应用的 ti3 text_input。
        if let Some(hb) = &mut self.state.host_bridge {
            for batch in hb.take_up_events_batched() {
                self.state.ime.apply_up_events(batch);
            }
        }

        let state = &mut self.state;
        let event_loop = &mut self.event_loop;
        event_loop.dispatch(Some(Duration::ZERO), state).unwrap();
        state.display_handle.flush_clients().unwrap();

        // dispatch 后把 host_bridge 取回 self.host_bridge（避免下一帧重复 take）
        self.host_bridge = state.host_bridge.take();
    }
}

delegate_compositor!(WLCState);
delegate_shm!(WLCState);
delegate_xdg_shell!(WLCState);
delegate_viewporter!(WLCState);
delegate_single_pixel_buffer!(WLCState);
delegate_dmabuf!(WLCState);
// delegate_input_method_manager!(WLCState);
// delegate_text_input_manager!(WLCState);
