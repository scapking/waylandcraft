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

mod audio;
mod bridge;
mod ddm;
mod desktop_windows;
mod portal_capture;
mod audio_capture;
mod egl;
mod host_ime;
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
    pub system_ime: Option<Box<dyn crate::host_ime::HostImBackend>>,
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
    pub requests: WindowRequests,
    pub seat: WLCSeatState,
    pub ime: ImeState,
    pub data: WLCDataState,
    pub output: WLCOutput,
    /// dmabuf 共享全局；无可用渲染节点时为 None（客户端自动回退 shm 路径）。
    pub dmabuf_global: Option<DmabufGlobal>,
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
    let mut ime_retry = false;
    let mut ime_ready = false;
    let system_ime = match crate::host_ime::probe(wayland_display) {
        crate::system_ime::ImeInit::Ready(si) => {
            eprintln!(
                "[waylandcraft][host_ime] passthrough ENABLED ({})",
                si.name()
            );
            ime_ready = true;
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

    // 初始穿透就绪状态同步给 Relay（端点选择）。
    state.ime.note_passthrough_ready(ime_ready);

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
        // 1. 游戏内会话状态 → 宿主 text-input enable/disable（命令出站）
        // 2. 宿主输入法事件（保序）→ 游戏内 active text-input（入站应用）
        // 3. 连接失效时惰性重试（每 5 秒一次，仅限暂时性故障）。
        if self.system_ime.is_none() && self.ime_retry {
            let now = std::time::Instant::now();
            let due = self
                .last_ime_retry
                .map(|t| now.duration_since(t).as_secs() >= 5)
                .unwrap_or(true);
            if due {
                self.last_ime_retry = Some(now);
                eprintln!("[waylandcraft][host_ime][RETRY] 重新探测...");
                match crate::host_ime::probe(self.wayland_display)
                {
                    crate::system_ime::ImeInit::Ready(si) => {
                        eprintln!(
                            "[waylandcraft][host_ime][RETRY] OK -> passthrough ENABLED ({})",
                            si.name()
                        );
                        // 穿透端点就绪；若游戏内已有激活会话，Relay 会补发 Activate。
                        self.state.ime.note_passthrough_ready(true);
                        self.system_ime = Some(si);
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
            // dbus 类后端异步就绪：每帧刷新端点可用性（幂等）。
            let ready = si.is_ready();
            self.state.ime.note_passthrough_ready(ready);

            // 游戏内会话状态 → 宿主 enable 门控
            let app_active = self.state.ime.app_active();
            si.set_active(app_active);

            // Relay 出站命令 → 穿透客户端（缓存 + 调和发送）
            let cmds = self.state.ime.take_passthrough_outbox();
            si.execute_commands(cmds);

            // 每帧驱动：收宿主事件 + 调和 enable/状态推送 + 按键裁决
            si.poll();

            // 后端裁决放行的按键：按提交顺序补投递给焦点应用
            // （dbus 类后端的 ProcessKeyEvent 异步往返结果，见 host_ime 模块文档）。
            for k in si.take_forwarded_keys() {
                self.state.seat.keyboard_key(k.key, k.action);
            }

            // 宿主事件（保序）→ Relay 原子应用 → 游戏内 text-input
            let events = si.take_events();
            if !events.is_empty() {
                self.state.ime.passthrough_events(events);
            }

            // 连接失效：丢弃实例，允许惰性重试重建。
            if si.is_dead() {
                eprintln!(
                    "[waylandcraft][system_ime] 连接失效 -> 将按 TRANSIENT 重试"
                );
                self.state.ime.note_passthrough_ready(false);
                self.system_ime = None;
                self.ime_retry = true;
                self.last_ime_retry = Some(std::time::Instant::now());
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
