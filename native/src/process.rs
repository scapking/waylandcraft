use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::io::Write;
use std::sync::{LazyLock, Mutex};

fn log(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open("/tmp/waylandcraft-launch.log")
    {
        let _ = writeln!(f, "{}", msg);
    }
}

// ═══ 会话收养登记表（fix/app-launcher-lifecycle）═══
//
// 根因：单实例应用（code/firefox/Discord）在**上一个游戏会话**里被 spawn 过，
// 那个会话的合成器 socket 随 JVM 退出而死，但应用进程存活（double-fork 后由
// init 收养，mod 从未知道它的 PID）。新会话 `/wl launch` 同一应用时，CLI 进程
// 把启动请求"交接"给这个绑死在陈旧 socket 上的旧实例后自行退出——旧实例窗口
// 永远出不来，表现为 mapped=0、进程在、无 toplevel。
//
// 修复三件套：
// 1. spawn 通过 pipe 把孙进程（exec 后即应用主进程）PID 写回父进程，连同
//    其进程组（setsid 的子进程 PID）记入本登记表——"收养"。
// 2. 每个被 spawn 的进程注入 WAYLANDCRAFT_SESSION 标记（<jvmPid>-<wayland-N>），
//    Java 侧据此区分"本会话实例 / 陈旧会话孤儿 / 宿主进程"（判定纯函数见
//    AppLifecycleLogic，绝不误杀宿主同名进程）。
// 3. 断开/退出与 /wl close 按登记表精确终止进程树：SIGTERM→限时宽限→SIGKILL。

#[derive(Clone, Debug)]
pub struct AdoptedApp {
    /// 孙进程 PID（exec 替换后即应用主进程）
    pub pid: i32,
    /// 进程组/会话 id = 中间子进程（setsid 的会话首进程）PID。
    /// 内核保证：只要进程组还有存活成员，该 PID 不会被复用，
    /// 因此 kill(-pgid) 始终命中本应用进程树，不会误伤复用同 PID 的新进程。
    pub pgid: i32,
    /// 启动命令 argv0（展示与排错用）
    pub cmd: String,
}

static ADOPTED: LazyLock<Mutex<Vec<AdoptedApp>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 会话标记（pid→env→归属会话判定的核心原料，与 Java 侧共享同一公式）。
pub fn session_marker_for(wayland_display: &str) -> String {
    format!("{}-{}", std::process::id(), wayland_display)
}

fn proc_alive(pid: i32) -> bool {
    pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

/// 进程组是否仍有存活成员（signal 0 探测：ESRCH → 组已消亡）。
fn group_alive(pgid: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// 列出收养进程（顺带回收主进程已退出的条目——树随主进程消亡是常态，
/// 残留组内孤儿由 close/terminate 的组杀兜底，不在这里追杀）。
pub fn list_adopted() -> Vec<AdoptedApp> {
    let Ok(mut reg) = ADOPTED.lock() else {
        return Vec::new();
    };
    reg.retain(|a| proc_alive(a.pid));
    reg.iter().cloned().collect()
}

fn record_adopted(pid: i32, pgid: i32, cmd: &str) {
    let Ok(mut reg) = ADOPTED.lock() else { return };
    reg.push(AdoptedApp { pid, pgid, cmd: cmd.to_string() });
    log(&format!("[adopt] tracked pid={} pgid={} cmd={}", pid, pgid, cmd));
}

fn kill_pid(pid: i32, sig: i32) {
    if pid > 1 {
        unsafe { libc::kill(pid, sig); }
    }
}

fn kill_group(pgid: i32, sig: i32) {
    // 双保险：pgid<=1 绝不发（-1 = 全进程表）；也绝不误杀游戏自身进程组。
    if pgid > 1 && pgid != unsafe { libc::getpgid(0) } {
        unsafe { libc::kill(-pgid, sig); }
    }
}

/// 后台升级线程：force=直接 SIGKILL；否则 SIGTERM→宽限（默认 4s 轮询）→SIGKILL。
/// 结束（或发起时即目标全灭）后移除登记，避免陈旧条目污染 list/status。
fn escalate_kill(pid: i32, pgid: i32, force: bool) {
    std::thread::spawn(move || {
        if force {
            kill_group(pgid, libc::SIGKILL);
            kill_pid(pid, libc::SIGKILL);
        } else {
            kill_group(pgid, libc::SIGTERM);
            kill_pid(pid, libc::SIGTERM);
            for _ in 0..40 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if !proc_alive(pid) && !group_alive(pgid) {
                    break;
                }
            }
            if proc_alive(pid) || group_alive(pgid) {
                log(&format!("[adopt] pid={} pgid={} ignored SIGTERM, escalating SIGKILL", pid, pgid));
                kill_group(pgid, libc::SIGKILL);
                kill_pid(pid, libc::SIGKILL);
            }
        }
        if let Ok(mut reg) = ADOPTED.lock() {
            reg.retain(|a| a.pid != pid);
        }
    });
}

/// 按收养登记精确终止单个应用进程树。pid 不在登记表 → false（调用方决定兜底）。
pub fn kill_adopted(pid: i32, force: bool) -> bool {
    let entry = list_adopted().into_iter().find(|a| a.pid == pid);
    let Some(entry) = entry else {
        return false;
    };
    escalate_kill(entry.pid, entry.pgid, force);
    true
}

/// 游戏断开/退出：终止**全部**本会话收养的进程树（SIGTERM→限时→SIGKILL）。
/// 立即返回（升级在后台线程完成），不阻塞客户端 tick 线程。
pub fn terminate_all_adopted() {
    let entries = list_adopted();
    if entries.is_empty() {
        return;
    }
    log(&format!("[adopt] session ending: terminating {} adopted app(s)", entries.len()));
    for e in entries {
        escalate_kill(e.pid, e.pgid, false);
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
fn build_universal_env_list(wayland_display: &str, display: &str, session_marker: &str) -> Vec<(String, String)> {
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
    // 会话标记：本 mod spawn 的每个进程都带上 "<jvmPid>-<wayland-N>"。
    // 旧会话孤儿（jvmPid 已死）与新会话（N 可能因 socket 文件删除+重建而重名）
    // 由此可严格区分——启动前的陈旧实例检测与 /wl close 精确化都以此为锚。
    if !session_marker.is_empty() {
        env.push(("WAYLANDCRAFT_SESSION".to_string(), session_marker.to_string()));
    }
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
fn inject_flatpak_env(args: &mut Vec<String>, wayland_display: &str, display: &str, session_marker: &str) {
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

    // 会话标记进沙箱：flatpak 沙箱内应用同样带 WAYLANDCRAFT_SESSION，
    // 陈旧检测/收养归属对 flatpak 应用同样成立（否则 flatpak 版微信/QQ 漏网）。
    if !session_marker.is_empty() {
        opts.push(format!("--env=WAYLANDCRAFT_SESSION={}", session_marker));
    }

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
    let session_marker = session_marker_for(&wayland_display);

    let (final_cmd, final_args) = if app_type == "flatpak" {
        let mut flatpak_args = args.clone();
        inject_flatpak_env(&mut flatpak_args, &wayland_display, &display, &session_marker);
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
        let env_list = build_universal_env_list(&wayland_display, &display, &session_marker);
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
        // 游戏进程继承的启动器环境不应透传给被拉起的客户端 [upstream f21ea6f]：
        // - LD_LIBRARY_PATH: 启动器为游戏注入的 native 库路径（nix/mesa、
        //   Android native-deps 预载目录等），会污染子应用的动态链接
        //   （与 satellite.rs 的 env_remove 先例一致；Android 的 .so 预载发生
        //   在游戏自身进程里，删子进程的环境变量不影响它）
        // - RUST_BACKTRACE: 仅对 waylandcraft/游戏自身调试有意义
        // - NO_COLOR: Prism 等启动器默认设置，不应强制子应用禁色
        // - __GL_THREADED_OPTIMIZATIONS: 为游戏性能追加的驱动调优，
        //   泄漏给子进程会改变其 GL 线程模型
        // 注意：与上游不同，fork 特意不删 DISPLAY/WAYLAND_DISPLAY——嵌套合成器
        // 需要客户端连它们（下方显式覆盖为 compositor 的值）。
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("RUST_BACKTRACE")
        .env_remove("NO_COLOR")
        .env_remove("__GL_THREADED_OPTIMIZATIONS")
        // 统一给子进程设置正确的 Wayland 环境
        // （flatpak 运行器需要宿主 XDG_RUNTIME_DIR 才能用 --filesystem=xdg-run 构建沙箱）
        .env("WAYLAND_DISPLAY", &wayland_display)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DISPLAY", &display)
        // 会话标记对**所有**路径生效（flatpak runner 外层进程也带上，
        // 沙箱内层经 --env= 注入，见 inject_flatpak_env）
        .env("WAYLANDCRAFT_SESSION", &session_marker);

    // double-fork：子进程立即退出，孙进程被 init 收养，避免僵尸；
    // 孙进程里直接 exec（不再调用 Command::spawn，避免 fork 后重用 std 的锁/allocator 风险）。
    // 收养登记：子进程把孙进程 PID 经 pipe 写回父进程（孙进程 exec 后即应用主进程，
    // 其进程组 = 子进程 setsid 后的 PGID）。没有这个 PID，跨会话孤儿就无从终止。
    let mut fds = [0 as libc::c_int; 2];
    let pipe_ok = unsafe { libc::pipe(fds.as_mut_ptr()) } == 0;
    if !pipe_ok {
        log("[spawn] pipe() failed - app will spawn untracked (adoption disabled for this launch)");
    }

    match unsafe { libc::fork() } {
        0 => {
            if pipe_ok {
                unsafe { libc::close(fds[0]); }
            }
            unsafe { libc::setsid(); }
            match unsafe { libc::fork() } {
                0 => {
                    // 孙进程：关闭 pipe 端再 exec（fd 绝不能泄进应用）。
                    // pipe_ok=false 时 fds 是 [0,0]，碰都不许碰（会关掉 stdin）。
                    if pipe_ok {
                        unsafe { libc::close(fds[1]); }
                    }
                    let err = command.exec();
                    log(&format!("[spawn] exec FAILED for '{}': {}", final_cmd, err));
                    eprintln!("[waylandcraft] exec FAILED for '{}': {}", final_cmd, err);
                    unsafe { libc::_exit(127); }
                }
                -1 => {
                    // 第二层 fork 失败：回写 -1，父进程据此返回 Err（不误报启动成功）。
                    if pipe_ok {
                        write_pid(fds[1], -1);
                        unsafe { libc::close(fds[1]); }
                    }
                    log("[spawn] second fork() failed!");
                    unsafe { libc::_exit(1); }
                }
                grandchild => {
                    if pipe_ok {
                        write_pid(fds[1], grandchild);
                        unsafe { libc::close(fds[1]); }
                    }
                    unsafe { libc::_exit(0); }
                }
            }
        }
        -1 => {
            if pipe_ok {
                unsafe { libc::close(fds[0]); libc::close(fds[1]); }
            }
            log("[spawn] fork() failed!");
            return Err(());
        }
        middle => {
            // 父进程（游戏 JVM 内的 native 线程）：读孙 PID → 收尸中间子进程 → 登记。
            if pipe_ok {
                unsafe { libc::close(fds[1]); }
            }
            let grandchild = if pipe_ok { read_pid(fds[0]) } else { 0 };
            if pipe_ok {
                unsafe { libc::close(fds[0]); }
            }
            unsafe { libc::wait(std::ptr::null_mut()); }
            match grandchild {
                -1 => {
                    log("[spawn] second fork failed in child; launch reported as error");
                    return Err(());
                }
                g if g > 0 => record_adopted(g, middle, &cmd),
                _ => {
                    // pipe 不可用/读失败：进程可能已正常 spawn，但本会话无法收养它。
                    log("[spawn] adopted pid unavailable - app running untracked this session");
                }
            }
        }
    }

    log(&format!("[spawn] done for cmd={}", cmd));
    Ok(())
}

fn write_pid(fd: libc::c_int, pid: libc::pid_t) {
    let bytes = pid.to_ne_bytes();
    let mut off = 0usize;
    while off < bytes.len() {
        let n = unsafe {
            libc::write(fd, bytes[off..].as_ptr() as *const libc::c_void, bytes.len() - off)
        };
        if n <= 0 {
            return; // 短写：读端拿到残缺数据由 read_pid 的循环长度校验兜住
        }
        off += n as usize;
    }
}

/// 读满 4 字节返回 PID；读不满（pipe 关闭/损坏）返回 0（视为"无 PID 可收养"）。
fn read_pid(fd: libc::c_int) -> i32 {
    let mut buf = [0u8; 4];
    let mut off = 0usize;
    while off < buf.len() {
        let n = unsafe { libc::read(fd, buf[off..].as_mut_ptr() as *mut libc::c_void, buf.len() - off) };
        if n <= 0 {
            return 0;
        }
        off += n as usize;
    }
    i32::from_ne_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_marker_formula_stable_and_distinct() {
        // 公式 = "<jvmPid>-<wayland-N>"：同 JVM 同 socket 两次调用一致；
        // socket 名不同（新旧会话）必然不同——陈旧判定的唯一锚点。
        let a = session_marker_for("wayland-1");
        let b = session_marker_for("wayland-1");
        let c = session_marker_for("wayland-2");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.contains(&std::process::id().to_string()));
        assert!(a.ends_with("-wayland-1"));
    }

    #[test]
    fn universal_env_carries_session_marker() {
        let env = build_universal_env_list("wayland-3", ":0", "4242-wayland-3");
        let hit = env.iter().find(|(k, _)| k == "WAYLANDCRAFT_SESSION");
        assert_eq!(hit.map(|(_, v)| v.as_str()), Some("4242-wayland-3"));
        // 空标记（理论不可达，防御）不注入垃圾变量
        let env2 = build_universal_env_list("wayland-3", ":0", "");
        assert!(!env2.iter().any(|(k, _)| k == "WAYLANDCRAFT_SESSION"));
    }

    #[test]
    fn flatpak_opts_carry_session_marker_before_app_id() {
        // flatpak run [opts] app-id --args：选项必须在 run 之后 app_id 之前，
        // 且标记必须进沙箱（否则 flatpak 版单实例应用逃过陈旧检测）。
        let mut args = vec!["run".to_string(), "--branch=x".to_string(),
            "com.tencent.WeChat".to_string(), "--foo".to_string()];
        inject_flatpak_env(&mut args, "wayland-1", ":9", "777-wayland-1");
        let marker_pos = args.iter().position(|a| a == "--env=WAYLANDCRAFT_SESSION=777-wayland-1")
            .expect("marker opt missing");
        let app_pos = args.iter().position(|a| a == "com.tencent.WeChat").unwrap();
        let run_pos = args.iter().position(|a| a == "run").unwrap();
        assert!(run_pos < marker_pos && marker_pos < app_pos);
        assert!(args.iter().any(|a| a == "--env=WAYLAND_DISPLAY=wayland-1"));
    }

    #[test]
    fn proc_alive_semantics() {
        assert!(proc_alive(std::process::id() as i32));
        assert!(!proc_alive(0));
        assert!(!proc_alive(-1));
        // 几乎不可能存在的超界 pid（Linux pid_max <= 4194304）
        assert!(!proc_alive(4194303));
    }

    #[test]
    fn group_kill_guard_rejects_dangerous_ids() {
        // pgid<=1 与本进程组绝不发信号（-1 = 全体进程，杀到游戏自己就是灾难）。
        // 用 0 号信号验证守卫不外溢：函数直接返回，不产生任何 EPERM/ESRCH 副作用。
        let our_pgid = unsafe { libc::getpgid(0) };
        kill_group(1, 0);
        kill_group(0, 0);
        kill_group(-1, 0);
        kill_group(our_pgid, 0); // 本组被守卫拒绝——绝不误伤 JVM 自身
        kill_pid(1, 0); // init（pid 1）被 `pid > 1` 守卫排除
        kill_pid(0, 0); // 0 号同样被拒（kill(0)=发给本组）
    }

    /// 真机闭环（非沙箱环境手动跑）：spawn → pipe 回读 PID → environ 带会话标记
    /// → kill_adopted(force) 进程树消亡。验证收养机制的三个关键事实，而不是 mock。
    /// 运行：cargo test --lib -- --ignored --test-threads=1 adopted_spawn_end_to_end
    #[test]
    #[ignore = "fork/exec 真实进程操作，单测并行环境下隔离运行（见 docs/APPS.md 验证节）"]
    fn adopted_spawn_end_to_end() {
        let marker_before = session_marker_for("wayland-77");
        spawn(
            "/bin/sleep".to_string(),
            vec!["600".to_string()],
            vec![],
            "wayland-77".to_string(),
            String::new(),
        )
        .expect("spawn failed");

        let apps = list_adopted();
        let me = apps.iter().find(|a| a.cmd == "/bin/sleep").expect("adopted entry missing");
        let pid = me.pid;

        // /proc/pid/environ 是**最近一次 exec 时刻**的快照：孙进程 fork 后、
        // Command::exec 完成前读到的是 JVM 的旧环境。轮询等待 exec 落地，
        // 任何断言失败路径都必须先杀掉进程再 panic（绝不留孤儿测试进程）。
        let mut marker_found = false;
        let mut env_str = String::new();
        for _ in 0..30 {
            if let Ok(raw) = std::fs::read(format!("/proc/{}/environ", pid)) {
                env_str = String::from_utf8_lossy(&raw).into_owned();
                if env_str.contains("WAYLANDCRAFT_SESSION=") {
                    marker_found = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // 失败路径一律先 SIGKILL 再 panic：真机测试绝不留孤儿进程
        if !(marker_found && env_str.contains(&marker_before)) {
            kill_pid(pid, libc::SIGKILL);
            panic!(
                "marker absent/wrong in exec'd environ (pid {} killed): {}",
                pid, &env_str[..env_str.len().min(200)]
            );
        }

        // 独立进程组（setsid 生效）：pgid == 登记的中间子 PID
        // /proc 格式：pid (comm) state ppid pgrp … → 末')'后第 3 个字段（索引 2）是 pgrp
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).unwrap_or_default();
        let pgid_field = stat.rsplit(')').next().unwrap().split_whitespace().nth(2).unwrap_or("");
        if pgid_field != me.pgid.to_string() {
            kill_pid(pid, libc::SIGKILL);
            panic!("child pgid {} != adopted pgid {} (pid killed)", pgid_field, me.pgid);
        }

        assert!(kill_adopted(pid, true), "kill_adopted rejected tracked pid");
        for _ in 0..50 {
            if !proc_alive(pid) {
                return; // 树已消亡，闭环成立
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        kill_pid(pid, libc::SIGKILL);
        panic!("adopted pid {} survived force kill", pid);
    }

    #[test]
    fn terminate_and_kill_adopted_reject_untracked_pids() {
        // 未登记的 PID 一律 false（close 路径据此回退提示，绝不盲杀）。
        assert!(!kill_adopted(std::process::id() as i32, true));
        assert!(!kill_adopted(4194303, false));
        // 空表时 terminate_all 早退，不派线程、不 panic。
        // （注：本测试与登记表共享进程全局状态，但纯单测进程里 spawn 从未发生，表恒空。）
        terminate_all_adopted();
        assert!(list_adopted().is_empty());
    }
}
