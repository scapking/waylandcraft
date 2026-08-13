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
    Connection, Dispatch, EventQueue, QueueHandle,
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
            match interface.as_str() {
                "zwp_text_input_manager_v3" => {
                    let manager = registry.bind::<ZwpTextInputManagerV3, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
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
                    state.seat = Some(seat);
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
            zwp_text_input_v3::Event::Enter { .. } => {
                state.entered = true;
                // enter 后必须重新 enable（协议：enter 使 enable 状态失效）。
                state.enabled = false;
            }
            zwp_text_input_v3::Event::Leave { .. } => {
                state.entered = false;
                state.enabled = false;
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                state.committed.push(text.unwrap_or_default());
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                state.preedit =
                    Some((text.unwrap_or_default(), cursor_begin, cursor_end));
            }
            zwp_text_input_v3::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                state.delete_surrounding =
                    Some((before_length, after_length));
            }
            zwp_text_input_v3::Event::Done { .. } => {}
            _ => {}
        }
    }
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
    /// 失败（桌面无 Wayland / 合成器无 text-input-v3）返回 None。
    pub fn new(wl_display_ptr: usize) -> Option<Self> {
        let conn = if wl_display_ptr != 0 {
            // Minecraft Wayland 后端：复用 GLFW 已建立的连接。
            let backend = unsafe {
                Backend::from_foreign_display(wl_display_ptr as *mut _)
            };
            Connection::from_backend(backend)
        } else {
            // Minecraft X11/XWayland 后端：自己连系统 Wayland 桌面。
            Connection::connect_to_env().ok()?
        };

        let mut queue = conn.new_event_queue::<SystemImeData>();
        let qh = queue.handle();

        // 请求 registry，随后 roundtrip 收集 globals 并 bind manager/seat。
        conn.display().get_registry(&qh, ());
        let mut data = SystemImeData::default();
        queue.roundtrip(&mut data).ok()?;

        if data.text_input.is_none() {
            eprintln!(
                "[waylandcraft] system IME: no zwp_text_input_v3 support"
            );
            return None;
        }

        eprintln!("[waylandcraft] system IME passthrough: ready");
        Some(Self { conn, queue, data })
    }

    /// 游戏内应用是否有 active text-input（由 ime 状态驱动）。
    pub fn set_active(&mut self, active: bool) {
        self.data.want_enabled = active;
    }

    /// 每帧非阻塞地收发系统端事件，并应用 enable/disable 状态机。
    pub fn poll(&mut self) {
        if let Some(guard) = self.queue.prepare_read() {
            // 非阻塞读：无新数据时返回 WouldBlock，忽略即可。
            let _ = guard.read();
        }
        let _ = self.queue.dispatch_pending(&mut self.data);

        // 状态机：只有合成器给了 enter（Minecraft 窗口有焦点）才 enable。
        if let Some(ti) = &self.data.text_input {
            if self.data.entered {
                if self.data.want_enabled && !self.data.enabled {
                    ti.enable();
                    ti.commit();
                    self.data.enabled = true;
                } else if !self.data.want_enabled && self.data.enabled {
                    ti.disable();
                    ti.commit();
                    self.data.enabled = false;
                }
            }
        }
        let _ = self.queue.flush();
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
