//! 真 fcitx5 端到端验证（#[ignore]，本机手动跑）。
//!
//! 与 `tests.rs`（模拟 IME client）互补：本文件把**真 fcitx5 二进制**
//! fork/exec 进测试——环境按 `nested_daemon::build_spawn_plan` 的隔离纪律
//! 构造（独立 HOME/XDG_*、WAYLAND_DISPLAY 指向我们的监听 socket、
//! DBUS 剥离/私有总线），`WAYLAND_DEBUG=client` 与 fcitx5 `--verbose`
//! 全部落文件。测试在同一个 `WLCState` 合成器里同时挂：
//!
//! - 编辑器 client（真 wayland-client，ti3 + wl_keyboard）——模拟嵌套应用；
//! - 真 fcitx5（im2 + VK client）。
//!
//! 验收链：bind → activate → **grab** → 逐键 evdev 注入（拼音 nihao +
//! 空格选词）→ 断言编辑器 ti3 收到**非 ASCII commit_string**（汉字落地）。
//!
//! ## 取证背景（2026-11-25，sync6 实机症状）
//!
//! 实机 fcitx5 对每键回"未消费"（VK 直通字母进窗口），从不出
//! preedit/commit。源码取证（fcitx5 5.1.12）：
//! - `src/lib/fcitx/instance.cpp` `Instance::inputMethod(ic)`：
//!   `inputState->isActive()==false` 时用**组内第一个 IM**——我们的
//!   profile 是 [keyboard-us, pinyin]，即直通引擎 keyboard-us；
//! - `src/lib/fcitx/globalconfig.cpp`：`[Behavior] ActiveByDefault`
//!   无显式默认值（=false），隔离 HOME 又没有 `config` 文件 →
//!   全新实例每个输入上下文都以"未激活"启动，拼音引擎根本没挂上。
//! - 与 dbus 无关：实机 fcitx5.log 在 `--disable=dbus` 下
//!   pinyin/pinyinhelper addon 均正常 Loaded（且加载耗时 4~5s = 词典
//!   真初始化）。
//!
//! 三个对照实验钉死结论：
//! - `real_fcitx5_repro_no_seed_config`：现状形态（无 config 种子）→
//!   复现"无汉字 commit + VK 直通字母"。
//! - `real_fcitx5_seed_config_active_by_default`：只补一个
//!   `[Behavior] ActiveByDefault=True` 种子 → 汉字 commit 经
//!   im2 → relay → ti3 回到编辑器。
//! - `real_fcitx5_private_dbus`：不禁 dbus + 私有 session 总线 + 种子
//!   → 行为与上一条一致（引擎不依赖总线；隔离靠 env 剥离/私有总线）。
//!
//! 跑法（本机）：
//! ```text
//! RUSTFLAGS="-L <linklibs>" cargo test --lib ime::tests_real -- --ignored \
//!     --test-threads=1 --nocapture
//! ```

#![cfg(test)]

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::{Client, Display};

use crate::seat::KeyboardAction;
use crate::{WLCClient, WLCState};

use super::tests::{
    clients::{EditorState, TestClient},
    editor_connect, server_setup, ServerClientData,
};

/// evdev 键码（handle_key 收 xkb 码 = evdev+8；nihao + 空格）。
const N: u32 = 49;
const I: u32 = 23;
const H: u32 = 35;
const A: u32 = 30;
const O: u32 = 32;
const SPACE: u32 = 57;

/// 隔离 profile：与实机 ime-nested/config/fcitx5/profile 同构
/// （组内 keyboard-us 在前、pinyin 第二、DefaultIM=pinyin）。
const SEED_PROFILE: &str = r#"[Groups/0]
# Group Name
Name=default
# Layout
Default Layout=us
# Default Input Method
DefaultIM=pinyin

[Groups/0/Items/0]
# Name
Name=keyboard-us
# Layout
Layout=

[Groups/0/Items/1]
# Name
Name=pinyin
# Layout
Layout=

[GroupOrder]
0=default
"#;

/// 修复种子的核心：GlobalConfig 的 INI 文件在
/// `$XDG_CONFIG_HOME/fcitx5/config`（instance.cpp reloadConfig：
/// `StandardPath::Type::PkgConfig` + "config"），`[Behavior]
/// ActiveByDefault=True` 让每个新 InputContext 以激活态启动 →
/// `Instance::inputMethod(ic)` 走 `group.defaultInputMethod()`=pinyin。
const SEED_CONFIG_ACTIVE: &str = "[Behavior]\nActiveByDefault=True\n";

struct Variant {
    tag: &'static str,
    fcitx_args: &'static [&'static str],
    seed_config: bool,
    /// true = 起私有 session bus 并导出 DBUS_SESSION_BUS_ADDRESS；
    /// false = 删除该变量（连不上任何总线）。
    private_dbus: bool,
}

/// 一次真 fcitx5 端到端运行的可断言结果。
struct RunResult {
    ti_events: Vec<String>,
    key_events: Vec<(u32, u8)>,
    im2_seen_bind: bool,
    im2_seen_grab: bool,
    log_dir: PathBuf,
}

fn run(variant: &Variant) -> RunResult {
    let base = std::env::temp_dir().join(format!("wlc-f5e2e-{}", variant.tag));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let runtime = base.join("runtime");
    let conf_dir = home.join("config/fcitx5");
    for d in [
        home.clone(),
        home.join("config"),
        conf_dir.clone(),
        home.join("data"),
        home.join("cache"),
        runtime.clone(),
    ] {
        std::fs::create_dir_all(&d).expect("create dir");
    }
    // XDG_RUNTIME_DIR 必须 0700，否则 libwayland 客户端会拒绝/警告。
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();

    std::fs::write(conf_dir.join("profile"), SEED_PROFILE).unwrap();
    if variant.seed_config {
        std::fs::write(conf_dir.join("config"), SEED_CONFIG_ACTIVE).unwrap();
    }

    // ── 合成器：真监听 socket（fcitx5 按 $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY 连接）──
    let (mut display, mut server) = server_setup();
    let socket_name = "wayland-ctest";
    let listener = UnixListener::bind(runtime.join(socket_name)).expect("bind wayland socket");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");

    // ── 编辑器 client（unix pair 直连，同 tests.rs）──
    let (es, ec) = UnixStream::pair().unwrap();
    let editor_client: Client = display
        .handle()
        .insert_client(es, Arc::new(ServerClientData))
        .unwrap();
    let mut editor: TestClient<EditorState> = editor_connect(ec, &mut display, &mut server);

    // ── 起 fcitx5 ──
    let log_path = base.join("fcitx5.log");
    let mut dbus_child: Option<Child> = None;
    let mut cmd = Command::new("fcitx5");
    cmd.args(variant.fcitx_args)
        // 实机 nested_daemon 不带 verbose；取证需要 waylandim/key_trace
        // 细节（注意：-v 是 --version，必须用 --verbose）。
        .arg("--verbose")
        .arg("default=3,waylandim=5,key_trace=5")
        .env("WAYLAND_DISPLAY", socket_name)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("WAYLAND_DEBUG", "client")
        .env("LANG", "C.UTF-8")
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS");
    if variant.private_dbus {
        // 私有 session 总线：绝不连宿主总线（隔离纪律）。
        let bus_addr = format!("unix:path={}", base.join("dbus-bus").display());
        let dbus = Command::new("dbus-daemon")
            .args(["--session", &format!("--address={bus_addr}"), "--nopidfile"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn private dbus-daemon");
        cmd.env("DBUS_SESSION_BUS_ADDRESS", &bus_addr);
        dbus_child = Some(dbus);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap();
    let log_err = log.try_clone().unwrap();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    let mut fcitx = cmd.spawn().expect("spawn fcitx5");

    let mut harness = Harness {
        display: &mut display,
        server: &mut server,
        editor: &mut editor,
        listener: &listener,
    };
    let mut result = RunResult {
        ti_events: Vec::new(),
        key_events: Vec::new(),
        im2_seen_bind: false,
        im2_seen_grab: false,
        log_dir: base.clone(),
    };

    {
        // 1) 等 im2 client bind（fcitx5 起完 waylandim 才会连上）。
        //    冷启动 pinyin 词典实测 ~6s，给 90s 上限。
        result.im2_seen_bind =
            harness.wait_for(|s| s.ime.im2.instance.is_some(), Duration::from_secs(90));
        if result.im2_seen_bind {
            // 2) 编辑器聚焦 + enable（ti3 会话开始 → relay 向 im2 发 activate）。
            let surface = harness.server_surface(&editor_client);
            {
                let mut ime = std::mem::take(&mut harness.server.ime);
                ime.set_focus(&surface, harness.server);
                harness.server.ime = ime;
                harness.server.seat.keyboard_focus(surface);
            }
            let ti = harness.editor.state.ti.as_ref().unwrap().clone();
            ti.enable();
            ti.commit();
            harness.pump_burst(Duration::from_millis(400));

            // 3) 等 grab（fcitx5 在收到 activate 对应 done 的回调里 grabKeyboard）。
            result.im2_seen_grab =
                harness.wait_for(|s| s.ime.im2.grab.is_some(), Duration::from_secs(15));
            if result.im2_seen_grab {
                // 4) 喂键：nihao + space（libpinyin 首选词「你好」）。
                for k in [N, I, H, A, O, SPACE] {
                    harness.type_evdev(k, Duration::from_millis(180));
                }
                // 5) 等 commit 落到编辑器 ti3（最多 10s）。
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_secs(10) {
                    harness.pump();
                    if harness
                        .editor
                        .state
                        .ti_events
                        .iter()
                        .any(|e| e.starts_with("commit(") && e.chars().any(|c| !c.is_ascii()))
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    // ── 收尾：取事件 + 杀进程 ──
    harness.pump_burst(Duration::from_millis(300));
    result.ti_events = harness.editor.state.ti_events.clone();
    result.key_events = harness.editor.state.key_events.clone();
    drop(harness);
    let _ = fcitx.kill();
    let _ = fcitx.wait();
    if let Some(mut d) = dbus_child {
        let _ = d.kill();
        let _ = d.wait();
    }
    result
}

/// 事件泵：手动驱动合成器一轮（accept 新连接 → dispatch → flush → editor
/// dispatch）。没有 calloop——真 fcitx5 的 socket 由本测试线程轮询。
struct Harness<'a> {
    display: &'a mut Display<WLCState>,
    server: &'a mut WLCState,
    editor: &'a mut TestClient<EditorState>,
    listener: &'a UnixListener,
}

impl Harness<'_> {
    fn pump(&mut self) {
        // 编辑器 client 的出站请求（enable/commit）缓冲在队列里，
        // 必须由本线程 flush 才会到达合成器（同 tests.rs::drive）。
        self.editor.flush();
        // 外部连入的 client（真 fcitx5）必须按生产路径挂 WLCClient
        // （wl_compositor 请求要求该 data 在场，lib.rs:208）。
        while let Ok((stream, _)) = self.listener.accept() {
            if self
                .display
                .handle()
                .insert_client(stream, Arc::new(WLCClient::new()))
                .is_err()
            {
                continue;
            }
        }
        let _ = self.display.dispatch_clients(self.server);
        let _ = self.display.flush_clients();
        self.editor.dispatch_pending();
    }

    fn pump_burst(&mut self, dur: Duration) {
        let t0 = Instant::now();
        while t0.elapsed() < dur {
            self.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for(&mut self, cond: impl Fn(&WLCState) -> bool, timeout: Duration) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            self.pump();
            if cond(self.server) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn server_surface(&self, editor_client: &Client) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        use wayland_client::Proxy;
        let proto_id = self.editor.state.surface.as_ref().unwrap().id().protocol_id();
        editor_client
            .object_from_protocol_id::<
                smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
            >(&self.display.handle(), proto_id)
            .expect("editor surface visible on server side")
    }

    /// press+release 一个 evdev 键，中间/之后各泵 pause。
    fn type_evdev(&mut self, evdev: u32, pause: Duration) {
        let mods = self.server.seat.modifiers_tuple();
        let xkb = evdev + 8;
        self.server.ime.handle_key(xkb, KeyboardAction::Press, mods);
        self.pump_burst(pause);
        self.server.ime.handle_key(xkb, KeyboardAction::Release, mods);
        self.pump_burst(pause);
    }
}

fn cjk_commits(events: &[String]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.starts_with("commit(") && e.chars().skip(7).any(|c| !c.is_ascii()))
        .cloned()
        .collect()
}

fn preedit_count(events: &[String]) -> usize {
    events.iter().filter(|e| e.starts_with("preedit(")).count()
}

fn dump_diags(r: &RunResult, tag: &str) {
    eprintln!(
        "=== [{tag}] im2 bind={} grab={}",
        r.im2_seen_bind, r.im2_seen_grab
    );
    eprintln!("=== [{tag}] editor ti3 events: {:?}", r.ti_events);
    eprintln!("=== [{tag}] editor wl_keyboard keys: {:?}", r.key_events);
    let p = r.log_dir.join("fcitx5.log");
    match std::fs::read_to_string(&p) {
        Ok(s) => {
            let lines: Vec<&str> = s.lines().collect();
            eprintln!("=== [{tag}] fcitx5.log 关键行（{} 行中的 fcitx 日志）:", lines.len());
            for l in lines.iter().filter(|l| {
                (l.starts_with('I') || l.starts_with('D') || l.starts_with('E') || l.starts_with('W'))
                    && (l.contains("fcitx")
                        || l.contains("ACTIVATE")
                        || l.contains("DONE")
                        || l.contains("grab")
                        || l.contains("Grab")
                        || l.contains("key")
                        || l.contains("Key")
                        || l.contains("pinyin")
                        || l.contains("keyboard")
                        || l.contains("imeapi")
                        || l.contains("waylandim"))
            }) {
                eprintln!("    {l}");
            }
        }
        Err(e) => eprintln!("=== [{tag}] 读 {} 失败: {e}", p.display()),
    }
}

/// 复现实验：现状 daemon 形态（--disable=dbus、无 config 种子、无 DBUS）。
/// 预期（源码分析）：无 preedit、无汉字 commit，字母以 VK 直通落进编辑器。
#[test]
#[ignore = "真 fcitx5 端到端取证（本机跑）：复现 sync6 症状"]
fn real_fcitx5_repro_no_seed_config() {
    let r = run(&Variant {
        tag: "repro",
        fcitx_args: &["-D", "--disable=dbus"],
        seed_config: false,
        private_dbus: false,
    });
    dump_diags(&r, "repro");
    assert!(r.im2_seen_bind, "fcitx5 应已 bind im2（框架层正常）");
    assert!(r.im2_seen_grab, "fcitx5 应已 grab 键盘（键送达引擎）");
    assert!(
        cjk_commits(&r.ti_events).is_empty(),
        "现状形态应复现『汉字从未出现』；实际 ti3: {:?}",
        r.ti_events
    );
    // 引擎未消费 → VK 直通：n 键以 wl_keyboard.key（xkb 码）到达编辑器。
    assert!(
        r.key_events.contains(&(N + 8, 1u8)),
        "复现场景应看到 n 键 VK 直通进编辑器；实际键流: {:?}",
        r.key_events
    );
}

/// 修复实验：仅多一个 `[Behavior] ActiveByDefault=True` 种子文件，其余同
/// 实机（--disable=dbus、无 DBUS 总线）。预期：preedit 出现、空格选词后
/// 汉字 commit 经 im2 → relay → ti3 回到编辑器。
#[test]
#[ignore = "真 fcitx5 端到端验收（本机跑）：ActiveByDefault 种子后出 commit"]
fn real_fcitx5_seed_config_active_by_default() {
    let r = run(&Variant {
        tag: "seed",
        fcitx_args: &["-D", "--disable=dbus"],
        seed_config: true,
        private_dbus: false,
    });
    dump_diags(&r, "seed");
    assert!(r.im2_seen_bind, "fcitx5 应已 bind im2");
    assert!(r.im2_seen_grab, "fcitx5 应已 grab 键盘");
    assert!(
        preedit_count(&r.ti_events) > 0,
        "ActiveByDefault=True 后组合期间应有 preedit；ti3 事件: {:?}",
        r.ti_events
    );
    assert!(
        !cjk_commits(&r.ti_events).is_empty(),
        "ActiveByDefault=True 后应出现汉字 commit；ti3 事件: {:?}",
        r.ti_events
    );
}

/// 生产路径端到端：完全走 `ImeState::nested_tick`（decide 状态机 →
/// seed_fcitx5_home 种子 → build_spawn_plan（FCITX5_ARGS + 隔离 env）→
/// spawn_daemon 双 fork + pidfile）→ 真 fcitx5 bind → grab → 拼音 →
/// 汉字 commit 回编辑器 ti3。与 `seed` 手工实验的区别：spawn 的每一个
/// 参数/env 都来自生产代码——这条绿了才算修复闭环。
///
/// 注意：监听 socket 必须建在**宿主 XDG_RUNTIME_DIR**（build_spawn_plan
/// 沿用继承值，这正是游戏真实行为；socket 名带 pid 防冲突，结束删文件）。
#[test]
#[ignore = "真 fcitx5 端到端验收（本机跑）：nested_tick 生产链路"]
fn real_fcitx5_production_spawn_path() {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("需要 XDG_RUNTIME_DIR（同游戏运行环境）");
    let runtime = PathBuf::from(runtime);
    assert!(
        runtime
            .metadata()
            .map(|m| m.permissions().mode() & 0o777 == 0o700)
            .unwrap_or(false),
        "XDG_RUNTIME_DIR={runtime:?} 必须 0700（libwayland 要求）"
    );

    let base = std::env::temp_dir().join(format!(
        "wlc-f5e2e-prod-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let socket_name = format!("wayland-wc-ime-{}", std::process::id());
    let sock_path = runtime.join(&socket_name);
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).expect("bind wayland socket");
    listener.set_nonblocking(true).unwrap();

    let (mut display, mut server) = server_setup();
    server.socket = OsString::from(&socket_name);
    let (es, ec) = UnixStream::pair().unwrap();
    let editor_client: Client = display
        .handle()
        .insert_client(es, Arc::new(ServerClientData))
        .unwrap();
    let mut editor: TestClient<EditorState> = editor_connect(ec, &mut display, &mut server);

    // 生产控制器：mode=on + 隔离 HOME（全新空目录 → 种子必须自己写出
    // profile/config，顺带验证"用户没配过 profile"的首启场景）。
    server.ime.nested_set_mode("on");
    server.ime.nested_set_home(&home);

    let mut harness = Harness {
        display: &mut display,
        server: &mut server,
        editor: &mut editor,
        listener: &listener,
    };

    // 1) 驱动 nested_tick 直到 daemon bind im2（tick 内含 decide/spawn/
    //    seed；Active 由后续 tick 依 im2_bound 判定）。
    let mut bound = false;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(90) {
        harness.pump();
        harness
            .server
            .ime
            .nested_tick(Instant::now(), std::ffi::OsStr::new(&socket_name));
        if harness.server.ime.im2.instance.is_some() {
            bound = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // bind 后再来一帧，让 phase 落 Active（decide 依 im2_bound 判定）。
    harness
        .server
        .ime
        .nested_tick(Instant::now(), std::ffi::OsStr::new(&socket_name));
    let phase_after_bind = format!("{:?}", harness.server.ime.nested.phase());
    let seed_profile_ok = home.join("config/fcitx5/profile").exists();
    let seed_config_ok = home.join("config/fcitx5/config").is_file();

    let mut ti3 = Vec::new();
    let mut grabbed = false;
    if bound {
        let surface = harness.server_surface(&editor_client);
        {
            let mut ime = std::mem::take(&mut harness.server.ime);
            ime.set_focus(&surface, harness.server);
            harness.server.ime = ime;
            harness.server.seat.keyboard_focus(surface);
        }
        let ti = harness.editor.state.ti.as_ref().unwrap().clone();
        ti.enable();
        ti.commit();
        harness.pump_burst(Duration::from_millis(400));
        grabbed = harness.wait_for(|s| s.ime.im2.grab.is_some(), Duration::from_secs(15));
        if grabbed {
            for k in [N, I, H, A, O, SPACE] {
                harness.type_evdev(k, Duration::from_millis(180));
            }
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(10) {
                harness.pump();
                if harness
                    .editor
                    .state
                    .ti_events
                    .iter()
                    .any(|e| e.starts_with("commit(") && e.chars().any(|c| !c.is_ascii()))
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        harness.pump_burst(Duration::from_millis(200));
        ti3 = harness.editor.state.ti_events.clone();
    }
    drop(harness);

    // 收尾：按 pidfile 杀 daemon（生产 spawn 是双 fork 的孤儿进程）。
    if let Some(pid) = server
        .ime
        .nested
        .pid()
        .or_else(|| {
            std::fs::read_to_string(home.join("fcitx5.pid"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
        })
    {
        super::nested_daemon::kill_pid(pid);
    }
    let _ = std::fs::remove_file(&sock_path);

    let commits = cjk_commits(&ti3);
    let r = RunResult {
        ti_events: ti3.clone(),
        key_events: Vec::new(),
        im2_seen_bind: bound,
        im2_seen_grab: grabbed,
        // 生产 spawn_daemon 的日志落点：plan.log_file = home/fcitx5.log。
        log_dir: home.clone(),
    };
    dump_diags(&r, "prod");
    eprintln!("=== [prod] base dir kept for forensics: {}", base.display());

    assert!(bound, "生产 spawn 链路应拉起 fcitx5 并 bind im2");
    assert_eq!(phase_after_bind, "Active", "nested phase 应为 Active");
    assert!(seed_profile_ok, "种子应自动写出 profile（空 HOME 首启）");
    assert!(seed_config_ok, "种子应自动写出 config（ActiveByDefault=True）");
    assert!(grabbed, "fcitx5 应 grab 键盘");
    assert!(
        !commits.is_empty(),
        "生产链路端到端应出汉字 commit；ti3 事件: {ti3:?}"
    );
}

/// 对照实验：保留 dbus addon（不禁）+ 私有 session 总线 + 同样种子。
/// 预期与 seed 一致——证明 pinyin 不依赖宿主总线；隔离不靠 --disable=dbus
/// 也成立（env 剥离/私有总线都不触碰宿主 fcitx5）。
#[test]
#[ignore = "真 fcitx5 端到端对照（本机跑）：私有 dbus 总线行为一致性"]
fn real_fcitx5_private_dbus() {
    let r = run(&Variant {
        tag: "privdbus",
        fcitx_args: &["-D"],
        seed_config: true,
        private_dbus: true,
    });
    dump_diags(&r, "privdbus");
    assert!(r.im2_seen_bind, "fcitx5 应已 bind im2");
    assert!(r.im2_seen_grab, "fcitx5 应已 grab 键盘");
    assert!(
        !cjk_commits(&r.ti_events).is_empty(),
        "私有总线形态也应出汉字 commit；ti3 事件: {:?}",
        r.ti_events
    );
}
