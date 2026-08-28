//! 宿主桌面输入法穿透（text-input-v3 **客户端**侧）。
//!
//! WaylandCraft 本体是合成器（服务端，见 [`crate::ime`]），本模块反过来作为
//! **宿主系统合成器的一个普通客户端**注册 `zwp_text_input_v3`：
//!
//! ```text
//! 游戏内 App(text-input-v3) ⇄ 合成器(ime/) ⇄ Relay
//!                                              │ 命令出站 / 事件入站（保序）
//!                                              ▼
//!                              SystemIme(客户端) ⇄ 宿主合成器 ⇄ 桌面输入法(fcitx5 等)
//! ```
//!
//! ## 能力检测（结构性限制，非临时降级）
//!
//! 穿透只在 Minecraft 以**原生 Wayland 后端**运行时可用：此时复用 GLFW 的
//! `wl_display`，我们的 text_input 与游戏窗口同属一个 client，宿主合成器才能
//! 把文本焦点路由过来（enter 需要 surface/client 关联）。
//!
//! Minecraft 跑 X11/XWayland 时，自建连接没有任何 wl_surface，
//! 宿主合成器的 enter 在结构上不可能到达 —— 因此直接判定 Unsupported 并给出
//! 明确原因，而不是无限轮询等待永远不会来的事件。
//!
//! ## 状态机
//!
//! - 宿主 `Enter`/`Leave`：宿主把文本焦点交给/拿走我们的 text_input。
//!   协议规定 enter 使已发送的 enable 失效，故 enter 后必须重新 enable。
//! - `want_enabled` 由游戏内会话状态驱动（[`SystemIme::set_active`]）。
//! - 每帧 [`Self::poll`] 做一次调和（reconcile）：按需发 enable/disable/
//!   状态推送 + commit。所有 wire 写入集中在这里，单一写者、无竞态。
//! - 焦点重协商是**事件驱动**的：Minecraft 窗口重新获得 OS 键盘焦点时
//!   （[`Self::notify_host_focus_gained`]），若仍处于 BLOCKED 则一次性重建
//!   text_input 触发合成器重新评估焦点。没有定时器、没有轮询。

use wayland_client::{
    backend::Backend,
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_v3::{ChangeCause, ContentHint, ContentPurpose},
    zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
    zwp_text_input_v3::{self as ti3c, ZwpTextInputV3},
};

use crate::host_ime::HostImBackend;
use crate::ime::{AppState, ImeCommand};

/// [system_ime] 日志宏：同时写 stderr 和 IME_LOG_FILE
/// （Java setImeLogFile 设置的 waylandcraft-ime.log）。
macro_rules! ime_log {
    ($($arg:tt)*) => {
        crate::bridge::ime_log_write(&format!($($arg)*))
    };
}

/// 宿主合成器 → 游戏内方向的事件（已保序）。
///
/// 文本操作与 `Done` 的相对顺序就是宿主侧的提交顺序；消费方
/// （`ImeState::passthrough_events`）依赖该顺序做原子应用。
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    Enter,
    Leave,
    CommitString(String),
    PreeditString(String, i32, i32),
    DeleteSurroundingText(u32, u32),
    /// 宿主批次完成标记；携带的 serial 仅作诊断（校验已在宿主侧完成）。
    Done(u32),
    /// 候选窗数据（ibus LookupTable / fcitx5 ClientSideUI 归一化）。
    /// 空列表 + visible=false ≡ 隐藏候选窗。
    LookupTable {
        candidates: Vec<String>,
        /// 候选序号标签（ibus 可能为空，渲染侧按 page 补 "1.".."9.","0."）。
        labels: Vec<String>,
        /// 高亮候选在【当前页内】的下标（ibus 全表绝对下标已换算；fcitx5 本页下标直用）。
        cursor_pos: u32,
        cursor_visible: bool,
        /// 每页候选数。
        page_size: u32,
        /// 0=水平 1=垂直 2=系统。
        orientation: u32,
        visible: bool,
    },
}

/// 初始化结果：区分「可重试的环境问题」和「结构性不支持」。
/// 就绪载荷为后端抽象（probe 链的产物），不再绑定具体实现。
pub enum ImeInit {
    /// 初始化成功。
    Ready(Box<dyn HostImBackend>),
    /// 暂时性失败（WAYLAND_DISPLAY 缺失/连接失败等），稍后可自动重试。
    Transient(String),
    /// 结构性不支持（所有后端均不可用），重试无意义。
    Unsupported(String),
}

/// 全字段默认值即正确初态：未连接、未进入、未启用、无缓冲事件。
#[derive(Default)]
struct SystemImeData {
    manager: Option<ZwpTextInputManagerV3>,
    seat: Option<wl_seat::WlSeat>,
    text_input: Option<ZwpTextInputV3>,

    /// 宿主是否已把文本焦点给到我们（enter/leave）。
    entered: bool,
    /// 游戏内是否有激活的文本会话（外部驱动）。
    want_enabled: bool,
    /// 是否已向宿主发送且未失效的 enable。
    sent_enable: bool,
    /// 最近缓存的 app 状态（反向同步内容）。
    last_state: AppState,
    /// last_state 是否有待推送的变化。
    state_dirty: bool,

    /// 宿主事件缓冲（保序）。
    events: Vec<HostEvent>,
    /// 连接是否已断（断开后停止一切操作）。
    dead: Option<String>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for SystemImeData {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwp_text_input_manager_v3" => {
                    let manager = registry.bind::<ZwpTextInputManagerV3, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    );
                    ime_log!(
                        "[waylandcraft][system_ime] bound zwp_text_input_manager_v3"
                    );
                    state.manager = Some(manager);
                }
                "wl_seat" => {
                    let seat = registry.bind::<wl_seat::WlSeat, _, _>(
                        name,
                        version.min(9),
                        qh,
                        (),
                    );
                    ime_log!("[waylandcraft][system_ime] bound wl_seat");
                    state.seat = Some(seat);
                }
                _ => {}
            }

            // manager 与 seat 都就绪后立即创建 text_input —— 越早创建，
            // 越可能赶在宿主首次焦点分配之前存在（KWin 只在焦点变化时广播 enter）。
            if state.text_input.is_none()
                && let (Some(manager), Some(seat)) = (&state.manager, &state.seat)
            {
                let ti = manager.get_text_input(seat, qh, ());
                state.text_input = Some(ti);
                ime_log!("[waylandcraft][system_ime] text_input created");
            }
        }
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for SystemImeData {
    fn event(
        _state: &mut Self,
        _manager: &ZwpTextInputManagerV3,
        _event: zwp_text_input_manager_v3::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for SystemImeData {
    fn event(
        _state: &mut Self,
        _seat: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpTextInputV3, ()> for SystemImeData {
    fn event(
        state: &mut Self,
        _ti: &ZwpTextInputV3,
        event: ti3c::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ti3c::Event::Enter { .. } => {
                state.entered = true;
                // 协议：enter 使 enable 状态失效，必须重新 enable。
                state.sent_enable = false;
                ime_log!("[waylandcraft][system_ime] ENTER");
            }
            ti3c::Event::Leave { .. } => {
                state.entered = false;
                state.sent_enable = false;
                ime_log!("[waylandcraft][system_ime] LEAVE");
            }
            ti3c::Event::CommitString { text } => {
                let t = text.unwrap_or_default();
                ime_log!("[waylandcraft][system_ime] host commit_string: {t:?}");
                state.events.push(HostEvent::CommitString(t));
            }
            ti3c::Event::PreeditString { text, cursor_begin, cursor_end } => {
                let t = text.unwrap_or_default();
                ime_log!(
                    "[waylandcraft][system_ime] host preedit: {t:?} cursor=({cursor_begin},{cursor_end})"
                );
                state.events.push(HostEvent::PreeditString(t, cursor_begin, cursor_end));
            }
            ti3c::Event::DeleteSurroundingText { before_length, after_length } => {
                ime_log!(
                    "[waylandcraft][system_ime] host delete_surrounding before={before_length} after={after_length}"
                );
                state
                    .events
                    .push(HostEvent::DeleteSurroundingText(before_length, after_length));
            }
            ti3c::Event::Done { serial } => {
                ime_log!("[waylandcraft][system_ime] host done serial={serial}");
                state.events.push(HostEvent::Done(serial));
            }
            _ => {}
        }
    }
}

impl SystemImeData {
    /// 把缓存的 app 状态写入宿主 text_input 的 double-buffer（不含 commit）。
    fn push_state_requests(ti: &ZwpTextInputV3, st: &AppState) {
        if !st.surrounding_text.is_empty() {
            ti.set_surrounding_text(
                st.surrounding_text.clone(),
                st.surrounding_cursor as i32,
                st.surrounding_anchor as i32,
            );
        }
        if st.change_cause != 0
            && let Some(cause) = change_cause_from_u32(st.change_cause)
        {
            ti.set_text_change_cause(cause);
        }
        if (st.content_hint != 0 || st.content_purpose != 0)
            && let Some(purpose) = content_purpose_from_u32(st.content_purpose)
        {
            let hint = ContentHint::from_bits_retain(st.content_hint);
            ti.set_content_type(hint, purpose);
        }
        if let Some((x, y, w, h)) = st.cursor_rect {
            ti.set_cursor_rectangle(x, y, w, h);
        }
    }
}

/// 协议原始值 → 客户端枚举（text-input-v3 change_cause：0=input_method 1=other）。
fn change_cause_from_u32(v: u32) -> Option<ChangeCause> {
    match v {
        0 => Some(ChangeCause::InputMethod),
        1 => Some(ChangeCause::Other),
        _ => None,
    }
}

/// 协议原始值 → 客户端枚举（与官方 XML 的 content_purpose 表一致）。
fn content_purpose_from_u32(v: u32) -> Option<ContentPurpose> {
    let p = match v {
        0 => ContentPurpose::Normal,
        1 => ContentPurpose::Alpha,
        2 => ContentPurpose::Digits,
        3 => ContentPurpose::Number,
        4 => ContentPurpose::Phone,
        5 => ContentPurpose::Url,
        6 => ContentPurpose::Email,
        7 => ContentPurpose::Name,
        8 => ContentPurpose::Password,
        9 => ContentPurpose::Pin,
        10 => ContentPurpose::Date,
        11 => ContentPurpose::Time,
        12 => ContentPurpose::Datetime,
        13 => ContentPurpose::Terminal,
        _ => return None,
    };
    Some(p)
}

/// 穿透桥接对外句柄。挂在 `WaylandCraft` 上，由 `update()` 驱动。
pub struct SystemIme {
    conn: Connection,
    queue: EventQueue<SystemImeData>,
    data: SystemImeData,
}

use wayland_client::EventQueue;

impl SystemIme {
    /// 建立到宿主合成器的客户端连接并注册 text-input-v3。
    ///
    /// 只接受「复用 GLFW 的 wl_display」这一条路径（原生 Wayland 后端）。
    pub fn connect(wl_display_ptr: usize) -> ImeInit {
        ime_log!("[waylandcraft][system_ime] init (protocol-correct rebuild)");

        if wl_display_ptr == 0 {
            // 结构性不支持：X11/XWayland 后端下自建连接没有 wl_surface，
            // 宿主合成器的 enter 需要 client/surface 关联，永远无法到达。
            // 这是能力边界而非可重试故障 —— 如实告知用户运行条件。
            let msg = "Minecraft 未以原生 Wayland 后端运行（无 GLFW wl_display）。\
                       宿主输入法穿透需要原生 Wayland 会话（例如启动器启用 Wayland 渲染）。"
                .to_string();
            ime_log!("[waylandcraft][system_ime] UNSUPPORTED: {msg}");
            return ImeInit::Unsupported(msg);
        }

        ime_log!(
            "[waylandcraft][system_ime] reuse GLFW wl_display (ptr=0x{wl_display_ptr:x})"
        );
        let backend = unsafe { Backend::from_foreign_display(wl_display_ptr as *mut _) };
        let conn = Connection::from_backend(backend);

        let mut queue = conn.new_event_queue::<SystemImeData>();
        let qh = queue.handle();

        conn.display().get_registry(&qh, ());
        let mut data = SystemImeData::default();
        if let Err(e) = queue.roundtrip(&mut data) {
            let msg = format!("registry roundtrip FAILED: {e}");
            ime_log!("[waylandcraft][system_ime] TRANSIENT: {msg}");
            return ImeInit::Transient(msg);
        }

        if data.manager.is_none() {
            let msg =
                "宿主合成器未暴露 zwp_text_input_manager_v3（无现代文本输入支持）".to_string();
            ime_log!("[waylandcraft][system_ime] UNSUPPORTED: {msg}");
            return ImeInit::Unsupported(msg);
        }
        if data.seat.is_none() {
            let msg = "宿主合成器未暴露 wl_seat".to_string();
            ime_log!("[waylandcraft][system_ime] UNSUPPORTED: {msg}");
            return ImeInit::Unsupported(msg);
        }
        if data.text_input.is_none() {
            let msg = "manager/seat 就绪但 text_input 创建失败".to_string();
            ime_log!("[waylandcraft][system_ime] TRANSIENT: {msg}");
            return ImeInit::Transient(msg);
        }

        ime_log!("[waylandcraft][system_ime] ready -> passthrough ENABLED");
        ImeInit::Ready(Box::new(Self { conn, queue, data }))
    }

    /// 游戏内是否有激活文本会话（enable 门控）。
    pub fn set_active(&mut self, active: bool) {
        if self.data.want_enabled != active {
            ime_log!(
                "[waylandcraft][system_ime] set_active {} -> {}",
                self.data.want_enabled, active
            );
            self.data.want_enabled = active;
        }
    }

    /// 执行来自 Relay 的抽象命令（lib.rs 每帧转交）。
    ///
    /// 这里只更新本地缓存与 dirty 标记；真正的 wire 写入统一在
    /// [`Self::poll`] 的调和阶段完成，保证顺序与原子性。
    pub fn execute_commands(&mut self, commands: Vec<ImeCommand>) {
        if self.data.dead.is_some() {
            return;
        }
        for cmd in commands {
            match cmd {
                ImeCommand::Activate(st) => {
                    self.data.last_state = st;
                    self.data.state_dirty = true;
                    self.data.want_enabled = true;
                }
                ImeCommand::Deactivate => {
                    self.data.want_enabled = false;
                }
                ImeCommand::PushState(st) => {
                    self.data.last_state = st;
                    self.data.state_dirty = true;
                }
            }
        }
    }

    /// Minecraft 窗口重新获得 OS 键盘焦点（Java 侧 GLFW focus 回调驱动）。
    ///
    /// 若此前因「text_input 晚于宿主焦点分配创建而收不到 enter」被卡住
    /// （KWin 已知行为），这里做**一次性**重建触发宿主重新评估焦点。
    /// 纯事件驱动，没有定时器。
    pub fn notify_host_focus_gained(&mut self) {
        if self.data.dead.is_some() || !self.data.want_enabled || self.data.entered {
            return;
        }
        ime_log!(
            "[waylandcraft][system_ime] host focus gained while BLOCKED -> recreate text_input (one-shot)"
        );
        self.recreate_text_input();
    }

    fn recreate_text_input(&mut self) {
        let qh = self.queue.handle();
        self.data.text_input = None; // drop 旧 proxy（自动 destroy）
        self.data.entered = false;
        self.data.sent_enable = false;
        if let (Some(mgr), Some(seat)) = (&self.data.manager, &self.data.seat) {
            self.data.text_input = Some(mgr.get_text_input(seat, &qh, ()));
            ime_log!("[waylandcraft][system_ime] text_input recreated");
        }
    }

    /// 每帧驱动：收宿主事件 → 调和 enable/状态 → flush。
    pub fn poll(&mut self) {
        // 1. 非阻塞读取 + 分发宿主事件到 data.events 缓冲。
        if let Some(guard) = self.queue.prepare_read()
            && let Err(e) = guard.read()
        {
            let is_wouldblock = matches!(
                &e,
                wayland_client::backend::WaylandError::Io(io)
                    if io.kind() == std::io::ErrorKind::WouldBlock
            );
            if !is_wouldblock {
                let msg = format!("read FAILED: {e} -> 连接失效");
                ime_log!("[waylandcraft][system_ime] {msg}");
                self.data.dead = Some(msg);
            }
        }
        if let Err(e) = self.queue.dispatch_pending(&mut self.data) {
            let msg = format!("dispatch_pending FAILED: {e} -> 连接失效");
            ime_log!("[waylandcraft][system_ime] {msg}");
            self.data.dead = Some(msg);
            return;
        }

        // 2. 调和：只有宿主给了 enter 才能生效 enable（协议要求）。
        if let Some(ti) = self.data.text_input.clone()
            && self.data.entered
        {
            if self.data.want_enabled && !self.data.sent_enable {
                ime_log!("[waylandcraft][system_ime] -> enable+state+commit");
                ti.enable();
                SystemImeData::push_state_requests(&ti, &self.data.last_state);
                ti.commit();
                self.data.sent_enable = true;
                self.data.state_dirty = false;
            } else if self.data.want_enabled && self.data.state_dirty {
                // 已启用下的状态增量（surrounding 变化等）：只推状态。
                SystemImeData::push_state_requests(&ti, &self.data.last_state);
                ti.commit();
                self.data.state_dirty = false;
            } else if !self.data.want_enabled && self.data.sent_enable {
                ime_log!("[waylandcraft][system_ime] -> disable+commit");
                ti.disable();
                ti.commit();
                self.data.sent_enable = false;
            }
        } else if self.data.want_enabled && self.data.sent_enable {
            // leave 之后 enable 自动失效：同步本地视图，等下次 enter 重发。
            self.data.sent_enable = false;
        }

        // 3. 冲刷请求队列（非阻塞）。
        if let Err(e) = self.queue.flush() {
            let broken = !matches!(
                &e,
                wayland_client::backend::WaylandError::Io(io)
                    if io.kind() == std::io::ErrorKind::WouldBlock
            );
            if broken {
                let msg = format!("flush FAILED: {e} -> 连接失效");
                ime_log!("[waylandcraft][system_ime] {msg}");
                self.data.dead = Some(msg);
            }
        }
    }

    /// 取走保序的宿主事件（lib.rs 灌入 `ImeState::passthrough_events`）。
    pub fn take_events(&mut self) -> Vec<HostEvent> {
        std::mem::take(&mut self.data.events)
    }

    /// 连接是否已失效（lib.rs 据此丢弃实例并允许重试初始化）。
    pub fn is_dead(&self) -> bool {
        self.data.dead.is_some()
    }

    #[allow(dead_code)]
    fn _keep_conn_alive(&self) -> &Connection {
        &self.conn
    }
}

// ── HostImBackend 适配 ────────────────────────────────────────────
// wayland-ti3 后端 = 现有 SystemIme 的薄封装。语义完全一致：
// - 不接管原始按键（宿主合成器自己处理键盘→IME 路由，我们只收文本结果）；
// - is_ready() 恒 true（connect() 成功即就绪，无异步初始化阶段）。

impl HostImBackend for SystemIme {
    fn name(&self) -> &'static str {
        "wayland-ti3"
    }

    fn is_ready(&self) -> bool {
        true
    }

    fn set_active(&mut self, active: bool) {
        Self::set_active(self, active);
    }

    fn execute_commands(&mut self, commands: Vec<ImeCommand>) {
        Self::execute_commands(self, commands);
    }

    fn notify_host_focus_gained(&mut self) {
        Self::notify_host_focus_gained(self);
    }

    fn poll(&mut self) {
        Self::poll(self);
    }

    fn take_events(&mut self) -> Vec<HostEvent> {
        Self::take_events(self)
    }

    fn is_dead(&self) -> bool {
        Self::is_dead(self)
    }
}
