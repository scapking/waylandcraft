/// 桌面窗口信息
#[derive(Debug, Clone)]
pub struct DesktopWindow {
    pub window_id: String,
    pub title: String,
    pub app_id: String,
    pub pid: u32,
    pub hash: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 获取桌面窗口列表（带退避重试，多方案回退）
pub fn get_desktop_windows() -> Vec<DesktopWindow> {
    let mut methods: Vec<(&str, fn() -> Option<Vec<DesktopWindow>>)> = vec![
        ("waylandcraft-extension", try_waylandcraft_extension),
        ("wmctrl-xauth", try_wmctrl_with_xauth),
    ];
    #[cfg(target_os = "linux")]
    methods.push(("wlr-foreign-toplevel", wlr_toplevel::try_wlr_foreign_toplevel));

    for (name, method) in &methods {
        eprintln!("[desktop_windows] === Trying {} ===", name);
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| method())) {
            Ok(Some(windows)) if !windows.is_empty() => {
                eprintln!("[desktop_windows] SUCCESS: {} returned {} windows", name, windows.len());
                for w in &windows {
                    eprintln!("[desktop_windows]   -> [{}] {} (app: {})", w.window_id, w.title, w.app_id);
                }
                return windows;
            }
            Ok(Some(_)) => {
                eprintln!("[desktop_windows] {} returned empty list", name);
            }
            Ok(None) => {
                eprintln!("[desktop_windows] {} returned None (failed)", name);
            }
            Err(e) => {
                eprintln!("[desktop_windows] {} PANICKED: {:?}", name, e);
            }
        }
    }

    // 最终回退: /proc
    eprintln!("[desktop_windows] ALL methods failed, falling back to /proc");
    get_process_list_fallback()
}

// ═══════════════════════════════════════════════════════
// 方案1: GNOME Shell Introspect D-Bus（最完整，有 title/app-id/width/height）
// ═══════════════════════════════════════════════════════

fn try_waylandcraft_extension() -> Option<Vec<DesktopWindow>> {
    use zbus::blocking::Connection;

    eprintln!("[wc-ext] Connecting to D-Bus session...");
    let conn = retry_with_backoff(|| {
        Connection::session().map_err(|e| e.to_string())
    }, 2, 200)?;

    eprintln!("[wc-ext] Calling org.waylandcraft.Windows.GetWindows...");
    let reply = match conn.call_method(
        Some("org.waylandcraft.Windows"),
        "/org/waylandcraft/Windows",
        Some("org.waylandcraft.Windows"),
        "GetWindows",
        &(),
    ) {
        Ok(r) => {
            eprintln!("[wc-ext] D-Bus call succeeded!");
            r
        }
        Err(e) => {
            eprintln!("[wc-ext] D-Bus call failed: {}", e);
            return None;
        }
    };

    let body = reply.body();
    let windows = parse_waylandcraft_windows(&body);
    eprintln!("[wc-ext] Parsed {} windows", windows.len());

    if windows.is_empty() {
        None
    } else {
        Some(windows)
    }
}

fn parse_waylandcraft_windows(body: &zbus::message::Body) -> Vec<DesktopWindow> {
    // GetWindows returns a(tsssu): array of (u64 id, string title, string wm_class, string app_id, u32 pid)
    let entries: Vec<(u64, String, String, String, u32)> = match body.deserialize() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[wc-ext] Failed to deserialize body: {}", e);
            return Vec::new();
        }
    };

    eprintln!("[wc-ext] Deserialized {} windows from GNOME extension", entries.len());

    let mut windows = Vec::new();

    for (id, title, wm_class, app_id, pid) in entries {
        let hash = pid_hash(pid);
        let display_title = if !title.is_empty() { title } else { app_id.clone() };

        eprintln!("[wc-ext]   id=0x{:08x} title={:?} app_id={:?} pid={}",
            id, display_title, app_id, pid);

        if !display_title.is_empty() {
            windows.push(DesktopWindow {
                window_id: format!("0x{:08x}", id),
                title: display_title,
                app_id: if !app_id.is_empty() { app_id } else { wm_class },
                pid,
                hash,
                x: 0, y: 0, width: 0, height: 0,
            });
        }
    }

    windows
}

/// 从 PID 生成 4-6 位 hex hash
fn pid_hash(pid: u32) -> String {
    // 简单 CRC32-like hash: PID * 2654435761 (Knuth), 取低 16-bit → 4-5 hex chars
    let h = pid.wrapping_mul(2654435761);
    format!("{:04x}", h & 0xFFFF)
}

// ═══════════════════════════════════════════════════════
// 方案2: wmctrl + X authority（XWayland 窗口）
// ═══════════════════════════════════════════════════════

fn try_wmctrl_with_xauth() -> Option<Vec<DesktopWindow>> {
    eprintln!("[wmctrl] Looking for X authority file...");

    // 查找 X authority 文件
    let xauth_paths = find_xauth_files();
    eprintln!("[wmctrl] Found {} X auth candidates: {:?}", xauth_paths.len(), xauth_paths);

    for xauth_path in &xauth_paths {
        eprintln!("[wmctrl] Trying XAUTHORITY={}...", xauth_path);

        let output = std::process::Command::new("wmctrl")
            .arg("-l")
            .arg("-p")
            .env("DISPLAY", ":0")
            .env("XAUTHORITY", xauth_path)
            .output()
            .ok()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("[wmctrl] Failed with XAUTHORITY={}: {}", xauth_path, stderr);
            continue;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        eprintln!("[wmctrl] Raw output ({} lines):", stdout.lines().count());
        for line in stdout.lines() {
            eprintln!("[wmctrl]   {}", line);
        }

        let windows: Vec<DesktopWindow> = stdout.lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let window_id = parts[0].to_string();
                    let pid = parts[2];
                    // wmctrl 格式: 0x... desktop PID hostname title...
                    // 找到 hostname 后面的部分作为 title
                    let title_start = line.find(parts[3]).unwrap_or(0) + parts[3].len() + 1;
                    let title = if title_start < line.len() {
                        line[title_start..].trim().to_string()
                    } else {
                        parts[0].to_string()
                    };
                    if !title.is_empty() {
                        Some(DesktopWindow {
                            window_id,
                            title,
                            app_id: format!("pid:{}", pid),
                            pid: pid.parse().unwrap_or(0),
                            hash: pid_hash(pid.parse().unwrap_or(0)),
                            x: 0, y: 0, width: 0, height: 0,
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        if !windows.is_empty() {
            eprintln!("[wmctrl] Parsed {} windows", windows.len());
            return Some(windows);
        }
    }

    eprintln!("[wmctrl] No windows found with any X auth file");
    None
}

/// 查找系统中的 X authority 文件
fn find_xauth_files() -> Vec<String> {
    let mut paths = Vec::new();

    // 直接读取 /run/user/1000/ 查找 Xwaylandauth
    if let Ok(entries) = std::fs::read_dir("/run/user/1000") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains("Xwaylandauth") || name.contains("mutter-X") {
                if let Some(p) = entry.path().to_str() {
                    paths.push(p.to_string());
                }
            }
        }
    }

    // 其他常见路径
    let extra = [
        "/home/server/.Xauthority",
        "/root/.Xauthority",
    ];
    for p in &extra {
        if std::path::Path::new(p).exists() {
            paths.push(p.to_string());
        }
    }

    paths.sort();
    paths.dedup();
    paths
}

// ═══════════════════════════════════════════════════════
// 方案3: wlr-foreign-toplevel（wlroots 系合成器）
// 仅 Linux 桌面：经 dlopen 加载 libwayland-client，Android 无此库会在
// 首次调用时 panic（release 为 abort），故整个方案在非 Linux 裁剪。
// ═══════════════════════════════════════════════════════
#[cfg(target_os = "linux")]
mod wlr_toplevel {
    use super::*;

    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        protocol::wl_registry,
    };
    use wayland_protocols_wlr::foreign_toplevel::v1::client::{
        zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
        zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    };

struct ToplevelEnumerator {
    manager: Option<ZwlrForeignToplevelManagerV1>,
    toplevels: Vec<ToplevelEntry>,
}

struct ToplevelEntry {
    handle: Option<ZwlrForeignToplevelHandleV1>,
    title: String,
    app_id: String,
    done: bool,
}

impl ToplevelEnumerator {
    fn new() -> Self {
        Self { manager: None, toplevels: Vec::new() }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for ToplevelEnumerator {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            if interface == "zwlr_foreign_toplevel_manager_v1" {
                let manager = registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(
                    name, version.min(3), qh, (),
                );
                state.manager = Some(manager);
            }
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for ToplevelEnumerator {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(), _: &Connection, _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(ToplevelEntry {
                handle: Some(toplevel.clone()),
                title: String::new(),
                app_id: String::new(),
                done: false,
            });
        }
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for ToplevelEnumerator {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(), _: &Connection, _: &QueueHandle<Self>,
    ) {
        if let Some(entry) = state.toplevels.iter_mut().find(|e| e.handle.as_ref() == Some(handle)) {
            match event {
                zwlr_foreign_toplevel_handle_v1::Event::Title { title } => entry.title = title,
                zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => entry.app_id = app_id,
                zwlr_foreign_toplevel_handle_v1::Event::Done => entry.done = true,
                zwlr_foreign_toplevel_handle_v1::Event::Closed => entry.done = true,
                _ => {}
            }
        }
    }
}

    pub fn try_wlr_foreign_toplevel() -> Option<Vec<DesktopWindow>> {
    eprintln!("[wlr] Connecting to Wayland compositor...");
    let conn = retry_with_backoff(|| {
        Connection::connect_to_env().map_err(|e| e.to_string())
    }, 2, 200)?;

    let mut event_queue = conn.new_event_queue::<ToplevelEnumerator>();
    let qh = event_queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut state = ToplevelEnumerator::new();

    eprintln!("[wlr] First roundtrip (discovering globals)...");
    event_queue.roundtrip(&mut state).ok()?;

    if state.manager.is_none() {
        eprintln!("[wlr] Manager NOT found (not a wlroots compositor)");
        return None;
    }
    eprintln!("[wlr] Found wlr-foreign-toplevel-manager!");

    eprintln!("[wlr] Second roundtrip (collecting toplevels)...");
    event_queue.roundtrip(&mut state).ok()?;
    event_queue.roundtrip(&mut state).ok()?;

    eprintln!("[wlr] Found {} toplevels", state.toplevels.len());

    let windows: Vec<DesktopWindow> = state.toplevels.iter()
        .filter(|e| !e.title.is_empty() || !e.app_id.is_empty())
        .enumerate()
        .map(|(i, e)| DesktopWindow {
            window_id: format!("0x{:04x}", i),
            title: if !e.title.is_empty() { e.title.clone() } else { e.app_id.clone() },
            app_id: e.app_id.clone(),
            pid: 0,
            hash: format!("{:04x}", i),
            x: 0, y: 0, width: 0, height: 0,
        })
        .collect();

    if let Some(ref manager) = state.manager { manager.stop(); }
    let _ = event_queue.roundtrip(&mut state);

    Some(windows)
}
} // end mod wlr_toplevel

// ═══════════════════════════════════════════════════════
// 回退: /proc 进程列表
// ═══════════════════════════════════════════════════════

fn get_process_list_fallback() -> Vec<DesktopWindow> {
    get_process_list()
        .into_iter()
        .filter_map(|(pid, cmdline)| {
            let name = cmdline.split_whitespace().next()?.to_string();
            let basename = name.rsplit('/').next()?.to_string();
            if !basename.is_empty() {
                Some(DesktopWindow {
                    window_id: format!("{}", pid),
                    title: basename.clone(),
                    app_id: basename,
                    pid: pid as u32,
                    hash: pid_hash(pid as u32),
                    x: 0, y: 0, width: 0, height: 0,
                })
            } else { None }
        })
        .collect()
}

pub fn get_process_list() -> Vec<(i32, String)> {
    let mut processes = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Ok(file_name) = entry.file_name().into_string() {
                if let Ok(pid) = file_name.parse::<i32>() {
                    let cmdline_path = format!("/proc/{}/cmdline", pid);
                    if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
                        let cmdline = cmdline.replace('\0', " ").trim().to_string();
                        if !cmdline.is_empty() {
                            processes.push((pid, cmdline));
                        }
                    }
                }
            }
        }
    }
    processes
}

// ═══════════════════════════════════════════════════════
// 通用工具: 退避重试
// ═══════════════════════════════════════════════════════

fn retry_with_backoff<F, T, E>(mut f: F, max_retries: u32, base_delay_ms: u64) -> Option<T>
where
    F: FnMut() -> Result<T, E>,
    E: std::fmt::Display,
{
    for attempt in 0..max_retries {
        match f() {
            Ok(val) => return Some(val),
            Err(e) => {
                let delay = base_delay_ms * 2u64.pow(attempt);
                eprintln!("[retry] Attempt {}/{} failed: {}, waiting {}ms",
                    attempt + 1, max_retries, e, delay);
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
        }
    }
    None
}

// ═══════════════════════════════════════════════════════
// GNOME Shell Screenshot D-Bus 调用（纯代码，无外部依赖）
// ═══════════════════════════════════════════════════════

/// 通过 GNOME Shell Extension CaptureWindow 截取指定窗口
/// 扩展运行在 GNOME Shell 进程内，有权限调用 Shell.Screenshot API
/// 返回 Ok(png_bytes) 或 Err
/// 通过 GNOME Shell Extension CaptureWindow 截取指定窗口
/// 扩展运行在 GNOME Shell 进程内，有权限调用 Shell.Screenshot API
/// 返回 Ok(png_bytes) 或 Err
pub fn screenshot_area(window_id: u64) -> Result<Vec<u8>, String> {
    use zbus::blocking::Connection;

    let conn = Connection::session().map_err(|e| format!("D-Bus connect: {}", e))?;

    let reply = conn.call_method(
        Some("org.waylandcraft.Windows"),
        "/org/waylandcraft/Windows",
        Some("org.waylandcraft.Windows"),
        "CaptureWindow",
        &(window_id,),
    ).map_err(|e| format!("CaptureWindow call: {}", e))?;

    let body = reply.body();
    let result_json: String = body.deserialize()
        .map_err(|e| format!("deserialize: {}", e))?;

    eprintln!("[capture] CaptureWindow result: {}", &result_json[..result_json.len().min(200)]);

    // JSON: {"ok": true, "path": "/tmp/wc_cap_XXX.png"}
    if !result_json.contains("true") {
        return Err(format!("CaptureWindow failed: {}", result_json));
    }

    // Extract path between quotes after "path":
    let marker = "path";
    let pos = result_json.find(marker).ok_or("no path field")?;
    let after = &result_json[pos + marker.len()..];
    // Skip to first quote
    let q1 = after.find('"').ok_or("no open quote")?;
    let rest = &after[q1 + 1..];
    let q2 = rest.find('"').ok_or("no close quote")?;
    let png_path = &rest[..q2];

    let png_data = std::fs::read(png_path)
        .map_err(|e| format!("read {}: {}", png_path, e))?;

    let _ = std::fs::remove_file(png_path);
    Ok(png_data)
}



/// 获取指定窗口的坐标（通过 GNOME extension D-Bus）
/// 返回 (x, y, width, height) 或 None
pub fn get_window_geometry(window_title: &str) -> Option<(i32, i32, i32, i32)> {
    let windows = get_desktop_windows();
    for w in &windows {
        if w.title.contains(window_title) || w.hash == window_title {
            if w.width > 0 && w.height > 0 {
                return Some((w.x, w.y, w.width, w.height));
            }
        }
    }
    None
}
