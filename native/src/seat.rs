use crate::WLCState;
use crate::utils::{get_time, new_serial, to_fixed2};
use smithay::{
    reexports::{
        wayland_protocols::wp::cursor_shape::v1::server::{
            wp_cursor_shape_device_v1,
            wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
            wp_cursor_shape_manager_v1,
            wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
        },
        wayland_protocols::wp::pointer_constraints::zv1::server::{
            zwp_confined_pointer_v1 as zwp_confined,
            zwp_confined_pointer_v1::ZwpConfinedPointerV1,
            zwp_locked_pointer_v1 as zwp_locked,
            zwp_locked_pointer_v1::ZwpLockedPointerV1,
            zwp_pointer_constraints_v1 as zwp_constraints,
            zwp_pointer_constraints_v1::ZwpPointerConstraintsV1,
        },
        wayland_protocols::wp::relative_pointer::zv1::server::{
            zwp_relative_pointer_manager_v1 as zwp_rpm,
            zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
            zwp_relative_pointer_v1 as zwp_relpointer,
            zwp_relative_pointer_v1::ZwpRelativePointerV1,
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
            Resource,
            backend::ClientId,
            protocol::{
                wl_keyboard::{self, KeyState, KeymapFormat, WlKeyboard},
                wl_pointer::{self, Axis, AxisSource, ButtonState, WlPointer},
                wl_seat::{self, WlSeat},
                wl_surface::WlSurface,
            },
        },
    },
    utils::SealedFile,
};
use std::collections::HashSet;
use std::ffi::CString;
use std::ops::DerefMut;
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};
use xkbcommon::xkb::{self, Keymap};

pub struct WLCSeatState {
    pub pointers: Vec<WlPointer>,
    pub keyboards: Vec<WlKeyboard>,
    pub kb_active: bool,
    pub pressed_keys: HashSet<u32>,
    pub keymap: Keymap,
    pub keymap_file: SealedFile,
    pub xkb_context: xkb::Context,
    pub xkb_state: xkb::State,
    pub cursor_shape: Option<u32>,
    /// 诊断：kb_active 但所有 keyboard 都无 focus 时只 warn 一次，避免刷屏
    pub no_focus_warned: std::cell::Cell<bool>,
}

pub struct WLCPointerData {
    // WlSurface holding pointer focus
    // This surface has to be of the same client as the WlPointer
    focus: Option<WlSurface>,
    // Value of current pointer focus enter serial
    last_enter: Option<u32>,
    // Value of last motion event wl_fixed
    last_motion: Option<(i32, i32)>,
    // Relative pointer objects
    relative_pointers: Vec<ZwpRelativePointerV1>,
    // Pointer position lock
    lock: Option<WLCPointerLock>,
    // Pointer confined surface
    confined: Option<WlSurface>,
}

type WLCPointer = Arc<Mutex<WLCPointerData>>;

pub struct WLCCursorShapeDeviceData {
    pointer: Option<WlPointer>,
}

type WLCCursorShapeDevice = Arc<Mutex<WLCCursorShapeDeviceData>>;

pub struct WLCPointerLock {
    locked_pointer: ZwpLockedPointerV1,
    surface: WlSurface,
    active: bool, // Activated event sent
}

pub struct WLCKeyboardData {
    // WlSurface holding keyboard focus
    // This surface has to be of the same client as the WlKeyboard
    focus: Option<WlSurface>,
}

type WLCKeyboard = Arc<Mutex<WLCKeyboardData>>;

// Keyboard RMLVO keymap specifier
#[allow(clippy::upper_case_acronyms)]
#[derive(Default)]
pub struct RMLVO {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: String,
}

/// 键盘动作三态。与 Java 侧 bridge 的 action 约定一致：
/// `0 = release`、`1 = press`、`2 = repeat`。
/// Java 侧 `pressKey/releaseKey` 已按此传参，`repeatKey` 会调
/// `keyboardInput(instance, scancode, 2)` 走 Repeat 分支。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardAction {
    Release = 0,
    Press = 1,
    Repeat = 2,
}

impl KeyboardAction {
    /// 从 bridge 收到的 `jint` action 还原为三态；非法值返回 `None`。
    pub fn from_i32(action: i32) -> Option<Self> {
        match action {
            0 => Some(Self::Release),
            1 => Some(Self::Press),
            2 => Some(Self::Repeat),
            _ => None,
        }
    }

    /// 发到 wire 的 `wl_keyboard.key` state。
    /// Repeat 复用 `Pressed`：客户端看到按键保持按下；但 xkb 状态机不被改变
    /// （repeat 不重新触发 Caps Lock 切换、不重复累加 Shift/Ctrl 按下），
    /// 该不变式由 `keyboard_key` 保证 —— 它从不更新 xkb_state。
    pub fn key_state(self) -> KeyState {
        match self {
            Self::Release => KeyState::Released,
            Self::Press | Self::Repeat => KeyState::Pressed,
        }
    }
}

fn with_pointer_data<F, R>(pointer: &WlPointer, f: F) -> R
where
    F: FnOnce(&mut WLCPointerData) -> R,
{
    let mut guard = pointer.data::<WLCPointer>().unwrap().lock().unwrap();
    let data = guard.deref_mut();
    f(data)
}

fn with_cursor_shape_device_data<F, R>(
    device: &WpCursorShapeDeviceV1,
    f: F,
) -> R
where
    F: FnOnce(&mut WLCCursorShapeDeviceData) -> R,
{
    let mut guard = device
        .data::<WLCCursorShapeDevice>()
        .unwrap()
        .lock()
        .unwrap();
    let data = guard.deref_mut();
    f(data)
}

fn with_keyboard_data<F>(keyboard: &WlKeyboard, f: F)
where
    F: FnOnce(&mut WLCKeyboardData),
{
    let mut guard = keyboard.data::<WLCKeyboard>().unwrap().lock().unwrap();
    let data = guard.deref_mut();
    f(data);
}

fn create_keymap_file(keymap: &Keymap) -> SealedFile {
    let keymap_str = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    SealedFile::with_content(
        c"waylandcraft-keymap",
        &CString::new(keymap_str.as_str()).unwrap(),
    )
    .expect("SealedFile create")
}

impl WLCSeatState {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let xkb_context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = Keymap::new_from_names(
            &xkb_context,
            "",                           // rules
            "",                           // model
            "",                           // layout
            "",                           // variant
            None,                         // options
            xkb::KEYMAP_COMPILE_NO_FLAGS, // flags
        )
        .expect("default keymap create");

        let xkb_state = xkb::State::new(&keymap);
        let keymap_file = create_keymap_file(&keymap);

        WLCSeatState {
            pointers: vec![],
            keyboards: vec![],
            kb_active: false,
            pressed_keys: HashSet::new(),
            keymap,
            keymap_file,
            xkb_context,
            xkb_state,
            cursor_shape: None,
            no_focus_warned: std::cell::Cell::new(false),
        }
    }

    pub fn create_globals(&self, disp: &DisplayHandle) {
        disp.create_global::<WLCState, WlSeat, ()>(8, ());
        disp.create_global::<WLCState, ZwpRelativePointerManagerV1, ()>(1, ());
        disp.create_global::<WLCState, ZwpPointerConstraintsV1, ()>(1, ());
        disp.create_global::<WLCState, WpCursorShapeManagerV1, ()>(2, ());
    }

    fn pointer_frame(&self, pointer: &WlPointer) {
        if pointer.version() >= wl_pointer::EVT_FRAME_SINCE {
            pointer.frame();
        }
    }

    fn pointer_focus_eq(
        &self,
        pointer: &WLCPointerData,
        surface: &WlSurface,
    ) -> bool {
        pointer.focus.as_ref().is_some_and(|s| s == surface)
    }

    fn pointer_focus(&mut self, surface: Option<&WlSurface>, x: f64, y: f64) {
        let serial = new_serial();

        // Unfocus any pointers currently focused on the wrong surface
        self.for_all_pointers(|pointer, data| {
            let focus = match &data.focus {
                Some(s) => s,
                None => return,
            };
            let unfocus = match surface {
                Some(s) => s != focus,
                None => true,
            };
            if unfocus {
                pointer.leave(serial, focus);
                self.pointer_frame(pointer);
                data.focus = None;
                data.last_enter = None;
                data.last_motion = None;
            }
        });

        let surface = match surface {
            Some(s) => s,
            None => return,
        };

        // Generate pointer enter events
        self.for_all_pointers(|pointer, data| {
            // Already correct focus
            if self.pointer_focus_eq(data, surface) {
                return;
            }
            assert_eq!(data.focus, None);

            // Client does not own surface
            if surface.client() != pointer.client() {
                return;
            }

            pointer.enter(serial, surface, x, y);
            self.pointer_frame(pointer);
            data.focus = Some(surface.clone());
            data.last_enter = Some(serial);
            data.last_motion = None;
        });
    }

    // Focus the pointer on the given surface and register movement
    pub fn pointer_motion_focus(
        &mut self,
        surface: Option<&WlSurface>,
        x: f64,
        y: f64,
    ) {
        let surface = surface.filter(|s| s.is_alive());

        self.pointer_focus(surface, x, y);
        if surface.is_none() {
            return;
        }

        self.pointer_motion(x, y);
    }

    // Send motion events
    pub fn pointer_motion(&mut self, x: f64, y: f64) {
        let time = get_time();
        let pos: (i32, i32) = to_fixed2(x, y);
        self.for_all_pointers(|pointer, data| {
            // Pointer does not hold focus
            if data.focus.is_none() {
                return;
            }
            // Pointer location did not change
            if data.last_motion == Some(pos) {
                return;
            }

            pointer.motion(time, x, y);
            self.pointer_frame(pointer);
            data.last_motion = Some(pos);
        });
    }

    // Emit relative movement on the surface with active pointer focus
    pub fn pointer_relative_motion(&self, dx: f64, dy: f64) {
        self.for_all_pointers(|pointer, data| {
            if data.focus.is_none() {
                return;
            }
            for relative_pointer in &data.relative_pointers {
                let time = (get_time() as u64) * 1000; // ms to µs
                relative_pointer.relative_motion(
                    (time >> 32) as u32,        // utime_hi
                    (time & 0xffffffff) as u32, // utime_lo
                    dx,                         // dx
                    dy,                         // dy
                    dx,                         // dx_unaccel
                    dy,                         // dy_unaccel
                );
            }
            self.pointer_frame(pointer);
        });
    }

    pub fn pointer_button(&mut self, button: u32, state: ButtonState) -> u32 {
        let serial = new_serial();
        self.for_all_pointers(|pointer, data| {
            if data.focus.is_none() {
                return;
            }

            pointer.button(serial, get_time(), button, state);
            self.pointer_frame(pointer);
        });
        serial
    }

    pub fn pointer_axis(&self, axis: Axis, value: f64) {
        let val120 = (value * 120.0).floor() as i32;
        if val120 == 0 { return }

        self.for_all_pointers(|pointer, data| {
            if data.focus.is_some() {
                let version = pointer.version();
                if version >= wl_pointer::EVT_AXIS_SOURCE_SINCE {
                    pointer.axis_source(AxisSource::Wheel);
                }
                if version >= wl_pointer::EVT_AXIS_VALUE120_SINCE {
                    pointer.axis_value120(axis, val120);
                } else if version >= wl_pointer::EVT_AXIS_DISCRETE_SINCE {
                    pointer.axis_discrete(axis, value.floor() as i32);
                }
                pointer.axis(get_time(), axis, value * 10.0);
                self.pointer_frame(pointer);
            }
        });
    }

    pub fn keyboard_update_xkb(&mut self, key: u32, pressed: bool) {
        // Java 侧 correctScancode 在 wayland 平台给 scancode +8（X11 风格键码），
        // keyboard_key 发 wire 事件时用 `key - 8` 还原为 evdev/wayland 键码。
        // xkb_state 必须用与 wl_keyboard.key 完全相同的键码更新，否则修饰键
        // （Shift/Ctrl/Alt/Super）的 mods 状态全部错位 → 应用永远收不到组合键：
        // 单键正常（key 事件照发），但 Ctrl+B / Shift+字母 等快捷键全部失效。
        // 修复前这里直接用 key（evdev+8）更新 xkb → serialize_mods 报 0 修饰。
        // 只有 PRESS/RELEASE 会走到这里（Java 侧 KeyboardHandlerMixin 只在
        // GLFW_PRESS/GLFW_RELEASE 时调 keyboardUpdate；REPEAT 不调），因此
        // repeat 天然不会重复改变状态机 —— 这正是"长按不重复触发 Caps Lock
        // 切换、不重复进入 Shift/Ctrl"的关键。
        let code = xkb::Keycode::new(key.saturating_sub(8));
        let dir = match pressed {
            true => xkb::KeyDirection::Down,
            false => xkb::KeyDirection::Up,
        };
        self.xkb_state.update_key(code, dir);

        // pressed_keys 用于 keyboard.enter 的按下键数组，同样必须是 wire 键码（evdev）
        let wire = key.saturating_sub(8);
        if pressed {
            self.pressed_keys.insert(wire);
        } else {
            self.pressed_keys.remove(&wire);
        }
    }

    pub fn keyboard_focus(&mut self, surface: WlSurface) {
        if !surface.is_alive() {
            eprintln!("[kb-debug] keyboard_focus: surface NOT ALIVE —— 焦点设置失败！");
            return;
        };
        let client = surface.client().unwrap();
        let serial = new_serial();

        let mut matched_any = false;
        let mut entered_any = false;
        let mut left_any = false;
        self.for_all_keyboards(|keyboard, data| {
            let keyboard_client = keyboard.client().unwrap();

            // If WlKeyboard belongs to different client, make it lose focus
            if keyboard_client != client {
                if let Some(focus) = &data.focus {
                    keyboard.leave(serial, focus);
                    data.focus = None;
                    left_any = true;
                    eprintln!("[kb-debug] keyboard_focus: keyboard(client {:?}) lost focus (surface 属于另一 client)", keyboard_client.id());
                }
                return;
            }
            matched_any = true;

            // This keyboard is now guaranteed to be of the same client as the
            // surface

            if let Some(focus) = &data.focus {
                if *focus == surface {
                    // Surface already focused —— 幂等，不打日志（避免每帧刷屏）
                    return;
                }
                keyboard.leave(serial, focus);
                data.focus = None;
                left_any = true;
            }

            // Keyboard should enter surface
            let pressed = self.serialize_pressed_keys();

            keyboard.enter(serial, &surface, pressed);
            data.focus = Some(surface.clone());
            entered_any = true;

            self.send_modifiers(keyboard, serial);
        });
        // 只在真正发生 enter/leave 时打（tick 每帧 focusSurface 是幂等已聚焦 → 静默）
        if entered_any || left_any {
            eprintln!(
                "[kb-debug] keyboard_focus: client={:?} keyboards={} matched={} entered={} left={}",
                client.id(),
                self.keyboards.len(),
                matched_any,
                entered_any,
                left_any
            );
        }
    }

    fn serialize_pressed_keys(&self) -> Vec<u8> {
        let mut pressed: Vec<u32> = vec![];
        if self.kb_active {
            pressed = self.pressed_keys.iter().copied().collect();
        }

        let pressed: Vec<u8> =
            pressed.iter().flat_map(|&k| k.to_ne_bytes()).collect();

        pressed
    }

    fn keyboard_refocus(&mut self) {
        let serial = new_serial();
        self.for_all_keyboards(|keyboard, data| {
            if let Some(focus) = &data.focus {
                if !focus.is_alive() {
                    return;
                }

                let pressed = self.serialize_pressed_keys();
                keyboard.leave(serial, focus);
                keyboard.enter(serial, focus, pressed);
                self.send_modifiers(keyboard, serial);
            }
        });
    }

    pub fn activate_keyboard(&mut self) {
        if self.kb_active {
            return;
        }

        self.kb_active = true;
        self.keyboard_refocus();
    }

    pub fn deactivate_keyboard(&mut self) {
        if !self.kb_active {
            return;
        }

        self.kb_active = false;
        self.keyboard_refocus();
    }

    /// 发送 `wl_keyboard.modifiers`。四个字段与 xkb_state 的对应关系（wl 协议
    /// 位掩码与 xkb 的 ModMask 一致：Shift=0、Lock=1、Control=2、Mod1(Alt)=3…）：
    ///   - depressed = 当前按住的瞬时修饰键（Shift / Ctrl / Alt / Super …）
    ///   - latched   = 被 latch 的修饰键（一般不用，如实上报）
    ///   - locked    = 锁定修饰键（Caps Lock / Num Lock …）
    ///   - group     = 当前布局索引（xkb layout，serialize_layout 返回 0 基索引）
    ///
    /// xkb_state 由 `keyboard_update_xkb` 用与 wire 事件相同的 evdev 键码更新，
    /// 因此这里序列化出的掩码与实际按键一一对应：按下 Caps Lock 时 xkb 内部切换
    /// Lock 位 → `serialize_mods(STATE_MODS_LOCKED)` 上报 LockMask，客户端据此
    /// 切换大小写；Shift/Ctrl/Alt 按下 → `serialize_mods(STATE_MODS_DEPRESSED)`。
    fn send_modifiers(&self, keyboard: &WlKeyboard, serial: u32) {
        if !self.kb_active {
            keyboard.modifiers(
                serial,
                0, // MODS_DEPRESSED
                0, // MODS_LATCHED
                0, // MODS_LOCKED
                self.xkb_state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            );
            return;
        }
        keyboard.modifiers(
            serial,
            self.xkb_state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            self.xkb_state.serialize_mods(xkb::STATE_MODS_LATCHED),
            self.xkb_state.serialize_mods(xkb::STATE_MODS_LOCKED),
            self.xkb_state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
        );
    }

    pub fn keyboard_unfocus(&mut self) {
        let serial = new_serial();
        self.for_all_keyboards(|keyboard, data| {
            if let Some(focus) = &data.focus {
                keyboard.leave(serial, focus);
                data.focus = None;
            }
        });
    }

    /// 向所有聚焦的 wl_keyboard 客户端发送 key 事件，并紧随其后发送 modifiers。
    ///
    /// **这里绝不改动 xkb_state 与 pressed_keys** —— 它们只由
    /// `keyboard_update_xkb`（keyboardUpdate）在按下/释放时维护。正因为如此，
    /// `KeyboardAction::Repeat` 只会向客户端补发一次 `Pressed` 键事件，而不会
    /// 重复改变状态机（不重复触发 Caps Lock 切换、不重复累加修饰键按下）。
    /// Java 侧保证在调用本函数前已先调 keyboardUpdate 同步状态机。
    ///
    /// 发 wire 事件时把键码还原为 evdev（`key - 8`，与 keyboard_update_xkb
    /// 中更新 xkb_state 所用的键码完全一致），保证 modifiers 序列化
    /// （MODS_DEPRESSED / MODS_LOCKED）与 key 事件一一对应。
    pub fn keyboard_key(&self, key: u32, action: KeyboardAction) {
        let wire_key = key.saturating_sub(8);
        if !self.kb_active {
            eprintln!(
                "[kb-debug] keyboard_key: DROPPED —— kb_active=false (key={} wire={} action={:?})",
                key, wire_key, action
            );
            return;
        }
        let serial = new_serial();
        let state = action.key_state();
        let mut any_focus = false;
        let mut focus_descs = String::new();
        self.for_all_keyboards(|keyboard, data| {
            if data.focus.is_some() {
                any_focus = true;
                focus_descs.push_str(&format!(" kb{}:focused", keyboard.id()));
                keyboard.key(serial, get_time(), wire_key, state);
                self.send_modifiers(keyboard, serial);
            } else {
                focus_descs.push_str(&format!(" kb{}:NO_FOCUS", keyboard.id()));
            }
        });
        // [kb-debug] 每键打：确认 Rust 收到 + 有没有 focus 可发 + 当前修饰键状态。
        // mods(depressed=X locked=Y)：X/Y 来自 xkb_state —— 按 Caps Lock 后 locked 应变 1
        // （窗口大小写切换靠它）；按 Ctrl/Shift 后 depressed 应变 1/2/4…（快捷键靠它）。
        // 任何键盘有 focus → key 事件已发出；全 NO_FOCUS → 按键被丢弃（Java 侧 focusSurface 没生效）。
        let depressed = self.xkb_state.serialize_mods(xkb::STATE_MODS_DEPRESSED);
        let locked = self.xkb_state.serialize_mods(xkb::STATE_MODS_LOCKED);
        eprintln!(
            "[kb-debug] keyboard_key: key={} wire={} action={:?} kb_active=true mods(depressed={} locked={}) |{}{}",
            key, wire_key, action, depressed, locked, focus_descs,
            if any_focus { " -> SENT" } else { " -> DROPPED(no focus)" }
        );
        // 诊断：键盘已激活但没有任何 wl_keyboard 有焦点 —— 按键被静默丢弃
        // （"穿透不了窗口"的直接证据）。只 warn 一次避免刷屏。
        if !any_focus && !self.no_focus_warned.get() {
            self.no_focus_warned.set(true);
            eprintln!(
                "[kb] WARN: kb_active=true 但无任何 keyboard focus（key={}）——按键被丢弃！Java 侧应 focusSurface 一个窗口",
                wire_key
            );
        }
    }

    pub fn pointer_unlock(&self) {
        self.for_all_pointers(|_pointer, data| {
            if let Some(lock) = &mut data.lock {
                if lock.active {
                    lock.locked_pointer.unlocked();
                }
                lock.active = false;
            }
        });
    }

    pub fn pointer_lock(&self, surface: &WlSurface) -> bool {
        for pointer in &self.pointers {
            let mut locked = false;
            with_pointer_data(pointer, |data| {
                if let Some(lock) = &mut data.lock {
                    if lock.surface == *surface {
                        if !lock.active {
                            lock.locked_pointer.locked();
                            lock.active = true;
                        }
                        locked = true;
                    } else if lock.active {
                        lock.locked_pointer.unlocked();
                        lock.active = false;
                    }
                }
            });

            if locked {
                return true;
            }
        }
        false
    }

    fn for_all_pointers<F>(&self, mut f: F)
    where
        F: FnMut(&WlPointer, &mut WLCPointerData),
    {
        for pointer in &self.pointers {
            with_pointer_data(pointer, |data| f(pointer, data));
        }
    }

    fn for_all_keyboards<F>(&self, mut f: F)
    where
        F: FnMut(&WlKeyboard, &mut WLCKeyboardData),
    {
        for keyboard in &self.keyboards {
            with_keyboard_data(keyboard, |data| f(keyboard, data));
        }
    }

    fn change_keymap(&mut self, keymap: Keymap) {
        let xkb_state = xkb::State::new(&keymap);
        let keymap_file = create_keymap_file(&keymap);

        self.xkb_state = xkb_state;
        self.keymap = keymap;
        self.keymap_file = keymap_file;
        self.keyboard_refocus();
    }

    pub fn change_keymap_to_default(&mut self) {
        let keymap = Keymap::new_from_names(
            &self.xkb_context,
            "",                           // rules
            "",                           // model
            "",                           // layout
            "",                           // variant
            None,                         // options
            xkb::KEYMAP_COMPILE_NO_FLAGS, // flags
        )
        .expect("default keymap create");
        self.change_keymap(keymap);
    }

    pub fn change_keymap_to_desc(&mut self, desc: &RMLVO) -> bool {
        let keymap = Keymap::new_from_names(
            &self.xkb_context,
            &desc.rules,
            &desc.model,
            &desc.layout,
            &desc.variant,
            Some(desc.options.clone()),
            xkb::KEYMAP_COMPILE_NO_FLAGS, // flags
        );
        let keymap = match keymap {
            Some(k) => k,
            None => return false,
        };
        self.change_keymap(keymap);
        true
    }

    pub fn change_keymap_from_str(&mut self, desc: String) -> bool {
        let keymap = Keymap::new_from_string(
            &self.xkb_context,
            desc,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        );
        let keymap = match keymap {
            Some(k) => k,
            None => return false,
        };
        self.change_keymap(keymap);
        true
    }

    pub fn export_keymap(&self) -> String {
        self.keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
    }
}

impl GlobalDispatch<WlSeat, ()> for WLCState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlSeat>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let seat: WlSeat = data_init.init(resource, ());
        if seat.version() >= wl_seat::EVT_NAME_SINCE {
            seat.name("waylandcraft-seat".into());
        }

        let mut caps: wl_seat::Capability = wl_seat::Capability::empty();
        caps.insert(wl_seat::Capability::Pointer);
        caps.insert(wl_seat::Capability::Keyboard);
        seat.capabilities(caps);
    }
}

impl Dispatch<WlSeat, ()> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        seat_resource: &WlSeat,
        request: wl_seat::Request,
        _data: &(),
        _disp: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_seat::Request::GetPointer { id } => {
                let pointer_data = WLCPointerData {
                    focus: None,
                    last_enter: None,
                    last_motion: None,
                    relative_pointers: vec![],
                    lock: None,
                    confined: None,
                };
                let pointer_data = Arc::new(Mutex::new(pointer_data));

                let pointer: WlPointer =
                    data_init.init(id, pointer_data.clone());

                state.seat.pointers.push(pointer);
            }
            wl_seat::Request::GetKeyboard { id } => {
                let keyboard_data = WLCKeyboardData { focus: None };
                let keyboard_data = Arc::new(Mutex::new(keyboard_data));

                let keyboard: WlKeyboard =
                    data_init.init(id, keyboard_data.clone());

                state.seat.keyboards.push(keyboard.clone());

                let keymap = &state.seat.keymap_file;
                keyboard.keymap(
                    KeymapFormat::XkbV1,
                    keymap.as_fd(),
                    keymap.size() as u32,
                );

                if keyboard.version() >= wl_keyboard::EVT_REPEAT_INFO_SINCE {
                    keyboard.repeat_info(25, 600);
                }
            }
            _ => {
                seat_resource.post_error(
                    wl_seat::Error::MissingCapability,
                    "accessed missing seat capability",
                );
            }
        }
    }
}

impl Dispatch<WlPointer, WLCPointer> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        pointer: &WlPointer,
        request: wl_pointer::Request,
        _data: &WLCPointer,
        _disp: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_pointer::Request::SetCursor {
                serial, surface, ..
            } => {
                let last_enter =
                    with_pointer_data(pointer, |data| data.last_enter);
                if last_enter.is_none() {
                    return;
                }
                if last_enter.unwrap() != serial {
                    return;
                }

                if surface.is_none() {
                    // Attaching an empty surface to hide cursor
                    // Zero value (not defined in protocol) means hidden here.
                    state.seat.cursor_shape = Some(0);
                } else {
                    // When an image is attached instead of a shape, reset to
                    // default because this compositor doesn't implement normal
                    // surface-based cursors, only cursor-shape.
                    state.seat.cursor_shape = None;
                }
            }
            wl_pointer::Request::Release => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        pointer_resource: &WlPointer,
        _data: &WLCPointer,
    ) {
        state.seat.pointers.retain(|p| p != pointer_resource);
    }
}

impl Dispatch<WlKeyboard, WLCKeyboard> for WLCState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _keyboard_resource: &WlKeyboard,
        request: wl_keyboard::Request,
        _data: &WLCKeyboard,
        _disp: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_keyboard::Request::Release => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        keyboard_resource: &WlKeyboard,
        _data: &WLCKeyboard,
    ) {
        state.seat.keyboards.retain(|kb| kb != keyboard_resource);
    }
}

impl GlobalDispatch<ZwpRelativePointerManagerV1, ()> for WLCState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpRelativePointerManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpRelativePointerManagerV1, ()> for WLCState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager_resource: &ZwpRelativePointerManagerV1,
        request: zwp_rpm::Request,
        _data: &(),
        _disp: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_rpm::Request::Destroy => {}
            zwp_rpm::Request::GetRelativePointer { id, pointer } => {
                let relative_pointer = data_init.init(id, ());

                with_pointer_data(&pointer, |data| {
                    data.relative_pointers.push(relative_pointer);
                });
            }
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for WLCState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _relpointer_resource: &ZwpRelativePointerV1,
        request: zwp_relpointer::Request,
        _data: &(),
        _disp: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_relpointer::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        relpointer_resource: &ZwpRelativePointerV1,
        _data: &(),
    ) {
        state.seat.for_all_pointers(|_pointer, data| {
            data.relative_pointers.retain(|r| r != relpointer_resource);
        });
    }
}

impl GlobalDispatch<ZwpPointerConstraintsV1, ()> for WLCState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpPointerConstraintsV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

fn has_existing_constraint(
    state: &mut WLCState,
    pointer: &WlPointer,
    surface: &WlSurface,
) -> bool {
    let mut err = false;
    with_pointer_data(pointer, |data| {
        if data.lock.is_some() || data.confined.is_some() {
            err = true;
        }
    });
    state.seat.for_all_pointers(|_pointer, data| {
        if let Some(lock) = &data.lock
            && lock.surface == *surface
        {
            err = true;
        }
        if let Some(lsurf) = &data.confined
            && lsurf == surface
        {
            err = true;
        }
    });
    err
}

impl Dispatch<ZwpPointerConstraintsV1, ()> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpPointerConstraintsV1,
        request: zwp_constraints::Request,
        _data: &(),
        _disp: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_constraints::Request::Destroy => {}
            zwp_constraints::Request::LockPointer {
                id,
                surface,
                pointer,
                ..
            } => {
                if has_existing_constraint(state, &pointer, &surface) {
                    resource.post_error(
                        zwp_constraints::Error::AlreadyConstrained,
                        "Pointer or surface already has attached constraint",
                    );
                    return;
                }

                let lock_resource = data_init.init(id, pointer.clone());

                with_pointer_data(&pointer, |data| {
                    data.lock = Some(WLCPointerLock {
                        locked_pointer: lock_resource,
                        surface: surface.clone(),
                        active: false,
                    });
                });
            }
            zwp_constraints::Request::ConfinePointer {
                id,
                surface,
                pointer,
                ..
            } => {
                if has_existing_constraint(state, &pointer, &surface) {
                    resource.post_error(
                        zwp_constraints::Error::AlreadyConstrained,
                        "Pointer or surface already has attached constraint",
                    );
                    return;
                }

                with_pointer_data(&pointer, |data| {
                    data.confined = Some(surface.clone());
                });

                let _confine_resource = data_init.init(id, pointer.clone());
            }
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, WlPointer> for WLCState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwpLockedPointerV1,
        request: zwp_locked::Request,
        _data: &WlPointer,
        _disp: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_locked::Request::Destroy => {}
            zwp_locked::Request::SetCursorPositionHint { .. } => {}
            zwp_locked::Request::SetRegion { .. } => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _locked_resource: &ZwpLockedPointerV1,
        pointer: &WlPointer,
    ) {
        with_pointer_data(pointer, |data| {
            data.lock = None;
        });
    }
}

impl Dispatch<ZwpConfinedPointerV1, WlPointer> for WLCState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwpConfinedPointerV1,
        request: zwp_confined::Request,
        _data: &WlPointer,
        _disp: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_confined::Request::Destroy => {}
            zwp_confined::Request::SetRegion { .. } => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _confined_resource: &ZwpConfinedPointerV1,
        pointer: &WlPointer,
    ) {
        with_pointer_data(pointer, |data| {
            data.confined = None;
        });
    }
}

impl GlobalDispatch<WpCursorShapeManagerV1, ()> for WLCState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WpCursorShapeManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WpCursorShapeManagerV1, ()> for WLCState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WpCursorShapeManagerV1,
        request: wp_cursor_shape_manager_v1::Request,
        _data: &(),
        _disp: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_cursor_shape_manager_v1::Request::Destroy => {}
            wp_cursor_shape_manager_v1::Request::GetPointer {
                cursor_shape_device,
                pointer,
            } => {
                let device_data = WLCCursorShapeDeviceData {
                    pointer: Some(pointer),
                };
                let device_data = Arc::new(Mutex::new(device_data));
                data_init.init(cursor_shape_device, device_data);
            }
            wp_cursor_shape_manager_v1::Request::GetTabletToolV2 {
                cursor_shape_device,
                ..
            } => {
                let device_data = WLCCursorShapeDeviceData { pointer: None };
                let device_data = Arc::new(Mutex::new(device_data));
                data_init.init(cursor_shape_device, device_data);
            }
            _ => unreachable!(),
        }
    }
}

impl Dispatch<WpCursorShapeDeviceV1, WLCCursorShapeDevice> for WLCState {
    fn request(
        state: &mut Self,
        _client: &Client,
        device: &WpCursorShapeDeviceV1,
        request: wp_cursor_shape_device_v1::Request,
        _data: &WLCCursorShapeDevice,
        _disp: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_cursor_shape_device_v1::Request::Destroy => {}
            wp_cursor_shape_device_v1::Request::SetShape { shape, serial } => {
                let pointer = with_cursor_shape_device_data(device, |data| {
                    data.pointer.clone()
                });

                if pointer.is_none() {
                    // No tablet support
                    return;
                }
                let pointer = pointer.unwrap();

                let last_enter =
                    with_pointer_data(&pointer, |data| data.last_enter);
                if last_enter.is_none() {
                    return;
                }
                if last_enter.unwrap() != serial {
                    return;
                }

                state.seat.cursor_shape = Some(shape.into());
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod keyboard_focus_tests {
    use super::*;
    use smithay::reexports::wayland_server::{
        Display, DisplayHandle, GlobalDispatch, New, Resource,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{
            wl_compositor, wl_compositor::WlCompositor,
            wl_keyboard::WlKeyboard, wl_seat::WlSeat,
            wl_surface, wl_surface::WlSurface,
        },
    };
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    // ================= server 端：最小 compositor =================

    struct TestState {
        seat: WLCSeatState,
        surfaces: Vec<WlSurface>,
    }

    struct TestClientData;

    impl ClientData for TestClientData {
        fn initialized(&self, _id: ClientId) {}
        fn disconnected(&self, _id: ClientId, _reason: DisconnectReason) {}
    }

    impl GlobalDispatch<WlSeat, ()> for TestState {
        fn bind(
            _state: &mut Self,
            _handle: &DisplayHandle,
            _client: &Client,
            resource: New<WlSeat>,
            _data: &(),
            data_init: &mut DataInit<'_, Self>,
        ) {
            let seat: WlSeat = data_init.init(resource, ());
            let mut caps: wl_seat::Capability = wl_seat::Capability::empty();
            caps.insert(wl_seat::Capability::Pointer);
            caps.insert(wl_seat::Capability::Keyboard);
            seat.capabilities(caps);
        }
    }

    impl Dispatch<WlSeat, ()> for TestState {
        fn request(
            state: &mut Self,
            _client: &Client,
            _seat_resource: &WlSeat,
            request: wl_seat::Request,
            _data: &(),
            _disp: &DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            match request {
                wl_seat::Request::GetKeyboard { id } => {
                    let keyboard_data = WLCKeyboardData { focus: None };
                    let keyboard_data = Arc::new(Mutex::new(keyboard_data));

                    let keyboard: WlKeyboard =
                        data_init.init(id, keyboard_data.clone());

                    state.seat.keyboards.push(keyboard.clone());

                    let keymap = &state.seat.keymap_file;
                    keyboard.keymap(
                        KeymapFormat::XkbV1,
                        keymap.as_fd(),
                        keymap.size() as u32,
                    );
                }
                _ => {}
            }
        }
    }

    impl Dispatch<WlKeyboard, WLCKeyboard> for TestState {
        fn request(
            _state: &mut Self,
            _client: &Client,
            _keyboard_resource: &WlKeyboard,
            request: wl_keyboard::Request,
            _data: &WLCKeyboard,
            _disp: &DisplayHandle,
            _data_init: &mut DataInit<'_, Self>,
        ) {
            match request {
                wl_keyboard::Request::Release => {}
                _ => {}
            }
        }
    }

    impl GlobalDispatch<WlCompositor, ()> for TestState {
        fn bind(
            _state: &mut Self,
            _handle: &DisplayHandle,
            _client: &Client,
            resource: New<WlCompositor>,
            _data: &(),
            data_init: &mut DataInit<'_, Self>,
        ) {
            data_init.init(resource, ());
        }
    }

    impl Dispatch<WlCompositor, ()> for TestState {
        fn request(
            state: &mut Self,
            _client: &Client,
            _compositor_resource: &WlCompositor,
            request: wl_compositor::Request,
            _data: &(),
            _disp: &DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            match request {
                wl_compositor::Request::CreateSurface { id } => {
                    let surface: WlSurface = data_init.init(id, ());
                    state.surfaces.push(surface);
                }
                _ => {}
            }
        }
    }

    impl Dispatch<WlSurface, ()> for TestState {
        fn request(
            _state: &mut Self,
            _client: &Client,
            _surface_resource: &WlSurface,
            request: wl_surface::Request,
            _data: &(),
            _disp: &DisplayHandle,
            _data_init: &mut DataInit<'_, Self>,
        ) {
            match request {
                _ => {}
            }
        }
    }

    // ================= client 端：真实 wayland-client，收集事件 =================
    // 注意：wayland-client 与 wayland-server 是同名协议的**不同 crate**，
    // 类型不能混用。这里把 client 完全隔离进子模块。

    mod client_side {
        use std::os::unix::net::UnixStream;
        use wayland_client::{
            delegate_noop, Connection, Dispatch, EventQueue, Proxy,
            QueueHandle,
            protocol::{
                wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_surface,
            },
        };

        pub use wayland_client::protocol::wl_keyboard as c_wl_keyboard;

        pub struct ClientState {
            pub key_events: Vec<(u32, wl_keyboard::KeyState)>,
            pub enter_count: usize,
            pub leave_count: usize,
            // 持有 proxy 防止被 GC
            _keyboard: Option<wl_keyboard::WlKeyboard>,
            _surface: Option<wl_surface::WlSurface>,
        }        impl Default for ClientState {
            fn default() -> Self {
                Self {
                    key_events: vec![],
                    enter_count: 0,
                    leave_count: 0,
                    _keyboard: None,
                    _surface: None,
                }
            }
        }

        impl Dispatch<wl_registry::WlRegistry, ()> for ClientState {
            fn event(
                state: &mut Self,
                registry: &wl_registry::WlRegistry,
                event: wl_registry::Event,
                _data: &(),
                _conn: &Connection,
                qh: &QueueHandle<Self>,
            ) {
                if let wl_registry::Event::Global {
                    name, interface, ..
                } = event
                {
                    match interface.as_str() {
                        "wl_seat" => {
                            let seat = registry.bind::<wl_seat::WlSeat, _, _>(
                                name, 1, qh, (),
                            );
                            state._keyboard = Some(seat.get_keyboard(qh, ()));
                        }
                        "wl_compositor" => {
                            let compositor = registry.bind::<
                                wl_compositor::WlCompositor,
                                _,
                                _,
                            >(name, 1, qh, ());
                            state._surface =
                                Some(compositor.create_surface(qh, ()));
                        }
                        _ => {}
                    }
                }
            }
        }

        impl Dispatch<wl_keyboard::WlKeyboard, ()> for ClientState {
            fn event(
                state: &mut Self,
                _keyboard: &wl_keyboard::WlKeyboard,
                event: wl_keyboard::Event,
                _data: &(),
                _conn: &Connection,
                _qh: &QueueHandle<Self>,
            ) {
                match event {
                    wl_keyboard::Event::Enter { .. } => {
                        state.enter_count += 1
                    }
                    wl_keyboard::Event::Leave { .. } => {
                        state.leave_count += 1
                    }
                    wl_keyboard::Event::Key {
                        key, state: ks, ..
                    } => {
                        // 真实事件里 state 是 WEnum 包装；测试中只会收到 Value 变体
                        if let wayland_client::WEnum::Value(v) = ks {
                            state.key_events.push((key, v));
                        }
                    }
                    _ => {}
                }
            }
        }

        delegate_noop!(ClientState: ignore wl_seat::WlSeat);
        delegate_noop!(ClientState: ignore wl_surface::WlSurface);
        delegate_noop!(ClientState: ignore wl_compositor::WlCompositor);

        pub struct TestClient {
            queue: EventQueue<ClientState>,
            pub state: ClientState,
        }

        impl TestClient {
            pub fn connect(stream: UnixStream) -> Self {
                let conn = Connection::from_socket(stream).unwrap();
                let mut queue = conn.new_event_queue();
                let qh = queue.handle();
                conn.display().get_registry(&qh, ());
                let state = ClientState::default();
                Self { queue, state }
            }

            /// 发送 client 端 pending 请求（bind/get_keyboard/create_surface）
            pub fn flush(&self) {
                self.queue.flush().unwrap();
            }

            /// 非阻塞处理已到达的 server 事件（global 列表 / keymap / key）
            pub fn dispatch_pending(&mut self) {
                if let Some(guard) = self.queue.prepare_read() {
                    // 非阻塞 socket：没有新数据时 read 返回 WouldBlock，忽略即可
                    let _ = guard.read();
                }
                self.queue.dispatch_pending(&mut self.state).unwrap();
            }
        }
    }

    use client_side::TestClient;

    /// 搭起 server + client。返回 (display, server_state, client)
    fn setup() -> (Display<TestState>, TestState, TestClient) {
        let mut display: Display<TestState> = Display::new().unwrap();
        let mut handle = display.handle();
        handle.create_global::<TestState, WlSeat, ()>(8, ());
        handle.create_global::<TestState, WlCompositor, ()>(5, ());

        let (server_stream, client_stream) = UnixStream::pair().unwrap();
        handle
            .insert_client(server_stream, Arc::new(TestClientData))
            .unwrap();

        let mut server_state = TestState {
            seat: WLCSeatState::new(),
            surfaces: vec![],
        };

        // client 连接（get_registry 已发出）
        let mut client = TestClient::connect(client_stream);

        // 握手：多轮交替驱动，直到 client 拿到 global 列表并 bind 完成
        for _ in 0..5 {
            client.flush();
            display.dispatch_clients(&mut server_state).unwrap();
            display.flush_clients().unwrap();
            client.dispatch_pending();
        }

        (display, server_state, client)
    }

    fn drive(
        display: &mut Display<TestState>,
        server_state: &mut TestState,
        client: &mut TestClient,
    ) {
        client.flush();
        display.dispatch_clients(server_state).unwrap();
        display.flush_clients().unwrap();
        client.dispatch_pending();
    }

    #[test]
    fn test_keyboard_key_requires_focus() {
        let (mut display, mut server_state, mut client) = setup();

        assert_eq!(server_state.seat.keyboards.len(), 1);
        let surface = server_state.surfaces[0].clone();

        // ===== 场景 1：未激活键盘（kb_active=false）→ 按键不转发 =====
        server_state.seat.keyboard_key(30, KeyboardAction::Press);
        drive(&mut display, &mut server_state, &mut client);
        assert_eq!(
            client.state.key_events.len(),
            0,
            "kb_active=false 时不应产生任何 key 事件"
        );

        // ===== 场景 2：激活但无 focus → 按键不转发（复现 bug！）=====
        server_state.seat.activate_keyboard();
        server_state.seat.keyboard_key(30, KeyboardAction::Press);
        drive(&mut display, &mut server_state, &mut client);
        assert_eq!(
            client.state.key_events.len(),
            0,
            "激活但无 focus 时按键必须被丢弃（这正是 v0.9.1 修复的根因）"
        );

        // ===== 场景 3：focus(surface) 后 → 按键转发 =====
        server_state.seat.keyboard_focus(surface.clone());
        drive(&mut display, &mut server_state, &mut client);
        assert_eq!(
            client.state.enter_count, 1,
            "focus 后应收到 enter"
        );

        server_state.seat.keyboard_key(30, KeyboardAction::Press);
        drive(&mut display, &mut server_state, &mut client);
        assert_eq!(
            client.state.key_events.len(),
            1,
            "focus 后按键应转发到 client"
        );
        let (key, ks) = client.state.key_events[0];
        assert_eq!(key, 22, "evdev 30 - 8 = 22 应为 wire keycode");
        assert_eq!(
            ks,
            client_side::c_wl_keyboard::KeyState::Pressed,
            "Press 动作应产生 Pressed 状态"
        );

        // ===== 场景 4：Release 语义 =====
        server_state.seat.keyboard_key(30, KeyboardAction::Release);
        drive(&mut display, &mut server_state, &mut client);
        assert_eq!(client.state.key_events.len(), 2);
        assert_eq!(
            client.state.key_events[1].1,
            client_side::c_wl_keyboard::KeyState::Released,
            "Release 动作应产生 Released 状态"
        );

        // ===== 场景 5：Repeat 语义（长按） =====
        server_state.seat.keyboard_key(30, KeyboardAction::Repeat);
        drive(&mut display, &mut server_state, &mut client);
        assert_eq!(client.state.key_events.len(), 3);
        assert_eq!(
            client.state.key_events[2].1,
            client_side::c_wl_keyboard::KeyState::Pressed,
            "Repeat 动作应补发 Pressed（长按保持按下）"
        );

        // ===== 场景 6：unfocus 后按键再次被丢弃 =====
        server_state.seat.keyboard_unfocus();
        drive(&mut display, &mut server_state, &mut client);
        assert_eq!(client.state.leave_count, 1, "unfocus 后应收到 leave");

        let before = client.state.key_events.len();
        server_state.seat.keyboard_key(30, KeyboardAction::Press);
        drive(&mut display, &mut server_state, &mut client);
        assert_eq!(
            client.state.key_events.len(),
            before,
            "unfocus 后按键必须再次被丢弃"
        );
    }

    #[test]
    fn test_keyboard_action_mapping() {
        assert_eq!(
            KeyboardAction::from_i32(0),
            Some(KeyboardAction::Release)
        );
        assert_eq!(KeyboardAction::from_i32(1), Some(KeyboardAction::Press));
        assert_eq!(
            KeyboardAction::from_i32(2),
            Some(KeyboardAction::Repeat)
        );
        assert_eq!(KeyboardAction::from_i32(99), None);
        assert_eq!(KeyboardAction::from_i32(-1), None);

        assert_eq!(KeyboardAction::Release.key_state(), KeyState::Released);
        assert_eq!(KeyboardAction::Press.key_state(), KeyState::Pressed);
        assert_eq!(KeyboardAction::Repeat.key_state(), KeyState::Pressed);
    }
}
