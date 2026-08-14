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
mod ime;
mod java_types;
mod output;
mod process;
mod satellite;
mod seat;
mod svg;
mod system_ime;
mod utils;
mod xdg_spec;

pub(crate) struct WaylandCraft<'a> {
    pub state: WLCState,
    pub event_loop: EventLoop<'a, WLCState>,
    pub bridge: BridgeState,
    pub egl: EGLHelper,
    pub xdg: XDGSpecHelper,
    pub system_ime: Option<crate::system_ime::SystemIme>,
    /// 初始探测用的 wl_display 指针，供惰性重试时复用。
    pub wayland_display: usize,
    /// 穿透初始化失败后是否继续自动重试（Unsupported 后置 false）。
    pub ime_retry: bool,
    /// 上次重试时间（节流：每 5 秒最多一次）。
    pub last_ime_retry: Option<std::time::Instant>,
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
    pub dmabuf_global: DmabufGlobal,
    pub requests: WindowRequests,
    pub seat: WLCSeatState,
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
    fn new(disp: DisplayHandle, egl: &EGLHelper) -> Self {
        let compositor_state = CompositorState::new::<WLCState>(&disp);
        let shm_state = ShmState::new::<WLCState>(&disp, vec![]);
        let xdg_state = XdgShellState::new::<WLCState>(&disp);
        let viewporter_state = ViewporterState::new::<WLCState>(&disp);
        let single_pixel_buffer_state =
            SinglePixelBufferState::new::<WLCState>(&disp);

        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = init_dmabuf(&disp, &mut dmabuf_state, egl);

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
        }
    }
}

fn init_dmabuf(
    disp: &DisplayHandle,
    state: &mut DmabufState,
    egl: &EGLHelper,
) -> DmabufGlobal {
    let render_node =
        egl.get_render_node().expect("Failed to get render node!");
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

    let mut state = WLCState::new(display.handle(), &egl);
    state.socket = socket.socket_name().to_os_string();

    // 系统桌面输入法穿透：复用 GLFW 的 wl_display（Wayland 后端下才可用），
    // 或 X11/XWayland 后端自连 WAYLAND_DISPLAY。
    // 暂时性失败（连接问题）会在 update() 里自动重试，不永久跳过。
    let mut ime_retry = false;
    let system_ime = match crate::system_ime::SystemIme::new(wayland_display) {
        crate::system_ime::ImeInit::Ready(si) => {
            eprintln!("[waylandcraft][system_ime] passthrough ENABLED");
            Some(si)
        }
        crate::system_ime::ImeInit::Transient(msg) => {
            ime_retry = true;
            eprintln!(
                "[waylandcraft][system_ime] TRANSIENT: {msg} -> 将自动重试"
            );
            None
        }
        crate::system_ime::ImeInit::Unsupported(msg) => {
            eprintln!(
                "[waylandcraft][system_ime] UNSUPPORTED: {msg} -> 不再重试"
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

    let instance = WaylandCraft {
        state,
        event_loop,
        bridge: BridgeState::new(),
        egl,
        xdg,
        system_ime,
        wayland_display,
        ime_retry,
        last_ime_retry: None,
    };
    Ok(instance)
}

impl<'a> WaylandCraft<'a> {
    pub fn update(&mut self) {
        // ── 系统桌面输入法穿透桥接 ──
        // 1. 游戏内 text-input 焦点 → 系统 text-input enable/disable
        // 2. 系统输入法 commit/preedit/delete → 游戏内 active text-input
        // 3. 穿透未就绪时惰性重试（每 5 秒一次，TRANSIENT 才重试），
        //    避免启动时 WAYLAND_DISPLAY 未就绪就永久跳过。
        if self.system_ime.is_none() && self.ime_retry {
            let now = std::time::Instant::now();
            let due = self
                .last_ime_retry
                .map(|t| now.duration_since(t).as_secs() >= 5)
                .unwrap_or(true);
            if due {
                self.last_ime_retry = Some(now);
                eprintln!(
                    "[waylandcraft][system_ime][RETRY] 重新初始化..."
                );
                match crate::system_ime::SystemIme::new(self.wayland_display)
                {
                    crate::system_ime::ImeInit::Ready(si) => {
                        self.system_ime = Some(si);
                        eprintln!(
                            "[waylandcraft][system_ime][RETRY] OK -> passthrough ENABLED"
                        );
                    }
                    crate::system_ime::ImeInit::Transient(msg) => {
                        eprintln!(
                            "[waylandcraft][system_ime][RETRY] 仍不可用: {msg}"
                        );
                    }
                    crate::system_ime::ImeInit::Unsupported(msg) => {
                        eprintln!(
                            "[waylandcraft][system_ime][RETRY] 不再重试: {msg}"
                        );
                        self.ime_retry = false;
                    }
                }
            }
        }
        if let Some(si) = &mut self.system_ime {
            let active = self.state.ime.text_input_active();
            si.set_active(active);
            si.poll();

            let committed = si.take_committed();
            let preedit = si.take_preedit();
            let delete = si.take_delete();

            for text in committed {
                self.state.ime.deliver_commit_string(&text);
            }
            if let Some((text, b, e)) = preedit {
                self.state.ime.deliver_preedit_string(&text, b, e);
            }
            if let Some((b, a)) = delete {
                self.state.ime.deliver_delete_surrounding(b, a);
            }
        }

        let state = &mut self.state;
        let event_loop = &mut self.event_loop;
        event_loop.dispatch(Some(Duration::ZERO), state).unwrap();
        state.display_handle.flush_clients().unwrap();
    }
}

delegate_compositor!(WLCState);
delegate_shm!(WLCState);
delegate_xdg_shell!(WLCState);
delegate_viewporter!(WLCState);
delegate_single_pixel_buffer!(WLCState);
delegate_dmabuf!(WLCState);
