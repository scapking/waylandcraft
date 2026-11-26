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
    /// true = 在 SPACE（选词）键事件**之后**再排入一次应用状态变更 done——
    /// 复现 sync7 实机 serial 竞态形态（fcitx5 回填的 serial 落后服务端
    /// wire 已发计数一拍）。见 `Harness::type_evdev_with_state_race`。
    drift_before_commit: bool,
}

/// sync7 回归：drift 注入的状态文本序号（保证每次 push 都是 meaningful 变化）。
static DRIFT_NONCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 一次真 fcitx5 端到端运行的可断言结果。
struct RunResult {
    ti_events: Vec<String>,
    key_events: Vec<(u32, u8)>,
    im2_seen_bind: bool,
    im2_seen_grab: bool,
    /// 修复后语义：commit serial 与 wire done 计数不符、但文本仍照常交付的
    /// 批次数（ImeState::serial_mismatch_count）。drift 用例断言其 ≥1，
    /// 证明「丢字路径」确实被触发过而不是绕过了。
    serial_mismatch_seen: u32,
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
        serial_mismatch_seen: 0,
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
                for k in [N, I, H, A, O] {
                    harness.type_evdev(k, Duration::from_millis(180));
                }
                if variant.drift_before_commit {
                    // sync7 故障形态注入：SPACE 键事件先到 fcitx5，随后才排入
                    // 一次状态 done——选词 commit 回填的 serial 落后 wire 计数。
                    harness.type_evdev_with_state_race(SPACE, Duration::from_millis(250));
                } else {
                    harness.type_evdev(SPACE, Duration::from_millis(180));
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
    result.serial_mismatch_seen = server.ime.serial_mismatch_count();
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

    /// sync7 实机故障形态注入（真 fcitx5 serial 竞态的确定化）。
    /// 取证注记（首版实现失败，WAYLAND_DEBUG 线路日志 /tmp/wlc-f5e2e-drift/
    /// fcitx5.log）：fcitx5 在收到 key 事件的 dispatch 内 ~0.4ms 就把
    /// `commit(serial_)` 写上了 socket（keyCallback → commitStringDelegate
    /// → `ic_->commit(serial_)`，见 waylandimserverv2.h:105-106）；但若
    /// 注入依赖「同一轮 dispatch_clients 里编辑器先被处理」，客户端处理
    /// 顺序并无该保证——commit 可能先被读取，wire 计数尚未 +1，serial
    /// 恰好匹配，mismatch 复现不了。确定化做法改为利用「commit 在途
    /// 未读」窗口，不依赖事件循环内部顺序：
    /// 1. SPACE press 发出并 flush（fcitx5 是独立进程，自己醒来处理）；
    /// 2. 睡 30ms：fcitx5 选词并把陈旧 serial 的 commit 上路，服务端故意
    ///    不读；
    /// 3. 服务端推进 wire 状态：直接下发一次 PushState（execute_ime_commands
    ///    → send_done，计数 +1 并 flush）——那条在途 commit 从此落后计数
    ///    一拍，正是实机日志「commit(serial=8…65) DROPPED」的形态；
    /// 4. 再 pump：服务端读入陈旧 serial 的 commit → 走 mismatch 分支，
    ///    汉字文本必须照常上屏（回归断言见调用方用例）。
    fn type_evdev_with_state_race(&mut self, evdev: u32, pause: Duration) {
        use std::sync::atomic::Ordering;
        let mods = self.server.seat.modifiers_tuple();
        let xkb = evdev + 8;
        // 1) key 事件先落地。
        self.server.ime.handle_key(xkb, KeyboardAction::Press, mods);
        let _ = self.display.flush_clients();
        // 2) 让陈旧 serial 的 commit 上路（实测 ~0.4ms，30ms 富余）。
        std::thread::sleep(Duration::from_millis(30));
        // 3) wire 计数此刻才 +1：done 排在 fcitx5 已发出的 commit 之后送达。
        let nonce = DRIFT_NONCE.fetch_add(1, Ordering::Relaxed);
        let st = super::AppState {
            surrounding_text: format!("drift{nonce}"),
            surrounding_cursor: 1,
            surrounding_anchor: 1,
            ..Default::default()
        };
        self.server
            .ime
            .execute_ime_commands(vec![super::ImeCommand::PushState(st)]);
        let _ = self.display.flush_clients();
        // 4) 服务端现在才读入在途 commit（serial 已落后 → mismatch 分支）。
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
        drift_before_commit: false,
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
        drift_before_commit: false,
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
        serial_mismatch_seen: server.ime.serial_mismatch_count(),
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
        drift_before_commit: false,
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

/// sync7 生产事故回归（真 fcitx5）：在选词键（SPACE）事件之后注入一次
/// 应用状态 done——fcitx5 在 key dispatch 回调里同步发出 commit，其回填
/// serial 落后服务端 wire 计数一拍（实机日志「commit(serial=8…65)
/// DROPPED」的确定化复现）。旧代码此批次被整批丢弃、汉字永不上屏；
/// 修复后必须：mismatch 被真实触发（serial_mismatch_seen ≥ 1，排除用例
/// 退化为普通提交）且真 fcitx5 汉字照常 commit 进编辑器 ti3。
#[test]
#[ignore = "真 fcitx5 端到端回归（本机跑）：serial 不符文本仍上屏"]
fn real_fcitx5_serial_drift_still_commits() {
    let r = run(&Variant {
        tag: "drift",
        fcitx_args: &["-D", "--disable=dbus"],
        seed_config: true,
        private_dbus: false,
        drift_before_commit: true,
    });
    dump_diags(&r, "drift");
    assert!(r.im2_seen_bind, "fcitx5 应已 bind im2");
    assert!(r.im2_seen_grab, "fcitx5 应已 grab 键盘");
    assert!(
        r.serial_mismatch_seen >= 1,
        "drift 注入应真实复现 serial 不符（否则用例退化为普通提交，证明不了修复）；seen={}",
        r.serial_mismatch_seen
    );
    assert!(
        !cjk_commits(&r.ti_events).is_empty(),
        "serial 不符时真 fcitx5 汉字也必须上屏（sync7 旧逻辑此处全灭）；ti3: {:?}",
        r.ti_events
    );
}

// ════════════════ 种子 v2：全部已装引擎 + Ctrl+Shift+B 切换 ════════════════

/// 按修饰键顺序发一个和弦（evdev 码 + press/release 序列），完全复刻
/// 生产链路：keyboardUpdate 先行更新 seat xkb 态 → keyboardInput 读
/// modifiers_tuple 喂 handle_key（KeyboardHandlerMixin 的次序纪律）。
fn type_chord(h: &mut Harness, seq: &[(u32, bool)]) {
    for (evdev, press) in seq {
        let xkb = evdev + 8;
        h.server.seat.keyboard_update_xkb(xkb, *press);
        let mods = h.server.seat.modifiers_tuple();
        h.server.ime.handle_key(
            xkb,
            if *press { KeyboardAction::Press } else { KeyboardAction::Release },
            mods,
        );
        h.pump_burst(Duration::from_millis(80));
    }
}

/// 等编辑器 ti3 出现新的非 ASCII commit（自 `since` 索引起），返回文本。
fn wait_new_cjk(h: &mut Harness, since: usize, timeout: Duration) -> Option<String> {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        h.pump();
        if let Some(ev) = h
            .editor
            .state
            .ti_events
            .iter()
            .skip(since)
            .find(|e| e.starts_with("commit(") && e.chars().skip(7).any(|c| !c.is_ascii()))
        {
            return Some(ev.clone());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// 种子 v2 端到端（真机）：生产种子函数写全新隔离 HOME → 真 fcitx5 拉起
/// → 默认引擎拼音出汉字 commit → **连按 Ctrl+Shift+B 枚举走到 wbpy（五笔
/// 拼音混合）**（种子补的 EnumerateForwardKeys——5.1.12 出厂该键为空，见
/// seed_fcitx5_home_v2 文档）→ wbpy 用同一段拼音编码再次 commit → SIGTERM
/// 后 fcitx5 退出保存把切换结果写回 profile，DefaultIM=wbpy 落盘为硬证。
///
/// 即“种子 v2 后真 fcitx5 至少两个引擎可切换/可 commit”的验收用例。
/// 前提（本机实况）：fcitx5-chinese-addons 已装且 pinyin/wbpy 均 conf
/// Enable=true（注意 cangjie.conf 出厂 Enable=False——fcitx5 语义下不算
/// 已装引擎，发现逻辑如实排除）；无宿主 profile（镜像路径有独立单测）。
#[test]
#[ignore = "真 fcitx5 端到端验收（本机跑）：种子 v2 全引擎 + Ctrl+Shift+B 切换"]
fn real_fcitx5_seed_v2_all_engines_switchable() {
    use super::nested_daemon::{
        parse_profile_groups, seed_fcitx5_home_v2, SeedPaths, SEEDED_MARK_V2,
    };
    use crate::ime::nested_daemon::kill_pid;

    let base = std::env::temp_dir().join(format!("wlc-f5e2e-v2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let runtime = base.join("runtime");
    for d in [&home, &home.join("config"), &runtime, &home.join("data"), &home.join("cache")] {
        std::fs::create_dir_all(d).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();

    // 1) 生产种子函数（游戏进程 env = 真实宿主 env）种全新隔离 HOME。
    let out = seed_fcitx5_home_v2(&home, &SeedPaths::from_process_env());
    eprintln!("=== [v2] 种子诊断行:");
    for n in &out.notes {
        eprintln!("    {n}");
    }
    assert!(out.engine_count >= 2, "种子 v2 应发现 ≥2 引擎: {:?}", out.notes);
    assert!(
        out.engines.iter().any(|e| e == "pinyin") && out.engines.iter().any(|e| e == "wbpy"),
        "本机应有 pinyin+wbpy（fcitx5-chinese-addons 默认启用）；实得: {:?}",
        out.engines
    );
    let profile_path = home.join("config/fcitx5/profile");
    let seeded = std::fs::read_to_string(&profile_path).expect("v2 profile 必须已写出");
    assert!(seeded.contains(SEEDED_MARK_V2), "种子 profile 必须带所有权标记");
    let groups = parse_profile_groups(&seeded);
    // keyboard-us 在首、pinyin 第二（枚举循环可预期起点）；其余升序。
    assert_eq!(groups[0].items[0], "keyboard-us");
    assert_eq!(groups[0].items[1], "pinyin");
    assert_eq!(groups[0].default_im, "pinyin");
    let switch_to = "wbpy";
    let target_idx = groups[0]
        .items
        .iter()
        .position(|i| i == switch_to)
        .expect("wbpy 必须在 v2 条目表里");
    let presses = target_idx - 1; // pinyin(idx1) → 目标引擎的枚举步数
    assert!(std::fs::read_to_string(home.join("config/fcitx5/config"))
        .unwrap()
        .contains("[Hotkey/EnumerateForwardKeys]"));

    // 2) 合成器 + 编辑器（真监听 socket，同既有真机测试族）。
    let socket_name = format!("wayland-wc-v2-{}", std::process::id());
    let sock_path = runtime.join(&socket_name);
    let listener = UnixListener::bind(&sock_path).expect("bind wayland socket");
    listener.set_nonblocking(true).unwrap();
    let (mut display, mut server) = server_setup();
    let (es, ec) = UnixStream::pair().unwrap();
    let editor_client: Client = display
        .handle()
        .insert_client(es, Arc::new(ServerClientData))
        .unwrap();
    let mut editor: TestClient<EditorState> = editor_connect(ec, &mut display, &mut server);

    // 3) 起真 fcitx5（隔离 env = build_spawn_plan 口径；HOME 下已是 v2 种子）。
    let mut cmd = Command::new("fcitx5");
    cmd.args(["-D", "--disable=dbus"])
        .arg("--verbose")
        .arg("default=3,waylandim=5,key_trace=5")
        .env("WAYLAND_DISPLAY", &socket_name)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("LANG", "C.UTF-8")
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(base.join("fcitx5.log"))
        .unwrap();
    let log_err = log.try_clone().unwrap();
    cmd.stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(log_err));
    let mut fcitx = cmd.spawn().expect("spawn fcitx5");

    let mut harness = Harness {
        display: &mut display,
        server: &mut server,
        editor: &mut editor,
        listener: &listener,
    };

    // 4) bind → 聚焦 enable → grab。
    let bound = harness.wait_for(|s| s.ime.im2.instance.is_some(), Duration::from_secs(90));
    assert!(bound, "fcitx5 应 bind im2（v2 种子不破坏 waylandim 链路）");
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
    let grabbed = harness.wait_for(|s| s.ime.im2.grab.is_some(), Duration::from_secs(15));
    assert!(grabbed, "fcitx5 应 grab 键盘");

    // 5) 默认引擎（拼音）：nihao + 空格 → CJK commit。
    for k in [N, I, H, A, O, SPACE] {
        harness.type_evdev(k, Duration::from_millis(180));
    }
    let commit_a = wait_new_cjk(&mut harness, 0, Duration::from_secs(10));
    let snapshot = harness.editor.state.ti_events.len();
    assert!(
        commit_a.is_some(),
        "拼音引擎应出汉字 commit；ti3: {:?}",
        harness.editor.state.ti_events
    );

    // 6) Ctrl+Shift+B 和弦 × presses 次（每次完整 按下→释放 序列）
    //    → 沿枚举序列走到目标引擎（每个中间引擎只收到被热键消费的
    //    B 和弦，不会污染组字）。
    for _ in 0..presses {
        type_chord(
            &mut harness,
            &[(29, true), (42, true), (48, true), (48, false), (42, false), (29, false)],
        );
    }
    harness.pump_burst(Duration::from_millis(1000));

    // 7) 切后引擎必须可 commit：wbpy 是五笔/拼音混合引擎，同一段拼音
    //    编码 nihao+空格 → 新非 ASCII commit。
    for k in [N, I, H, A, O, SPACE] {
        harness.type_evdev(k, Duration::from_millis(220));
    }
    let commit_b = wait_new_cjk(&mut harness, snapshot, Duration::from_secs(12));
    let events = harness.editor.state.ti_events.clone();
    harness.pump_burst(Duration::from_millis(300));
    let ti_final = harness.editor.state.ti_events.clone();
    drop(harness);

    // 8) SIGTERM → fcitx5 退出保存（instance.cpp exitEvent → save）把
    //    枚举结果写回 profile：DefaultIM=切换目标引擎 是“切换真实发生”的硬证。
    kill_pid(fcitx.id());
    let mut persisted = None;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(6) {
        if let Ok(text) = std::fs::read_to_string(&profile_path) {
            let g = parse_profile_groups(&text);
            if g.first().is_some_and(|x| x.default_im == switch_to) {
                persisted = Some(text);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fcitx.kill();
    let _ = fcitx.wait();
    let _ = std::fs::remove_file(&sock_path);

    let r = RunResult {
        ti_events: ti_final.clone(),
        key_events: Vec::new(),
        im2_seen_bind: bound,
        im2_seen_grab: grabbed,
        log_dir: base.clone(),
        // v2 用例不校验 serial 漂移（漂移回归由 real_fcitx5_serial_drift_still_commits 负责）
        serial_mismatch_seen: 0,
    };
    dump_diags(&r, "seedv2");
    eprintln!("=== [v2] commit A（拼音）: {commit_a:?}\n=== [v2] commit B（切后）: {commit_b:?}");
    eprintln!("=== [v2] 引擎切换前的中间 ti3 事件（快照后）: {:?}", &events[snapshot.min(events.len())..]);

    assert!(
        commit_b.is_some(),
        "Ctrl+Shift+B 切换后新引擎（仓颉）必须可 commit；切换后 ti3: {events:?}"
    );
    let persisted = persisted.unwrap_or_else(|| {
        panic!(
            "fcitx5 退出应把枚举切换结果（DefaultIM={switch_to}）写回 profile；\
             未观察到=枚举热键没生效或退出保存未发生（见 fcitx5.log）。\
             当前 profile: {:?}",
            std::fs::read_to_string(&profile_path)
        )
    });
    // 落盘文件仍应保持全引擎条目（fcitx5 重写只动 DefaultIM/丢标记——
    // 我们的所有权规则据此在下次启动无缝升级）。
    let pg = parse_profile_groups(&persisted);
    assert!(pg[0].items.iter().any(|i| i == "pinyin") && pg[0].items.iter().any(|i| i == switch_to));
    assert_eq!(pg[0].items.len(), groups[0].items.len(), "重写后条目数必须不变");
    eprintln!("=== [v2] base dir kept for forensics: {}", base.display());
}

// ═══════════════ sync15 性能 bench（先测量后修的标尺）═══════════════

/// Java tick 的单帧仿真：`WaylandCraft.update()` + `focusSurface` +
/// `ImeCandidateOverlay.tick` 三件套按生产节奏走一遍——
/// 1. pump（dispatch+flush，等价 update 的 event_loop.dispatch）；
/// 2. 每帧 focusSurface(toplevel)：生产 Java 每 tick 调 bridge.focusSurface
///    → keyboard_focus → ime.set_focus → popup_touch（sync14 病灶的
///    沉默每帧 epoch++ 源头，本 bench 必须复刻它，测量才有意义）；
/// 3. take_popup_snapshot + encode_popup_snapshot：与 bridge::ime_popup_frame
///    生产函数逐字相同（交付耗时/字节记入 metrics）。
///
/// 返回本帧是否发生了 JNI 交付（Some）。
fn java_tick(
    h: &mut Harness,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> bool {
    h.pump();
    {
        let mut ime = std::mem::take(&mut h.server.ime);
        ime.set_focus(surface, h.server);
        h.server.ime = ime;
    }
    let t_del = Instant::now();
    let delivered = if let Some(snap) = h.server.ime.take_popup_snapshot() {
        let data = crate::bridge::encode_popup_snapshot(&snap);
        let dur = t_del.elapsed();
        let m = &mut h.server.ime.metrics;
        m.popup_delivery.record(dur);
        m.popup_delivered_bytes += data.len() as u64;
        true
    } else {
        false
    };
    delivered
}

/// 以 ~16ms 帧节拍把 java_tick 跑 `dur` 时长（等价一轮 Java 渲染循环窗）。
fn java_burst(
    h: &mut Harness,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    dur: Duration,
) {
    let t0 = Instant::now();
    while t0.elapsed() < dur {
        java_tick(h, surface);
        std::thread::sleep(Duration::from_millis(6));
    }
}

/// sync15 性能 bench（#[ignore]，本机跑）：**脚本化 50 键打字 + 候选窗
/// 更新**，用与生产逐字相同的调用形状复刻 sync14 用户实测病灶
/// （"输入法的时候特别卡，多次差点卡崩"）：
///
/// - 50 次 handle_key（press/release 各 50，key_input 直方图入册）；
/// - 键间按 Java tick 节奏泵 + 每帧 set_focus（病灶驱动源）；
/// - 统计 = `ImeState.metrics` 全量 + 桥级 IME_LOG_IO 快照，
///   eprintln `=== [bench50] ...` 行，修复前/后各跑一次取对比数字。
///
/// 断言刻意宽松（场景成立性门槛，不锁性能绝对值——性能改善以打印
/// 数字为准，写进交付报告）：
/// - 50 键全部入册（key_input n=100）；
/// - 打字期间候选窗至少真实交付过 3 帧（面板确实在更新）；
/// - 汉字 commit 落地（链路功能没被性能改动破坏）。
#[test]
#[ignore = "真 fcitx5 latency bench（本机跑）：50 键脚本化打字 + 候选交付经济学"]
fn real_fcitx5_50_key_latency_bench() {
    use crate::ime::metrics::PopupDeliveryCounts;

    let base = std::env::temp_dir().join(format!("wlc-f5bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let runtime = base.join("runtime");
    let conf_dir = home.join("config/fcitx5");
    for d in [&home, &home.join("config"), &conf_dir, &home.join("data"), &home.join("cache"), &runtime] {
        std::fs::create_dir_all(d).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(conf_dir.join("profile"), SEED_PROFILE).unwrap();
    std::fs::write(conf_dir.join("config"), SEED_CONFIG_ACTIVE).unwrap();

    let (mut display, mut server) = server_setup();
    let socket_name = format!("wayland-bench50-{}", std::process::id());
    let listener = UnixListener::bind(runtime.join(&socket_name)).expect("bind");
    listener.set_nonblocking(true).unwrap();

    let (es, ec) = UnixStream::pair().unwrap();
    let editor_client: Client = display
        .handle()
        .insert_client(es, Arc::new(ServerClientData))
        .unwrap();
    let mut editor: TestClient<EditorState> = editor_connect(ec, &mut display, &mut server);

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(base.join("fcitx5.log"))
        .unwrap();
    let log_err = log.try_clone().unwrap();
    let mut cmd = Command::new("fcitx5");
    cmd.args(["-D", "--disable=dbus"])
        .arg("--verbose")
        .arg("default=3,waylandim=5")
        .env("WAYLAND_DISPLAY", &socket_name)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("LANG", "C.UTF-8")
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS");
    cmd.stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(log_err));
    let mut fcitx = cmd.spawn().expect("spawn fcitx5");

    let mut h = Harness {
        display: &mut display,
        server: &mut server,
        editor: &mut editor,
        listener: &listener,
    };

    // 生产同款：Java 启动时 setImeLogFile 打开 .minecraft/waylandcraft-ime.log。
    // bench 里装一个真文件，log_io 的 write_ops/bytes 才是可对比的真实
    // 系统调用读数（无文件时只有 stderr，file ops 恒 0）。
    let ime_log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(base.join("ime.log"))
        .unwrap();
    crate::bridge::set_ime_log_file_for_test(ime_log_file);

    // ── 建链：bind → 聚焦 enable → grab（同生产会话启动形状）──
    let bound = h.wait_for(|s| s.ime.im2.instance.is_some(), Duration::from_secs(90));
    assert!(bound, "真 fcitx5 应 bind im2（bench 前置）");
    let surface = h.server_surface(&editor_client);
    {
        let mut ime = std::mem::take(&mut h.server.ime);
        ime.set_focus(&surface, h.server);
        h.server.ime = ime;
        h.server.seat.keyboard_focus(surface.clone());
    }
    let ti = h.editor.state.ti.as_ref().unwrap().clone();
    ti.enable();
    ti.commit();
    h.pump_burst(Duration::from_millis(400));
    let grabbed = h.wait_for(|s| s.ime.im2.grab.is_some(), Duration::from_secs(15));
    assert!(grabbed, "引擎应 grab 键盘（bench 需要 im2 主路径形态）");

    // ── 脚本化 50 键：8×(n i h a o space) + n i ──
    // 覆盖"逐键候选重绘 + 空格选词 commit"两类 popup 更新事件。
    let words: Vec<u32> = [N, I, H, A, O, SPACE]
        .into_iter()
        .cycle()
        .take(48)
        .chain([N, I])
        .collect();
    assert_eq!(words.len(), 50);
    let bench_t0 = Instant::now();
    let mut jni_deliveries = 0u64;
    for evdev in words {
        for pressed in [true, false] {
            let mods = h.server.seat.modifiers_tuple();
            let action = if pressed { KeyboardAction::Press } else { KeyboardAction::Release };
            // 与生产 keyboard_input 同形：handle_key 计时入 key_input 直方图
            //（grab 在场时 handle_key 即 bridge 的绝大部分本体）。
            let t0 = Instant::now();
            let _handled = h.server.ime.handle_key(evdev + 8, action, mods);
            h.server.ime.metrics.key_input.record(t0.elapsed());
            // 键后按 Java 帧节拍泵（press 120ms / release 60ms ≈ 用户
            // 150ms 键间隔的一半，保证候选重绘落在窗口内被数到）。
            let pause = if pressed { 120 } else { 60 };
            let t_end = Instant::now() + Duration::from_millis(pause);
            while Instant::now() < t_end {
                if java_tick(&mut h, &surface) {
                    jni_deliveries += 1;
                }
                std::thread::sleep(Duration::from_millis(6));
            }
        }
    }
    let bench_secs = bench_t0.elapsed().as_secs_f64();
    // 让最后一次 commit 落地。
    let commit_seen = wait_new_cjk(&mut h, 0, Duration::from_secs(6));
    java_burst(&mut h, &surface, Duration::from_millis(300));

    // ── 数字 dump（报告原料：修复前/后各一份）──
    let m = &h.server.ime.metrics;
    let popup = PopupDeliveryCounts {
        frames: h.server.ime.popup.frames_captured() as u64,
        deliveries: h.server.ime.popup.deliveries() as u64,
    };
    let commits = cjk_commits(&h.editor.state.ti_events);
    eprintln!("=== [bench50] 场景: 50 键(8×nihao+空格 + n i), 时长 {bench_secs:.1}s, Java tick=6ms 帧节拍+每帧 set_focus");
    eprintln!("=== [bench50] key_input   {}", m.key_input.format());
    eprintln!("=== [bench50] submit_wait {}", m.submit_sync_wait.format());
    eprintln!("=== [bench50] popup_deliv {}", m.popup_delivery.format());
    eprintln!(
        "=== [bench50] popup: frames={} deliveries={} dup_skipped={} delivered_bytes={}KB jni_tick_deliveries={}",
        popup.frames, popup.deliveries, m.popup_dup_skipped, m.popup_delivered_bytes / 1024, jni_deliveries
    );
    let (lines, bytes, ops, supp) = crate::bridge::IME_LOG_IO.snapshot();
    eprintln!("=== [bench50] log_io: lines={lines} bytes={}KB write_ops={ops} suppressed={supp}", bytes / 1024);
    eprintln!("=== [bench50] commits={commits:?} first_commit={commit_seen:?}");
    eprintln!("=== [bench50] ime_info 延迟段:\n{}", m.format_latency_section(popup));
    eprintln!("=== [bench50] base dir kept for forensics: {}", base.display());

    // ── 场景成立性断言（宽松；性能数字以 eprintln 为准）──
    assert_eq!(m.key_input.count(), 100, "50 键 press+release 全入册");
    assert!(
        popup.frames >= 3,
        "打字期间候选面板应重绘 ≥3 帧（场景成立性），实际 {}",
        popup.frames
    );
    assert!(
        !commits.is_empty(),
        "脚本化打字必须产生汉字 commit（链路功能完好），ti3: {:?}",
        h.editor.state.ti_events
    );

    let _ = fcitx.kill();
    let _ = fcitx.wait();
}
