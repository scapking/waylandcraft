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

/// 检测应用类型。
///
/// 通用化设计（v0.8.5）：**只识别"沙箱运行器"，不识别具体应用**。
/// - 只有隔离环境的运行器（flatpak / snap）需要特殊处理：它们把宿主 socket 藏进沙箱，
///   必须显式暴露 wayland/x11/audio socket，否则应用连不到我们的 compositor。
/// - 其余一切（原生二进制、AppImage、Nix、deb、portable、wine、electron、gtk、qt、
///   浏览器、以及未来任何新打包格式）都是"直接 exec 的进程"——它们继承宿主环境，
///   统一走通用环境注入（见 build_universal_env_list）：注入"wayland 优先 + x11 自动
///   fallback"的 toolkit 变量，让**应用自己探测**后端，不需要我们认识它。
///
/// 所以"新的环境 app 怎么检测"的答案是：**后端协议不检测**（通用注入让应用自探测），
/// 唯一可能新增的识别是"新的隔离沙箱运行器"——出现时只需在这里加一行识别它的命令，
/// 其余逻辑（全量暴露 socket）完全复用。
fn detect_app_type(cmd: &str) -> &'static str {
    let cmd_lower = cmd.to_lowercase();
    if cmd_lower.ends_with("/flatpak") || cmd_lower == "flatpak"
        || cmd_lower.ends_with("/flatpak-spawn") {
        return "flatpak";
    }
    if cmd_lower.ends_with("/snap") || cmd_lower == "snap"
        || cmd_lower.ends_with("/snap-confine") || cmd_lower.contains("/snap/bin/") {
        return "snap";
    }
    "app"
}

/// 通用环境注入：对**所有**非沙箱应用注入同一份环境，让应用自己选后端。
///
/// 为什么可以全量注入而不用按应用分类：
/// 每个 toolkit 只读自己的变量，互不干扰；且每个 toolkit 官方都支持
/// "wayland 优先、失败自动 fallback x11"：
/// - GTK:        GDK_BACKEND=wayland,x11    —— 逗号列表按序尝试，wayland 连不上自动 x11
/// - Qt:         QT_QPA_PLATFORM=wayland;xcb —— 分号列表是 Qt 官方 fallback 机制
/// - Firefox:    MOZ_ENABLE_WAYLAND=1       —— 连不上自动退 X11
/// - Chromium:   OZONE_PLATFORM_HINT=auto   —— 有 wayland 用 wayland，否则 x11
/// - Electron:   ELECTRON_OZONE_PLATFORM_HINT=auto —— 同 Chromium
/// - 其他 native: 只看 DISPLAY（已指向 xwayland-satellite）→ X11 回 Minecraft
///
/// 这样 AppImage / Nix / deb / portable / wine / 未来任何新格式都不用识别：
/// 只要它是直接 exec 的进程，这套环境就让它 wayland 可用时走 wayland（原生渲染），
/// 不可用时自动退 x11（satellite 渲染），全部回 Minecraft。
///
/// C 方案（v0.9.39+）：显式传递 `DBUS_SESSION_BUS_ADDRESS` —— 这是
/// 嵌套应用（如 firefox、gnome-terminal）通过 GdkIMContext 找宿主 IME daemon
/// 的关键变量。如果不传，应用会尝试默认 `$XDG_RUNTIME_DIR/bus`，但**嵌套
/// 合成器里 `$XDG_RUNTIME_DIR` 是我们写的**（不是宿主），可能导致 bus
/// 路径错误。显式传宿主的 bus 地址确保嵌套应用直接连宿主 IME daemon。
fn build_universal_env_list(wayland_display: &str, display: &str) -> Vec<(String, String)> {
    let mut env = vec![
        ("WAYLAND_DISPLAY".to_string(), wayland_display.to_string()),
        ("DISPLAY".to_string(), display.to_string()),
        // 会话类型告知：我们就是 wayland 会话（compositor 真实存在）
        ("XDG_SESSION_TYPE".to_string(), "wayland".to_string()),
        ("GDK_BACKEND".to_string(), "wayland,x11".to_string()),
        ("QT_QPA_PLATFORM".to_string(), "wayland;xcb".to_string()),
        ("MOZ_ENABLE_WAYLAND".to_string(), "1".to_string()),
        ("OZONE_PLATFORM_HINT".to_string(), "auto".to_string()),
        ("ELECTRON_OZONE_PLATFORM_HINT".to_string(), "auto".to_string()),
    ];
    // C 方案：嵌套应用需要宿主 dbus 路径找 IME daemon（ibus / fcitx5）
    // 如果有就传（最常见 unix:path=/run/user/1000/bus）
    if let Ok(bus) = std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        env.push(("DBUS_SESSION_BUS_ADDRESS".to_string(), bus));
    }
    env
}

/// 为 flatpak 注入 --env= / --socket= / --filesystem= 参数
///
/// 通用化设计（v0.8.5）：**不读 manifest、不猜协议、不按应用分类**。
/// 对每个 flatpak 应用无差别做同一件事：把 wayland / x11 / pulseaudio 三个 socket
/// 全部暴露进沙箱 + DISPLAY/WAYLAND_DISPLAY 指向我们的 compositor/satellite。
/// 然后**完全不注入任何后端变量**（QT_QPA_PLATFORM / GDK_BACKEND /
/// MOZ_ENABLE_WAYLAND / OZONE 一律不动）：
/// - manifest 自带后端变量（如微信 QT_QPA_PLATFORM=xcb）→ 尊重，沙箱内它自己会用它；
/// - 没带的 → 应用用默认（GTK 检测到 WAYLAND_DISPLAY → wayland；Qt → xcb 连 :N；
///   Electron → auto），反正两个 socket 都在，怎么选都回 Minecraft。
///
/// 这样任何 flatpak 应用（现在的、未来的、新装上的）都无需识别，行为一致：
/// 支持 wayland 的走我们的 compositor（原生渲染），只支持 x11 的走 satellite（抓帧），
/// 音频走 pulseaudio socket（pipewire-pulse 兼容）→ 宿主按 PID 捕获。
fn inject_flatpak_env(args: &mut Vec<String>, wayland_display: &str, display: &str) {
    // 找到 app_id 的位置（run 之后第一个非选项参数）
    let mut insert_pos = None;
    let mut found_run = false;
    for (i, arg) in args.iter().enumerate() {
        if arg == "run" {
            found_run = true;
            continue;
        }
        if found_run && !arg.starts_with('-') {
            insert_pos = Some(i);
            break;
        }
    }
    let insert_pos = insert_pos.unwrap_or(args.len());

    // flatpak run 的选项必须放在 run 之后、app_id 之前
    let mut opts = vec![
        // 暴露宿主 wayland socket（flatpak 读宿主 WAYLAND_DISPLAY=wayland-1 → bind wayland-1 进沙箱）
        // 注意：不要再手动加 --filesystem=<runtime>/wayland-1，它会和 flatpak 自身的
        // wayland 导出机制冲突（"is not a symlink to ../../flatpak/wayland-1 as expected"），
        // 导致沙箱内 wayland-1 时而连不上（QQ 曾 "Failed to connect to Wayland display"）。
        "--socket=wayland".to_string(),
        format!("--env=WAYLAND_DISPLAY={}", wayland_display),
        // X11 完整暴露：--socket=x11 bind 整个 /tmp/.X11-unix 进沙箱，
        // x11-only 应用（微信等）环境完整不崩溃（v0.7.1 教训：只 bind 单个文件会早期崩溃）。
        // 支持 wayland 的应用即使也走 x11 也照样能连 satellite——反正 DISPLAY 指向它。
        "--socket=x11".to_string(),
        // 音频：flatpak 合法 socket 只有 pulseaudio（无 pipewire 选项）。
        // 显式给上，即使 manifest 没声明（如部分微信/QQ 版本）沙箱内也有音频能力；
        // flatpak bind 宿主 pulse socket（pipewire-pulse 兼容），宿主无音频服务时跳过不影响启动。
        "--socket=pulseaudio".to_string(),
    ];

    // DISPLAY 永远指向 xwayland-satellite（窗口回 Minecraft 的保证）；
    // 同时 bind satellite 的单个 X socket 文件，双保险。
    if !display.is_empty() {
        let dpy = display.trim_start_matches(':');
        if !dpy.is_empty() {
            opts.push(format!("--filesystem=/tmp/.X11-unix/X{}", dpy));
            opts.push(format!("--env=DISPLAY={}", display));
        }
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

    let app_type = detect_app_type(&cmd);
    log(&format!("[detect] type={}", app_type));
    
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
    
    let (final_cmd, final_args) = if app_type == "flatpak" {
        let mut flatpak_args = args.clone();
        inject_flatpak_env(&mut flatpak_args, &wayland_display, &display);
        log(&format!("[flatpak] final args={:?}", flatpak_args));
        (cmd.clone(), flatpak_args)
    } else {
        if app_type == "snap" {
            // snap 是沙箱运行器但没有 flatpak 那样的 --socket 注入选项：
            // snap 的沙箱接口（wayland / x11 / audio-playback）在安装时由 snap connect
            // 配置，运行时 snap run 继承宿主环境变量 → 下面的通用 bash 注入一样生效。
            // 若应用连不上，多半是接口没连，提示用户：
            //   snap connect <app>:wayland snapd::wayland
            //   snap connect <app>:x11 snapd::x11
            //   snap connect <app>:audio-playback snapd::audio-playback
            log("[snap] universal env injection (ensure snap interfaces wayland/x11/audio-playback are connected)");
        }
        // 非沙箱 / snap: 用 bash -c 包装，强制设置环境变量后 exec
        let env_list = build_universal_env_list(&wayland_display, &display);
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
