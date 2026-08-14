//! 系统桌面输入法穿透（text-input-v3 客户端侧）。
//!
//! waylandcraft 本身是一个 Wayland 合成器（服务端），但这里它反过来充当
//! 系统合成器的一个 **客户端**：注册一个 `zwp_text_input_v3` 文本输入。
//!
//! 连接方式二选一：
//!   1. Minecraft 以 Wayland 后端跑 → 复用 GLFW 已建立的 `wl_display`；
//!   2. Minecraft 跑 X11/XWayland → 自己 `connect_to_env()` 连 `WAYLAND_DISPLAY`。
//!
//! 这样，当 Minecraft 窗口在系统里拥有键盘焦点时，系统合成器会把文本输入
//! 焦点路由给我们，从而激活系统输入法（ibus/fcitx）；系统输入法 commit 的
//! 文字再由这里转发回游戏内应用，实现「原生桌面输入法穿透到 Minecraft」。
//!
//! 数据流：
//!   游戏内 App(text-input-v3) ⇄ waylandcraft(服务端, ime.rs)
//!   waylandcraft(客户端, 本模块) ⇄ 系统合成器 ⇄ 系统输入法(input-method-v2)
//!
//! 关键点：get_text_input 不需要 surface 参数，surface 由合成器在 enter 事件里
//! 给出；焦点路由由「text-input 落在 seat 上、而 Minecraft 窗口拥有键盘焦点」
//! 这一事实决定。因此只需要拿到系统 `wl_display`（复用或自连均可），不需要
//! 复用 `wl_surface`。

use wayland_client::{
    backend::Backend,
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
    zwp_text_input_v3::{self, ZwpTextInputV3},
};

/// EventQueue 的 dispatch state：持有 proxy、处理事件、缓存待转发数据。
struct SystemImeData {
    manager: Option<ZwpTextInputManagerV3>,
    seat: Option<wl_seat::WlSeat>,
    text_input: Option<ZwpTextInputV3>,

    /// 合成器是否已把文本输入焦点给到 Minecraft 窗口（enter/leave）。
    entered: bool,
    /// 游戏内应用是否有 active 的 text-input（外部驱动）。
    want_enabled: bool,
    /// 是否已向系统发送 enable（double-buffer，随 commit 生效）。
    enabled: bool,

    /// 系统输入法 commit 的文字，待转发给游戏内应用。
    committed: Vec<String>,
    /// 系统输入法 preedit（拼音候选），待转发。
    preedit: Option<(String, i32, i32)>,
    /// 系统输入法请求删除的环绕文本，待转发。
    delete_surrounding: Option<(u32, u32)>,
    /// 上次打印 BLOCKED 提示的时间（节流用，避免每帧刷屏）。
    last_blocked_log: Option<std::time::Instant>,
}

impl Default for SystemImeData {
    fn default() -> Self {
        Self {
            manager: None,
            seat: None,
            text_input: None,
            entered: false,
            want_enabled: false,
            enabled: false,
            committed: Vec::new(),
            preedit: None,
            delete_surrounding: None,
            last_blocked_log: None,
        }
    }
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
            eprintln!(
                "[waylandcraft][system_ime] global: {} v{} (name={})",
                interface, version, name
            );
            match interface.as_str() {
                "zwp_text_input_manager_v3" => {
                    let manager = registry.bind::<ZwpTextInputManagerV3, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    );
                    state.manager = Some(manager);
                    eprintln!(
                        "[waylandcraft][system_ime] bound zwp_text_input_manager_v3"
                    );
                }
                "wl_seat" => {
                    let seat = registry.bind::<wl_seat::WlSeat, _, _>(
                        name,
                        version.min(9),
                        qh,
                        (),
                    );
                    state.seat = Some(seat);
                    eprintln!(
                        "[waylandcraft][system_ime] bound wl_seat (name={})",
                        name
                    );
                }
                _ => {}
            }

            // manager 与 seat 都就绪后创建 text-input。
            if state.text_input.is_none() {
                if let (Some(manager), Some(seat)) =
                    (&state.manager, &state.seat)
                {
                    let ti = manager.get_text_input(seat, qh, ());
                    state.text_input = Some(ti);
                    eprintln!(
                        "[waylandcraft][system_ime] created text_input (seat name={})",
                        name
                    );
                }
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
        // manager 无事件。
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
        // 只借用 seat 对象，不关心其事件。
    }
}

impl Dispatch<ZwpTextInputV3, ()> for SystemImeData {
    fn event(
        state: &mut Self,
        _ti: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { surface } => {
                state.entered = true;
                // enter 后必须重新 enable（协议：enter 使 enable 状态失效）。
                state.enabled = false;
                let sid = surface.id();
                eprintln!(
                    "[waylandcraft][system_ime] ENTER: text-input focus -> surface id={sid:?} (entered=true)"
                );
            }
            zwp_text_input_v3::Event::Leave { .. } => {
                state.entered = false;
                state.enabled = false;
                eprintln!(
                    "[waylandcraft][system_ime] LEAVE: text-input focus lost (entered=false)"
                );
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                let t = text.unwrap_or_default();
                eprintln!(
                    "[waylandcraft][system_ime] commit_string: {:?}",
                    t
                );
                state.committed.push(t);
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                let t = text.unwrap_or_default();
                eprintln!(
                    "[waylandcraft][system_ime] preedit_string: {:?} cursor=({cursor_begin},{cursor_end})",
                    t
                );
                state.preedit = Some((t, cursor_begin, cursor_end));
            }
            zwp_text_input_v3::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                eprintln!(
                    "[waylandcraft][system_ime] delete_surrounding: before={before_length} after={after_length}"
                );
                state.delete_surrounding =
                    Some((before_length, after_length));
            }
            zwp_text_input_v3::Event::Done { serial } => {
                // 合成器确认状态变更（含 enable 生效）都会回一个 done；
                // serial 不断增长 = 合成器正在响应，可用于确认 enable 已生效。
                eprintln!(
                    "[waylandcraft][system_ime][EVENT] done serial={serial}"
                );
            }
            _ => {}
        }
    }
}

/// 初始化结果：区分「可重试的环境问题」和「重试无意义的协议缺失」。
pub enum ImeInit {
    /// 初始化成功。
    Ready(SystemIme),
    /// 暂时性失败（WAYLAND_DISPLAY 缺失/连接失败等），稍后可自动重试。
    Transient(String),
    /// 合成器不支持（无 text-input-v3 / 无 seat），重试无意义。
    Unsupported(String),
}

/// 穿透桥接的对外句柄。挂在 `WaylandCraft` 上，在 `update()` 里驱动。
pub struct SystemIme {
    conn: Connection,
    queue: EventQueue<SystemImeData>,
    data: SystemImeData,
}

impl SystemIme {
    /// 建立到系统合成器的 guest 客户端连接并注册 text-input-v3。
    ///
    /// 优先复用 GLFW 的 `wl_display`（Minecraft 以 Wayland 后端跑时）；
    /// 拿不到（Minecraft 跑 X11/XWayland 后端）则自己 `connect_to_env()`
    /// 连 `WAYLAND_DISPLAY`——text-input-v3 是 seat 级协议，不依赖
    /// surface/连接来源，只要 Minecraft 窗口在系统合成器里有键盘焦点，
    /// 合成器就会给本连接的 text_input 发 enter。
    pub fn new(wl_display_ptr: usize) -> ImeInit {
        eprintln!(
            "[waylandcraft][system_ime][BUILD] log-v3 (全流程检查点+自动重试)"
        );
        eprintln!(
            "[waylandcraft][system_ime][PHASE=probe] wl_display_ptr=0x{:x} (0x0 => Minecraft X11/XWayland 后端)",
            wl_display_ptr
        );

        let conn = if wl_display_ptr != 0 {
            // Minecraft Wayland 后端：复用 GLFW 已建立的连接。
            eprintln!(
                "[waylandcraft][system_ime][PHASE=connect] 复用 GLFW wl_display (guest mode)"
            );
            let backend = unsafe {
                Backend::from_foreign_display(wl_display_ptr as *mut _)
            };
            Connection::from_backend(backend)
        } else {
            // Minecraft X11/XWayland 后端：自己连系统 Wayland 桌面。
            let env = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
            eprintln!(
                "[waylandcraft][system_ime][PHASE=connect] connect_to_env() WAYLAND_DISPLAY={:?}",
                env
            );
            match Connection::connect_to_env() {
                Ok(c) => {
                    eprintln!(
                        "[waylandcraft][system_ime][PHASE=connect] OK: 已连上系统 Wayland 合成器"
                    );
                    c
                }
                Err(e) => {
                    let msg = format!(
                        "connect_to_env FAILED: {e} (Minecraft 进程拿不到 WAYLAND_DISPLAY, 启动器未继承)"
                    );
                    eprintln!(
                        "[waylandcraft][system_ime][PHASE=connect][ERROR] {msg}"
                    );
                    return ImeInit::Transient(msg);
                }
            }
        };

        let mut queue = conn.new_event_queue::<SystemImeData>();
        let qh = queue.handle();

        // 请求 registry，随后 roundtrip 收集 globals 并 bind manager/seat。
        conn.display().get_registry(&qh, ());
        let mut data = SystemImeData::default();
        eprintln!(
            "[waylandcraft][system_ime][PHASE=registry] 请求 globals... (roundtrip)"
        );
        if let Err(e) = queue.roundtrip(&mut data) {
            let msg = format!(
                "registry roundtrip FAILED: {e} (合成器断开/协议错误)"
            );
            eprintln!(
                "[waylandcraft][system_ime][PHASE=registry][ERROR] {msg}"
            );
            return ImeInit::Transient(msg);
        }

        eprintln!(
            "[waylandcraft][system_ime][PHASE=registry] done: manager={}, seat={}, text_input={}",
            data.manager.is_some(),
            data.seat.is_some(),
            data.text_input.is_some(),
        );

        if data.manager.is_none() {
            let msg = "合成器未暴露 zwp_text_input_manager_v3 (KWin<6.6 / 无 text-input 支持)".to_string();
            eprintln!(
                "[waylandcraft][system_ime][PHASE=registry][ERROR] {msg}"
            );
            return ImeInit::Unsupported(msg);
        }
        if data.seat.is_none() {
            let msg = "合成器未暴露 wl_seat".to_string();
            eprintln!(
                "[waylandcraft][system_ime][PHASE=registry][ERROR] {msg}"
            );
            return ImeInit::Unsupported(msg);
        }
        if data.text_input.is_none() {
            let msg = "manager/seat 就绪但未创建 text_input".to_string();
            eprintln!(
                "[waylandcraft][system_ime][PHASE=registry][ERROR] {msg}"
            );
            return ImeInit::Unsupported(msg);
        }

        eprintln!(
            "[waylandcraft][system_ime][PHASE=init] OK -> passthrough ready"
        );
        ImeInit::Ready(Self { conn, queue, data })
    }

    /// 游戏内应用是否有 active text-input（由 ime 状态驱动）。
    pub fn set_active(&mut self, active: bool) {
        if self.data.want_enabled != active {
            eprintln!(
                "[waylandcraft][system_ime] set_active: {} -> {}",
                self.data.want_enabled, active
            );
        }
        self.data.want_enabled = active;
    }

    /// 每帧非阻塞地收发系统端事件，并应用 enable/disable 状态机。
    pub fn poll(&mut self) {
        if let Some(guard) = self.queue.prepare_read() {
            // 非阻塞读：WouldBlock 表示暂无新数据（正常，忽略），
            // 其余错误必须打出来，否则会静默丢事件。
            if let Err(e) = guard.read() {
                let is_wouldblock = matches!(
                    &e,
                    wayland_client::backend::WaylandError::Io(io)
                        if io.kind() == std::io::ErrorKind::WouldBlock
                );
                if !is_wouldblock {
                    eprintln!(
                        "[waylandcraft][system_ime][PHASE=poll][ERROR] read FAILED: {e}"
                    );
                }
            }
        }
        if let Err(e) = self.queue.dispatch_pending(&mut self.data) {
            eprintln!(
                "[waylandcraft][system_ime][PHASE=poll][ERROR] dispatch_pending FAILED: {e} (合成器协议错误/断开)"
            );
        }

        // 状态机：只有合成器给了 enter（Minecraft 窗口有焦点）才 enable。
        if let Some(ti) = &self.data.text_input {
            if self.data.entered {
                if self.data.want_enabled && !self.data.enabled {
                    eprintln!(
                        "[waylandcraft][system_ime] state: entered=true want_enabled=true enabled=false -> ENABLE+commit"
                    );
                    ti.enable();
                    ti.commit();
                    self.data.enabled = true;
                } else if !self.data.want_enabled && self.data.enabled {
                    eprintln!(
                        "[waylandcraft][system_ime] state: want_enabled=false enabled=true -> DISABLE+commit"
                    );
                    ti.disable();
                    ti.commit();
                    self.data.enabled = false;
                }
            } else if self.data.want_enabled {
                // 节流：最多每 5 秒打一次，避免每帧刷屏。
                let now = std::time::Instant::now();
                let due = self
                    .data
                    .last_blocked_log
                    .map(|t| now.duration_since(t).as_secs() >= 5)
                    .unwrap_or(true);
                if due {
                    eprintln!(
                        "[waylandcraft][system_ime] BLOCKED: want_enabled=true but entered=false (compositor never sent ENTER)"
                    );
                    self.data.last_blocked_log = Some(now);
                }
            }
        }
        if let Err(e) = self.queue.flush() {
            eprintln!(
                "[waylandcraft][system_ime] flush ERROR: {e}"
            );
        }
    }

    /// 取出系统输入法 commit 的文字（转发给游戏内应用）。
    pub fn take_committed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.data.committed)
    }

    /// 取出系统输入法 preedit（拼音候选）。
    pub fn take_preedit(&mut self) -> Option<(String, i32, i32)> {
        self.data.preedit.take()
    }

    /// 取出系统输入法请求删除的环绕文本范围。
    pub fn take_delete(&mut self) -> Option<(u32, u32)> {
        self.data.delete_surrounding.take()
    }

    #[allow(dead_code)]
    fn _keep_conn_alive(&self) -> &Connection {
        &self.conn
    }
}
