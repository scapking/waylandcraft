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
/// 修复说明：
/// 1. v0.2.15 用 `--filesystem=/run/user/1000/wayland-1`（绝对路径）暴露宿主 wayland socket，
///    但实测 QQ/WeChat 仍报 "Failed to connect to Wayland display: 没有那个文件或目录"——
///    flatpak 对 runtime 目录（/run/user/<uid>）内路径有预订保护，--filesystem 静默不生效。
///    **正确做法是 `--socket=wayland`**：flatpak 会读 flatpak 进程自身的 WAYLAND_DISPLAY
///    （我们在 spawn 时已设为 wayland-1 = WaylandCraft compositor），把宿主
///    /run/user/<uid>/wayland-1 bind 进沙箱同名路径。这是 flatpak 暴露 wayland socket 的
///    官方机制，能正确处理 runtime 目录绑定。
/// 2. 注意：之前 `--nosocket=wayland` 是为了阻止 manifest 默认把 WAYLAND_DISPLAY 指向宿主桌面
///    （GNOME wayland-0）。现在改为 `--socket=wayland` 后，因为宿主 env WAYLAND_DISPLAY=wayland-1
///    （而非 wayland-0），flatpak 暴露的正是我们的 compositor socket，应用不会跑到真实桌面。
/// 3. X11 应用还需要能看到 xwayland-satellite 的 X socket（/tmp/.X11-unix/X<dpy>），
///    所以额外暴露 /tmp/.X11-unix 目录。
/// 4. 找不到 app_id 时 insert_pos 保持 0，会把选项插到最前面（甚至插到 `run` 之前），
///    导致 flatpak 报错。现在找不到就追加到末尾。
/// 5. 有些 flatpak 应用只支持 X11/xcb（manifest 明确 QT_QPA_PLATFORM=xcb，
///    如 com.tencent.WeChat）。对它们绝不能注入 QT_QPA_PLATFORM=wayland / 
///    GDK_BACKEND=wayland——应用会找不到 wayland platform plugin 直接退出。
///    这类应用保持 xcb 后端，仅通过 --filesystem 暴露 xwayland-satellite 的
///    X socket 并把 DISPLAY 指向它，窗口就会出现在 Minecraft 世界里。
const X11_ONLY_FLATPAKS: &[&str] = &[
    "com.tencent.WeChat", // manifest: "Only supports xcb" → QT_QPA_PLATFORM=xcb
];

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
    let x11_only = X11_ONLY_FLATPAKS.contains(&app_id.as_str());

    // flatpak run 的选项必须放在 run 之后、app_id 之前
    let mut opts = vec![
        // 暴露宿主 wayland socket（flatpak 读宿主 WAYLAND_DISPLAY=wayland-1 → bind wayland-1 进沙箱）
        // 注意：不要再手动加 --filesystem=<runtime>/wayland-1，它会和 flatpak 自身的
        // wayland 导出机制冲突（"is not a symlink to ../../flatpak/wayland-1 as expected"），
        // 导致沙箱内 wayland-1 时而连不上（QQ 曾 "Failed to connect to Wayland display"）。
        "--socket=wayland".to_string(),
        format!("--env=WAYLAND_DISPLAY={}", wayland_display),
    ];

    if x11_only {
        // X11-only（微信等）：保留 xcb 后端，不注入 wayland 强制变量。
        // 显式设 xcb 以覆盖 manifest 缺失/冲突的情况；Qt 会连 DISPLAY 指向的 satellite。
        opts.push("--env=QT_QPA_PLATFORM=xcb".to_string());
        // 关键修复（v0.8.1）：不能再 --nosocket=x11！
        // 微信 manifest 声明 --socket=x11，之前用 --nosocket=x11 + 只 bind 单个
        // /tmp/.X11-unix/X<dpy> 文件，导致微信 Qt xcb 极早期崩溃
        // （Breakpad tgkill 刷屏，连自身日志都没打出来）。
        // 保留完整 X11 socket（flatpak 会 bind 整个 /tmp/.X11-unix 并设 DISPLAY=宿主值），
        // 但下方 --env=DISPLAY=:2 显式覆盖指向 xwayland-satellite → 窗口必然回 Minecraft。
    } else {
        opts.push("--nosocket=x11".to_string());
        opts.push("--env=GDK_BACKEND=wayland".to_string());
        opts.push("--env=QT_QPA_PLATFORM=wayland".to_string());
        opts.push("--env=ELECTRON_OZONE_PLATFORM_HINT=auto".to_string());
    }
    // X11-only flatpak apps need DISPLAY from xwayland-satellite；
    // 对 x11-only（微信）：--socket=x11 已 bind 整个 /tmp/.X11-unix，DISPLAY 指向 satellite 即可；
    // 对 wayland 应用：只 bind satellite 的单个 X socket（/tmp/.X11-unix/X<dpy>），
    // 不暴露宿主桌面的 X socket，这样沙箱内唯一可用的 X server 就是 satellite → 窗口必然回到 Minecraft。
    if !display.is_empty() {
        let dpy = display.trim_start_matches(':');
        if !dpy.is_empty() {
            if !x11_only {
                opts.push(format!("--filesystem=/tmp/.X11-unix/X{}", dpy));
            }
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
