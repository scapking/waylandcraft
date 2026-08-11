use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::io::Write;

fn log(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open("/tmp/waylandcraft-launch.log")
    {
        let _ = writeln!(f, "{}", msg);
    }
}

/// 打开一个 per-app 日志文件，用于捕获子进程的 stdout/stderr。
/// 之前用 Stdio::null() 把子进程输出全丢了，应用为什么连不上合成器
/// 完全看不到。现在 QQ/Chrome/flatpak 的 stderr 都会透传到这里。
fn app_log_file(cmd: &str) -> std::fs::File {
    let safe = cmd.rsplit('/').next().unwrap_or(cmd);
    let path = format!("/tmp/waylandcraft-app-{}.log", safe);
    std::fs::OpenOptions::new()
        .create(true).append(true).open(&path)
        .or_else(|_| std::fs::File::open("/dev/null"))
        .unwrap_or_else(|_| panic!("cannot open {} nor /dev/null", path))
}

/// flatpak 应用的 manifest 能力（解析 `flatpak info --show-metadata` 的 [Context]/[Environment]）
#[derive(Debug, Default)]
struct FlatpakCaps {
    /// manifest 是否成功读到（false = flatpak 不可用/未安装/解析失败 → 走保守路径）
    detected: bool,
    has_wayland: bool,
    has_x11: bool,
    has_pulse: bool,
    has_pipewire: bool,
    /// manifest 自带的 QT_QPA_PLATFORM（如微信 =xcb），尊重它，不覆盖
    manifest_qpa: Option<String>,
    /// manifest 自带的 GDK_BACKEND
    manifest_gdk: Option<String>,
}

/// 读取 flatpak 应用的 manifest，检测它声明支持哪些协议（wayland/x11/pulseaudio/pipewire）。
/// 这是通用方案：不再硬编码应用列表，任何 flatpak 应用都按它自己的 manifest 注入。
/// 检测失败时 detected=false（调用方走保守路径：只给 socket+DISPLAY，不强制后端变量）。
fn detect_flatpak_caps(app_id: &str) -> FlatpakCaps {
    let mut caps = FlatpakCaps::default();
    if app_id.is_empty() {
        return caps;
    }
    let out = std::process::Command::new("flatpak")
        .args(["info", "--show-metadata", app_id])
        .output();
    let text = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            log(&format!("[flatpak] detect manifest failed for {}", app_id));
            return caps;
        }
    };
    let mut in_context = false;
    let mut in_env = false;
    let mut saw_sockets = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_context = line == "[Context]";
            in_env = line == "[Environment]";
            continue;
        }
        if in_context {
            if let Some(v) = line.strip_prefix("sockets=") {
                saw_sockets = true;
                let sockets: Vec<&str> = v.split(';').collect();
                caps.has_wayland = sockets.contains(&"wayland");
                caps.has_x11 = sockets.contains(&"x11");
                caps.has_pulse = sockets.contains(&"pulseaudio");
                caps.has_pipewire = sockets.contains(&"pipewire");
            }
        }
        if in_env {
            if let Some(v) = line.strip_prefix("QT_QPA_PLATFORM=") {
                caps.manifest_qpa = Some(v.trim().to_string());
            }
            if let Some(v) = line.strip_prefix("GDK_BACKEND=") {
                caps.manifest_gdk = Some(v.trim().to_string());
            }
        }
    }
    // 只要读到了 [Context] sockets 行就算检测成功；老 manifest 没有 sockets 行时
    // 保持 detected=false 走保守路径（应用自己选后端，两个 socket 都给）。
    caps.detected = saw_sockets || caps.manifest_qpa.is_some() || caps.manifest_gdk.is_some();
    log(&format!(
        "[flatpak] {} caps: detected={} wayland={} x11={} pulse={} pipewire={} qpa={:?} gdk={:?}",
        app_id, caps.detected, caps.has_wayland, caps.has_x11, caps.has_pulse, caps.has_pipewire,
        caps.manifest_qpa, caps.manifest_gdk
    ));
    caps
}

/// 检测应用类型
fn detect_app_type(cmd: &str, args: &[String]) -> &'static str {
    let cmd_lower = cmd.to_lowercase();
    
    if cmd_lower.ends_with("/flatpak") || cmd_lower == "flatpak" {
        return "flatpak";
    }
    if cmd_lower.contains("wine") || cmd_lower.contains("proton") {
        return "wine";
    }
    if cmd_lower.contains("electron") || cmd_lower.contains("code") 
        || cmd_lower.contains("discord") || cmd_lower.contains("slack")
        || cmd_lower.contains("clash-verge") || cmd_lower.contains("clash-nyanpasu") {
        return "electron";
    }
    if cmd_lower.contains("gnome-") || cmd_lower.contains("nautilus") 
        || cmd_lower.contains("totem") || cmd_lower.contains("evince")
        || cmd_lower.contains("gedit") || cmd_lower.contains("eog")
        || cmd_lower.contains("thunderbird") || cmd_lower.contains("transmission") {
        return "gtk";
    }
    if cmd_lower.contains("dolphin") || cmd_lower.contains("kate") 
        || cmd_lower.contains("okular") || cmd_lower.contains("konsole")
        || cmd_lower.contains("vlc") || cmd_lower.contains("obs")
        || cmd_lower.contains("qbittorrent") || cmd_lower.contains("kdenlive") {
        return "qt";
    }
    if cmd_lower.contains("firefox") || cmd_lower.contains("chromium") {
        return "browser";
    }
    "native"
}

/// 根据类型构建环境变量列表
fn build_env_list(app_type: &str, wayland_display: &str, display: &str) -> Vec<(String, String)> {
    let mut env_list = vec![
        ("WAYLAND_DISPLAY".to_string(), wayland_display.to_string()),
        ("DISPLAY".to_string(), display.to_string()),
    ];
    
    match app_type {
        "flatpak" => {
            env_list.clear();
        }
        "wine" => {}
        "electron" => {
            env_list.push(("ELECTRON_OZONE_PLATFORM_HINT".to_string(), "auto".to_string()));
            env_list.push(("OZONE_PLATFORM".to_string(), "wayland".to_string()));
            env_list.push(("GDK_BACKEND".to_string(), "wayland".to_string()));
        }
        "gtk" => {
            env_list.push(("GDK_BACKEND".to_string(), "wayland".to_string()));
        }
        "qt" => {
            env_list.push(("QT_QPA_PLATFORM".to_string(), "wayland".to_string()));
        }
        _ => {
            env_list.push(("GDK_BACKEND".to_string(), "wayland".to_string()));
            env_list.push(("QT_QPA_PLATFORM".to_string(), "wayland".to_string()));
            env_list.push(("ELECTRON_OZONE_PLATFORM_HINT".to_string(), "auto".to_string()));
        }
    }
    
    env_list
}

/// 为 flatpak 注入 --env= / --socket= / --filesystem= 参数
///
/// 设计原则（v0.8.2 起）：**通用协议检测，不硬编码任何应用**。
/// 1. 每个 flatpak 应用启动时读它自己的 manifest（flatpak info --show-metadata），
///    检测它声明支持哪些协议（wayland / x11 / pulseaudio / pipewire）。
/// 2. **wayland + x11 都完整暴露**（--socket=wayland 和 --socket=x11 都不省略）：
///    - wayland：flatpak 读宿主 WAYLAND_DISPLAY=wayland-1（我们的 compositor）→ bind 进沙箱；
///    - x11：flatpak 把整个 /tmp/.X11-unix bind 进沙箱并默认设 DISPLAY=宿主值，
///      我们用 --env=DISPLAY=:N 显式覆盖指向 xwayland-satellite → 任何走 X11 的窗口都回 Minecraft。
///    - 之前 v0.2.18~v0.8 对 wayland 应用 --nosocket=x11、对 x11-only 应用只 bind 单个
///      X socket 文件，导致微信等 x11-only 应用 X11 环境不完整、极早期崩溃。现在不再省略。
/// 3. 后端变量按 manifest 检测注入：支持 wayland → 强制 wayland 后端；只支持 x11 → xcb/x11；
///    manifest 自带 QT_QPA_PLATFORM/GDK_BACKEND（如微信 =xcb）→ 尊重它，不覆盖。
/// 4. 检测失败（flatpak 不可用/未安装）→ 保守：只给 socket+DISPLAY，不强制后端变量，
///    让应用用自己默认（Qt→xcb 连 :N，GTK→wayland，Electron→auto），依然回 Minecraft。
/// 5. 音频：manifest 声明 pulseaudio/pipewire 时，确保宿主音频 socket 可见
///    （--socket=pulseaudio / --socket=pipewire，flatpak 1.12+ 支持）。
fn inject_flatpak_env(args: &mut Vec<String>, wayland_display: &str, display: &str, runtime_dir: &str) {
    // 找到 app_id 的位置（run 之后第一个非选项参数），同时拿到 app_id 本身
    let mut insert_pos = None;
    let mut app_id: Option<String> = None;
    let mut found_run = false;
    for (i, arg) in args.iter().enumerate() {
        if arg == "run" {
            found_run = true;
            continue;
        }
        if found_run && !arg.starts_with('-') {
            insert_pos = Some(i);
            app_id = Some(arg.clone());
            break;
        }
    }
    let insert_pos = insert_pos.unwrap_or(args.len());
    let app_id = app_id.unwrap_or_default();
    let caps = detect_flatpak_caps(&app_id);

    // flatpak run 的选项必须放在 run 之后、app_id 之前
    let mut opts = vec![
        // 暴露宿主 wayland socket（flatpak 读宿主 WAYLAND_DISPLAY=wayland-1 → bind wayland-1 进沙箱）
        // 注意：不要再手动加 --filesystem=<runtime>/wayland-1，它会和 flatpak 自身的
        // wayland 导出机制冲突（"is not a symlink to ../../flatpak/wayland-1 as expected"），
        // 导致沙箱内 wayland-1 时而连不上（QQ 曾 "Failed to connect to Wayland display"）。
        "--socket=wayland".to_string(),
        format!("--env=WAYLAND_DISPLAY={}", wayland_display),
    ];

    // X11 策略（v0.8.2，依据 flatpak 官方文档）：
    // - 支持 wayland 的应用：--socket=fallback-x11（wayland 可用时 flatpak 不 bind X11 目录，
    //   不把宿主桌面 X socket 暴露进沙箱；应用 wayland 失败想 fallback x11 时，
    //   我们单独 bind 的 /tmp/.X11-unix/X<N> 文件保证 X 可用）→ 窗口回 Minecraft。
    // - 只支持 x11 的应用（微信等）：--socket=x11 完整 bind 整个 /tmp/.X11-unix，
    //   DISPLAY 显式指向 satellite → 完整 X11 环境，不崩溃。
    // - 检测失败：--socket=x11 保守完整给（避免 x11-only 应用没 X 可用）。
    if caps.detected && caps.has_wayland && caps.has_x11 {
        opts.push("--socket=fallback-x11".to_string());
    } else if !caps.detected || caps.has_x11 {
        opts.push("--socket=x11".to_string());
    }
    // DISPLAY 永远指向 xwayland-satellite（窗口回 Minecraft 的保证）；
    // 同时 bind satellite 的单个 X socket 文件，让 fallback-x11 场景下 X 也可见。
    if !display.is_empty() {
        let dpy = display.trim_start_matches(':');
        if !dpy.is_empty() {
            opts.push(format!("--filesystem=/tmp/.X11-unix/X{}", dpy));
            opts.push(format!("--env=DISPLAY={}", display));
        }
    }

    // 音频：flatpak 合法 socket 只有 pulseaudio（无 pipewire 选项）。
    // 显式 --socket=pulseaudio：即使应用 manifest 没声明（如部分微信/QQ 版本），
    // 沙箱内也有音频能力——flatpak 会 bind 宿主 pulse socket（pipewire-pulse 兼容）。
    // 宿主没有音频服务时 flatpak 跳过，不影响启动。宿主侧 waylandcraft 按 PID 捕获。
    // 若应用 manifest 已声明 pulseaudio，重复声明无害（flatpak 去重）。
    opts.push("--socket=pulseaudio".to_string());

    // 后端变量：只在成功检测到 manifest 时注入；检测失败让应用用自己默认
    // （Qt→xcb 连 :N，GTK→wayland，Electron→auto，都能回 Minecraft）。
    if caps.detected {
        // 尊重 manifest 自带的 QPA/GDK（如微信 QT_QPA_PLATFORM=xcb），否则按支持情况注入
        match &caps.manifest_qpa {
            Some(v) => log(&format!("[flatpak] {} manifest QT_QPA_PLATFORM={} (respect)", app_id, v)),
            None => {
                let v = if caps.has_wayland { "wayland" } else { "xcb" };
                opts.push(format!("--env=QT_QPA_PLATFORM={}", v));
            }
        }
        match &caps.manifest_gdk {
            Some(v) => log(&format!("[flatpak] {} manifest GDK_BACKEND={} (respect)", app_id, v)),
            None => {
                let v = if caps.has_wayland { "wayland" } else { "x11" };
                opts.push(format!("--env=GDK_BACKEND={}", v));
            }
        }
        // Electron 类应用（manifest 一般不带 ozone 变量）：wayland 支持时 auto，否则 x11
        let ozone = if caps.has_wayland { "auto" } else { "x11" };
        opts.push(format!("--env=ELECTRON_OZONE_PLATFORM_HINT={}", ozone));
    }

    for (offset, opt) in opts.iter().enumerate() {
        log(&format!("[flatpak] injecting: {}", opt));
        args.insert(insert_pos + offset, opt.clone());
    }
}

/// 写一个调试脚本，启动前先 dump 环境变量到文件
fn write_env_dump_script(cmd: &str, args: &[String], env_list: &[(String, String)]) -> String {
    let dump_file = format!("/tmp/wlc-env-{}.log", std::process::id());
    let mut script = String::new();
    script.push_str("#!/bin/bash\n");
    script.push_str(&format!("echo '=== ENV DUMP for {}' > {}\n", cmd, dump_file));
    script.push_str(&format!("echo 'WAYLAND_DISPLAY='$WAYLAND_DISPLAY >> {}\n", dump_file));
    script.push_str(&format!("echo 'DISPLAY='$DISPLAY >> {}\n", dump_file));
    script.push_str(&format!("echo 'GDK_BACKEND='$GDK_BACKEND >> {}\n", dump_file));
    script.push_str(&format!("echo 'QT_QPA_PLATFORM='$QT_QPA_PLATFORM >> {}\n", dump_file));
    script.push_str(&format!("echo 'OZONE_PLATFORM='$OZONE_PLATFORM >> {}\n", dump_file));
    script.push_str(&format!("echo 'ELECTRON_OZONE_PLATFORM_HINT='$ELECTRON_OZONE_PLATFORM_HINT >> {}\n", dump_file));
    script.push_str(&format!("echo 'XDG_RUNTIME_DIR='$XDG_RUNTIME_DIR >> {}\n", dump_file));
    script.push_str(&format!("env | sort >> {}\n", dump_file));
    script.push_str(&format!("echo '=== END ENV DUMP' >> {}\n", dump_file));
    // 然后 exec 真正的程序
    script.push_str(&format!("exec {} {}\n", shell_quote(cmd), args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")));
    
    let script_path = "/tmp/wlc-spawn-wrapper.sh".to_string();
    if let Ok(mut f) = std::fs::File::create(&script_path) {
        let _ = f.write_all(script.as_bytes());
    }
    let _ = std::fs::set_permissions(&script_path, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    script_path
}

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/' || c == '=') {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn spawn(
    cmd: String,
    args: Vec<String>,
    env: Vec<(OsString, OsString)>,
    wayland_display: String,
    _runtime_dir: String,
) -> Result<(), ()> {
    log("========================================");
    log(&format!("[spawn] cmd={}", cmd));
    log(&format!("[spawn] args={:?}", args));
    log(&format!("[spawn] WAYLAND_DISPLAY target={}", wayland_display));
    log(&format!("[spawn] current env: WAYLAND_DISPLAY={:?}, DISPLAY={:?}", 
        std::env::var("WAYLAND_DISPLAY").unwrap_or_default(),
        std::env::var("DISPLAY").unwrap_or_default()));

    // 从 bridge 传入的 env 中提取 DISPLAY（xwayland-satellite 提供时）
    let display = env.iter()
        .find(|(k, _)| k == "DISPLAY")
        .map(|(_, v)| v.to_string_lossy().to_string())
        .unwrap_or_default();
    log(&format!("[spawn] DISPLAY from bridge={:?}", display));

    let app_type = detect_app_type(&cmd, &args);
    log(&format!("[detect] type={}", app_type));
    
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
    
    let (final_cmd, final_args) = if app_type == "flatpak" {
        let mut flatpak_args = args.clone();
        inject_flatpak_env(&mut flatpak_args, &wayland_display, &display, &runtime_dir);
        log(&format!("[flatpak] final args={:?}", flatpak_args));
        (cmd.clone(), flatpak_args)
    } else {
        // 非 flatpak: 用 bash -c 包装，强制设置环境变量后 exec
        let env_list = build_env_list(app_type, &wayland_display, &display);
        log(&format!("[env] will set: {:?}", env_list));
        
        // 构建 bash 命令: export VAR=val; exec cmd args...
        let mut bash_cmd = String::new();
        for (k, v) in &env_list {
            bash_cmd.push_str(&format!("export {}={}; ", k, shell_quote(v)));
        }
        // XDG_RUNTIME_DIR 已在外部获取
        bash_cmd.push_str(&format!("export XDG_RUNTIME_DIR={}; ", shell_quote(&runtime_dir)));
        
        bash_cmd.push_str("env > /tmp/wlc-env-dump.log; ");
        bash_cmd.push_str(&format!("exec {} {}", shell_quote(&cmd), args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")));
        
        log(&format!("[bash] cmd: {}", bash_cmd));
        ("/bin/bash".to_string(), vec!["-c".to_string(), bash_cmd])
    };
    
    log(&format!("[spawn] final_cmd={}, final_args={:?}", final_cmd, final_args));
    // 把最终命令打进游戏日志（stderr 会被 Minecraft 捕获），方便直接看 flatpak 注入是否生效
    eprintln!("[waylandcraft] spawn: cmd={} args={:?} WAYLAND_DISPLAY={} DISPLAY={:?} XDG_RUNTIME_DIR={}",
        final_cmd, final_args, wayland_display, display, runtime_dir);

    // 子进程 stdout/stderr -> per-app 日志文件，不再丢进 null
    let out_file = app_log_file(&final_cmd);
    let err_file = out_file.try_clone().unwrap_or_else(|_| {
        std::fs::File::open("/dev/null").unwrap_or_else(|_| panic!("cannot open /dev/null"))
    });

    let mut command = Command::new(&final_cmd);
    command
        .args(&final_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        // 统一给子进程设置正确的 Wayland 环境
        // （flatpak 运行器需要宿主 XDG_RUNTIME_DIR 才能用 --filesystem=xdg-run 构建沙箱）
        .env("WAYLAND_DISPLAY", &wayland_display)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DISPLAY", &display);

    // double-fork：子进程立即退出，孙进程被 init 收养，避免僵尸；
    // 孙进程里直接 exec（不再调用 Command::spawn，避免 fork 后重用 std 的锁/allocator 风险）
    match unsafe { libc::fork() } {
        0 => {
            unsafe { libc::setsid(); }
            match unsafe { libc::fork() } {
                0 => {
                    // 孙进程：exec 替换成目标程序。exec 失败才返回。
                    let err = command.exec();
                    log(&format!("[spawn] exec FAILED for '{}': {}", final_cmd, err));
                    eprintln!("[waylandcraft] exec FAILED for '{}': {}", final_cmd, err);
                    unsafe { libc::_exit(127); }
                }
                -1 => {
                    log("[spawn] second fork() failed!");
                    unsafe { libc::_exit(1); }
                }
                _ => unsafe { libc::_exit(0); },
            }
        }
        -1 => {
            log("[spawn] fork() failed!");
            return Err(());
        }
        _ => {}
    }

    unsafe { libc::wait(std::ptr::null_mut()); }
    log(&format!("[spawn] done for cmd={}", cmd));
    Ok(())
}
