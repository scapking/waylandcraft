//! 嵌套 IME 守护进程管理 —— 在我们的 WAYLAND_DISPLAY 上拉起独立 IME 实例。
//!
//! ## 为什么
//!
//! host dbus-ibus 路线的天花板已被实测钉死（IME_RESEARCH_CONCLUSIONS.md）：
//! 宿主引擎的 focus 状态归**宿主会话**管理（GNOME/mutter 隔离），我们提交的
//! 每个 ProcessKeyEvent 恒 consumed=false——中文永远打不出。本模块把 IME
//! 引擎的所有权夺回嵌套会话：
//!
//! ```text
//! decide() 状态机（每帧 tick，纯逻辑可单测）
//!   Unconfigured ──Java 注入 mode+home──┐
//!   NoBinary ←─ PATH 探测失败（fcitx5→ibus 顺序）
//!   Disabled   ←─ mode=off / 用户 /wl ime nested stop
//!   Starting   ── im2 client bind（ime_present）──> Active
//!      │ spawn+10s 未 bind / pid 消失
//!      ▼
//!   Failed(backoff 5s，连续 3 次封顶，如实上报后停)──手动 start 复位──> Starting
//! ```
//!
//! ## 隔离纪律（绝不污染宿主）
//!
//! - **环境**：`WAYLAND_DISPLAY`=我们的 socket、`HOME`/`XDG_CONFIG_HOME`/
//!   `XDG_DATA_HOME`/`XDG_CACHE_HOME`=gameDir 下的隔离目录、
//!   `XDG_RUNTIME_DIR` 沿用宿主（合成器 socket 文件在那），**删除**
//!   `DBUS_SESSION_BUS_ADDRESS`/`DISPLAY`（不抢宿主 session bus、不开 X）。
//! - **参数**：fcitx5 用 `-D --disable=dbus`（前台、禁 dbus addon——避免
//!   org.fcitx.Fcitx5 名字与宿主实例冲突）；ibus-daemon 用独立
//!   `--address=unix:path=…` 且禁 panel/config/emoji 子进程。
//! - **绝不传 `--replace`/`-r`**：那会踢掉用户的宿主输入法实例（单元测试
//!   `spawn_plan_never_replaces` 把这条纪律写死在断言里）。
//! - **进程管理**：双 fork + setsid（同 process.rs 惯例）脱离游戏会话；
//!   孙进程 pid 写 pidfile；stdout/stderr 重定向到隔离目录日志。
//!
//! ## 探活与回退
//!
//! - 探活主信号 = **im2 client bind**（`zwp_input_method_manager_v2` 的
//!   get_input_method 到达即 Active；比 pidfile 强——进程活着但连不上
//!   我们的 socket 没有意义）。
//! - pidfile 仅用于 Active 后的进程死亡检测与 stop 杀进程。
//! - daemon 缺席/挂掉/没连上 → phase 离开 Active，`nested_route_live()`
//!   变 false，按键路由自动回到 v0.13 的 host_bridge/raw（可选后备路径，
//!   本模块不碰）。失败/降级经 `status::note_error` 进 ⑥ error ring，
//!   全部决策行经 ime_log_write 带 ⑥ 时间戳写 ime.log。
//!
//! ## 已知边界（实测项，见模块底部 TODO）
//!
//! - 隔离 HOME 下 fcitx5 默认 profile 只有 keyboard-us；中文引擎（pinyin）
//!   需用户在隔离 profile 里启用（`fcitx5-configtool` 指向该 HOME，或把
//!   现有 profile 拷入）。逻辑与 wire 层已用模拟 client 全测，端到端行为
//!   由真机 fcitx5 实测确认。
//! - ibus-daemon 无 im2 客户端实现（实测其 waylandim 是 fcitx5 独有），
//!   预计表现为"进程在但 im2 永不 bind → Starting 超时 → Failed 回退"，
//!   如实上报，不做假成功。

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 配置模式（Java settings 键 `imeNested`，值 auto|on|off）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NestedMode {
    /// 默认：探测到可用二进制才拉 daemon，否则静默走 v0.13 后备路。
    #[default]
    Auto,
    /// 强制拉 daemon；失败如实进 Failed 状态（不回退 settings——用户显式意图）。
    On,
    /// 彻底禁用嵌套路：不 spawn、im2 客户端在场也不路由（保留 v0.13 行为）。
    Off,
}

impl NestedMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// daemon 二进制选择（优先级顺序由 `pick_daemon` 决定，勿改）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonKind {
    Fcitx5,
    Ibus,
}

impl DaemonKind {
    pub fn binary(self) -> &'static str {
        match self {
            Self::Fcitx5 => "fcitx5",
            Self::Ibus => "ibus-daemon",
        }
    }
    /// /proc/<pid>/comm 前缀（死亡检测；防 pid 复用误判成别的进程）。
    pub fn comm_prefix(self) -> &'static str {
        match self {
            Self::Fcitx5 => "fcitx5",
            Self::Ibus => "ibus-daemon",
        }
    }
}

// ── 常量（daemon 参数与节流；改这里前读模块头注释的隔离纪律）──

/// fcitx5：前台 + 禁 dbus addon。
/// TODO(实测): `--disable=dbus` 是否牺牲 waylandim 需要的 dbus 侧能力
/// （如 dbus-based module loading）由真机实测定；若引擎加载失败，等价
/// 方案是保留 dbus addon 但删 DBUS_SESSION_BUS_ADDRESS 环境（连不上总线，
/// addon 自己降级）——两条路都不触碰宿主实例。
pub const FCITX5_ARGS: &[&str] = &["-D", "--disable=dbus"];
/// 探活超时：spawn 后等多久没等到 im2 bind 算一次失败。
pub const BIND_TIMEOUT: Duration = Duration::from_secs(10);
/// 失败重试退避（与 ⑧ host_bridge 5s 节流同形）。
pub const RETRY_DELAY: Duration = Duration::from_secs(5);
/// 连续失败封顶：超过则停在 Failed（手动 start 复位），绝不 fork 炸弹。
pub const MAX_ATTEMPTS: u32 = 3;

/// 状态机相位。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NestedPhase {
    /// mode=off 或用户 `/wl ime nested stop`。
    Disabled,
    /// Java 尚未注入 mode+gameDir（配置未同步的启动窗口期）。初始相位。
    #[default]
    Unconfigured,
    /// PATH 里既没有 fcitx5 也没有 ibus-daemon。
    NoBinary,
    /// 已 spawn，等 im2 bind。
    Starting,
    /// im2 客户端在位（嵌套主路径在线）。
    Active,
    /// 连续失败封顶/进程死亡等，已回退 host_bridge/raw。原因如实携带。
    Failed(String),
}

/// 一次 spawn 的完整计划（纯数据——单测断言的就是它）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnPlan {
    pub program: &'static str,
    pub args: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub env_unset: Vec<&'static str>,
    pub log_file: PathBuf,
    pub pidfile: PathBuf,
}

/// `decide()` 产出的执行请求（decide 改好状态后交给调用方真正 fork——
/// 状态机因此可以在无进程/无文件系统的单测里跑完整转移图）。
#[derive(Debug)]
pub struct SpawnRequest {
    pub kind: DaemonKind,
    pub plan: SpawnPlan,
}

/// 嵌套 daemon 控制器。挂在 `ImeState.nested` 上，由 `ImeState::nested_tick`
/// 每帧驱动。
#[derive(Default)]
pub struct NestedIme {
    mode: NestedMode,
    /// 隔离 HOME（<gameDir>/waylandcraft/ime-nested，Java 注入）。
    home: Option<PathBuf>,
    /// 我们合成器的 WAYLAND_DISPLAY 值（state.socket）。
    socket: Option<OsString>,
    phase: NestedPhase,
    kind: Option<DaemonKind>,
    /// 最近一次 PATH 探测缓存（None=没探过；Some(None)=探过且都没有）。
    probed: Option<Option<DaemonKind>>,
    pid: Option<u32>,
    attempts: u32,
    next_attempt: Option<Instant>,
    started_at: Option<Instant>,
    /// `/wl ime nested stop` 锁：im2 client 断连要等 dispatch 轮次，
    /// 期间 decide 不得把 Disabled 翻回 Active（stop 必须是立即生效的）。
    /// 手动 start / settings 改 mode 解除。
    user_stopped: bool,
}

impl NestedIme {
    pub fn phase(&self) -> &NestedPhase {
        &self.phase
    }
    pub fn mode(&self) -> NestedMode {
        self.mode
    }
    pub fn kind(&self) -> Option<DaemonKind> {
        self.kind
    }
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Java 侧 settings/命令注入配置。mode 非法字符串返回 false（调用方
    /// 报错给用户，不改内部状态）。
    pub fn set_mode(&mut self, raw: &str) -> bool {
        let Some(m) = NestedMode::parse(raw) else {
            return false;
        };
        if m == NestedMode::Off {
            self.phase = NestedPhase::Disabled;
        } else if matches!(self.phase, NestedPhase::Disabled) {
            // off → on/auto：恢复调度（Unconfigured 或 Starting 由后续 tick 定）。
            self.phase = NestedPhase::Unconfigured;
            self.attempts = 0;
            self.next_attempt = None;
        }
        if m != NestedMode::Off {
            // settings 显式改动解除 stop 锁。
            self.user_stopped = false;
        }
        self.mode = m;
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime][nested] 配置 mode={raw:?} → phase={:?}",
            self.phase
        ));
        true
    }

    /// 注入隔离目录（不存在则创建）。
    pub fn set_home(&mut self, path: &Path) {
        if self.home.as_deref() == Some(path) {
            return;
        }
        for sub in ["", "config", "data", "cache"] {
            let p = if sub.is_empty() { path.to_path_buf() } else { path.join(sub) };
            let _ = std::fs::create_dir_all(&p);
        }
        self.home = Some(path.to_path_buf());
        if matches!(self.phase, NestedPhase::Unconfigured) {
            // 等首次 tick 探测二进制。
        }
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime][nested] 隔离 HOME={}",
            path.display()
        ));
    }

    pub fn set_socket(&mut self, socket: &OsStr) {
        if self.socket.as_deref() != Some(socket) {
            self.socket = Some(socket.to_os_string());
        }
    }

    /// 每帧裁决（由 `ImeState::nested_tick` 调用）。`im2_bound` 来自
    /// input_method 对象是否在位；`pid_alive` 注入以便单测。
    /// 返回 Some(request) = 本帧需要真正 spawn（调用方执行后回报结果）。
    pub fn decide(
        &mut self,
        now: Instant,
        im2_bound: bool,
        pid_alive: impl Fn(u32, &str) -> bool,
    ) -> Option<SpawnRequest> {
        // 用户关路 / 停止：彻底不路由不 spawn。
        if self.mode == NestedMode::Off {
            if self.phase != NestedPhase::Disabled {
                self.phase = NestedPhase::Disabled;
                crate::bridge::ime_log_write(
                    "[waylandcraft][ime][nested] ⑥ 决策: mode=off → 路由回退 host_bridge/raw",
                );
            }
            return None;
        }

        // 用户 stop 锁：立即生效，不等 im2 client 断连的那次 dispatch。
        if self.user_stopped {
            return None;
        }
        // 配置未同步（启动窗口期）。
        if self.home.is_none() || self.socket.is_none() {
            self.phase = NestedPhase::Unconfigured;
            return None;
        }
        // 探活主信号：im2 client bind。
        if im2_bound {
            if self.phase != NestedPhase::Active {
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][nested] ⑥ 决策: im2 client bind → 主路径激活（原 phase={:?}）",
                    self.phase
                ));
            }
            self.phase = NestedPhase::Active;
            self.attempts = 0;
            self.next_attempt = None;
            if self.pid.is_none() {
                self.pid = self.read_pid();
            }
            return None;
        }
        // Active 但 client 没了（destroyed 已把 im2_bound 置 false）：
        // 检查进程——还活着就是断连（不重 spawn 同进程没用），杀了重拉。
        if self.phase == NestedPhase::Active || self.phase == NestedPhase::Starting {
            if let (Some(pid), Some(kind)) = (self.pid.or_else(|| self.read_pid()), self.kind) {
                if !pid_alive(pid, kind.comm_prefix()) {
                    crate::bridge::ime_log_write(&format!(
                        "[waylandcraft][ime][nested] ⑥ 决策: daemon pid={pid}({}) 消失 → 回退",
                        kind.binary()
                    ));
                    crate::status::note_error(
                        "warn",
                        &format!("[ime][nested] daemon {} (pid {pid}) 消失，回退 host_bridge/raw", kind.binary()),
                    );
                    self.pid = None;
                    self.phase = NestedPhase::Failed("daemon 进程消失".into());
                    self.next_attempt = Some(now + RETRY_DELAY);
                }
            }
        }
        // Starting 超时判失败。
        if self.phase == NestedPhase::Starting {
            if self.started_at.is_some_and(|t| now - t >= BIND_TIMEOUT) {
                let msg = format!(
                    "spawn {} 后 {}s 无 im2 bind",
                    self.kind.map_or("daemon", |k| k.binary()),
                    BIND_TIMEOUT.as_secs()
                );
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][nested] ⑥ 决策: {msg} → Failed（第 {} 次）",
                    self.attempts + 1
                ));
                self.attempts += 1;
                if self.attempts >= MAX_ATTEMPTS {
                    self.phase = NestedPhase::Failed(format!("{msg}（已重试 {MAX_ATTEMPTS} 次，停；手动 /wl ime nested start 复位）"));
                    crate::status::note_error("error", &format!("[ime][nested] {msg}，放弃自动重试"));
                } else {
                    self.phase = NestedPhase::Failed(msg);
                    self.next_attempt = Some(now + RETRY_DELAY);
                }
            }
            return None;
        }
        // NoBinary / Disabled(手动) 不自动拉。
        if matches!(self.phase, NestedPhase::NoBinary | NestedPhase::Disabled) {
            return None;
        }
        // 封顶：连续失败 MAX_ATTEMPTS 次后彻底停手（防 fork 炸弹），
        // 直到手动 `/wl ime nested start` 或 settings 改动复位。
        if self.attempts >= MAX_ATTEMPTS {
            return None;
        }
        // Failed / Unconfigured / Starting 之外 → 尝试 spawn。
        if let Some(next) = self.next_attempt {
            if now < next {
                return None;
            }
        }
        let available = *self.probed.get_or_insert_with(|| {
            let k = pick_daemon(&find_binary(DaemonKind::Fcitx5.binary()), &find_binary(DaemonKind::Ibus.binary()));
            crate::bridge::ime_log_write(&format!(
                "[waylandcraft][ime][nested] 二进制探测: fcitx5={} ibus-daemon={} → {}",
                find_binary(DaemonKind::Fcitx5.binary()).is_some(),
                find_binary(DaemonKind::Ibus.binary()).is_some(),
                k.map_or("none".to_string(), |kk| kk.binary().to_string()),
            ));
            k
        });
        let Some(kind) = available else {
            if self.phase != NestedPhase::NoBinary {
                self.phase = NestedPhase::NoBinary;
                crate::bridge::ime_log_write(
                    "[waylandcraft][ime][nested] ⑥ 决策: 无 fcitx5/ibus-daemon → 保持 host_bridge/raw",
                );
            }
            return None;
        };
        // 隔离目录可能在游戏生命周期里被删——再兜底建一次。
        let home = self.home.clone()?;
        // 上一 attempt 的进程若还活着（拉起了但没连上我们的 socket）先杀掉
        // 再重拉——绝不累积多个嵌套 daemon。
        if let (Some(pid), Some(kind)) = (self.pid.or_else(|| self.read_pid()), self.kind) {
            if pid_alive(pid, kind.comm_prefix()) {
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][nested] ⑥ 决策: 残留 daemon pid={pid}({}) 未 bind，先杀再重拉",
                    kind.binary()
                ));
                kill_pid(pid);
            }
        }
        let plan = build_spawn_plan(kind, self.socket.as_deref()?, &home);
        self.kind = Some(kind);
        self.phase = NestedPhase::Starting;
        self.started_at = Some(now);
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime][nested] ⑥ 决策: spawn {} (attempt {}/{}) args={:?} env 隔离 HOME={} 禁宿主 dbus",
            kind.binary(),
            self.attempts + 1,
            MAX_ATTEMPTS,
            plan.args,
            home.display(),
        ));
        Some(SpawnRequest { kind, plan })
    }

    /// 调用方回报 spawn 结果。
    pub fn note_spawn_result(&mut self, kind: DaemonKind, result: Result<(), String>, now: Instant) {
        match result {
            Ok(()) => {
                self.pid = self.read_pid();
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][nested] spawn {} 成功 pid={:?}",
                    kind.binary(),
                    self.pid
                ));
            }
            Err(e) => {
                crate::bridge::ime_log_write(&format!(
                    "[waylandcraft][ime][nested] ⑥ 决策: spawn {} 失败: {e}",
                    kind.binary()
                ));
                crate::status::note_error("error", &format!("[ime][nested] spawn {} 失败: {e}", kind.binary()));
                self.attempts += 1;
                self.phase = if self.attempts >= MAX_ATTEMPTS {
                    NestedPhase::Failed(format!("spawn 失败 x{MAX_ATTEMPTS}: {e}（停；手动 start 复位）"))
                } else {
                    self.next_attempt = Some(now + RETRY_DELAY);
                    NestedPhase::Failed(format!("spawn 失败: {e}"))
                };
            }
        }
    }

    /// `/wl ime nested start`：解除 stop 锁、复位重试计数并立即允许一次 spawn。
    pub fn request_start(&mut self, now: Instant) -> String {
        self.mode = NestedMode::On;
        self.user_stopped = false;
        self.attempts = 0;
        self.next_attempt = Some(now);
        if !matches!(self.phase, NestedPhase::Active | NestedPhase::Starting) {
            self.phase = NestedPhase::Unconfigured;
        }
        crate::bridge::ime_log_write("[waylandcraft][ime][nested] ⑥ 决策: 手动 start（计数复位）");
        "nested IM 已请求启动（下一帧 spawn；状态见 /wl ime nested status）".to_string()
    }

    /// `/wl ime nested stop`：杀 pidfile 进程 + 进入 Disabled（不再自动拉）。
    pub fn request_stop(&mut self) -> String {
        let mut killed = String::from("无 pidfile 进程");
        if let Some(pid) = self.pid.or_else(|| self.read_pid()) {
            let ok = kill_pid(pid);
            killed = format!("kill(pid={pid}) {}", if ok { "已发送 SIGTERM" } else { "失败(进程可能已退)" });
        }
        self.pid = None;
        self.phase = NestedPhase::Disabled;
        self.attempts = 0;
        self.next_attempt = None;
        self.user_stopped = true;
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime][nested] ⑥ 决策: 手动 stop → Disabled（{killed}）；路由回退 host_bridge/raw"
        ));
        format!("nested IM 已停止（{killed}），按键回退 host_bridge/raw")
    }

    /// status 一行（/wl ime nested status 与 status.rs 共用）。
    pub fn status_line(&self, im2_bound: bool, grabbed: bool, vk_count: usize) -> String {
        format!(
            "mode={:?} phase={:?} kind={} pid={:?} attempts={} im2={} grab={} vk={}",
            self.mode,
            self.phase,
            self.kind.map_or("-", DaemonKind::binary),
            self.pid.or_else(|| self.read_pid()),
            self.attempts,
            if im2_bound { "bound" } else { "absent" },
            if grabbed { "on" } else { "off" },
            vk_count,
        )
    }

    fn pidfile_path(&self) -> Option<PathBuf> {
        let home = self.home.as_ref()?;
        Some(home.join(format!(
            "{}.pid",
            self.kind.map_or("fcitx5", |k| k.binary())
        )))
    }

    fn read_pid(&self) -> Option<u32> {
        let p = self.pidfile_path()?;
        std::fs::read_to_string(p).ok()?.trim().parse().ok()
    }
}

/// PATH 里找可执行文件（自实现 which：不 fork 子进程，游戏主线程可用）。
pub fn find_binary(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if let Ok(md) = std::fs::metadata(&cand) {
            if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                return Some(cand);
            }
        }
    }
    None
}

/// 选择优先级：fcitx5 优先（实测本机 5.1.12 libwaylandim.so 含
/// zwp_input_method_manager_v2 + zwp_virtual_keyboard_manager_v1 符号，
/// 是唯一完整的 im2 客户端）；没有才试 ibus-daemon（预期 im2 不 bind，
/// 由 Starting 超时如实回退，见模块头）。
pub fn pick_daemon(fcitx5: &Option<PathBuf>, ibus: &Option<PathBuf>) -> Option<DaemonKind> {
    if fcitx5.is_some() {
        Some(DaemonKind::Fcitx5)
    } else if ibus.is_some() {
        Some(DaemonKind::Ibus)
    } else {
        None
    }
}

/// 纯函数：按 kind 组装隔离环境 spawn 计划（单测直接断言它）。
pub fn build_spawn_plan(kind: DaemonKind, socket: &OsStr, home: &Path) -> SpawnPlan {
    let socket_s = socket.to_string_lossy().into_owned();
    // ibus 不传 -d/--daemonize：前台运行，生命周期由我们的双 fork + pidfile 管。
    let (args, log_name): (Vec<String>, &str) = match kind {
        DaemonKind::Fcitx5 => (
            FCITX5_ARGS.iter().map(|s| s.to_string()).collect(),
            "fcitx5.log",
        ),
        DaemonKind::Ibus => (
            vec![
                format!("--address=unix:path={}", home.join("ibus-socket").display()),
                "--panel=disable".into(),
                "--config=disable".into(),
                "--emoji-extension=disable".into(),
                "-n".into(),
                "waylandcraft-nested".into(),
            ],
            "ibus-daemon.log",
        ),
    };
    let mut env_set = vec![
        ("WAYLAND_DISPLAY".to_string(), socket_s),
        ("HOME".to_string(), home.display().to_string()),
        ("XDG_CONFIG_HOME".to_string(), home.join("config").display().to_string()),
        ("XDG_DATA_HOME".to_string(), home.join("data").display().to_string()),
        ("XDG_CACHE_HOME".to_string(), home.join("cache").display().to_string()),
    ];
    // 合成器的 socket 文件在宿主 XDG_RUNTIME_DIR（ListeningSocketSource 行为），
    // daemon 必须看见同一个 runtime dir 才连得上。没有就沿用继承值（不写入 set）。
    if let Some(rd) = std::env::var_os("XDG_RUNTIME_DIR") {
        env_set.push(("XDG_RUNTIME_DIR".to_string(), rd.to_string_lossy().into_owned()));
    }
    SpawnPlan {
        program: kind.binary(),
        args,
        env_set,
        // 隔离纪律核心：绝不连宿主 session bus、绝不开 X。
        env_unset: vec!["DBUS_SESSION_BUS_ADDRESS", "DISPLAY", "XAUTHORITY"],
        log_file: home.join(log_name),
        pidfile: home.join(format!("{}.pid", kind.binary())),
    }
}

/// 双 fork + setsid spawn（process.rs 同惯例：孙进程被 init 收养、无僵尸，
/// 中间子进程把孙 pid 写进 pidfile）。
pub fn spawn_daemon(plan: &SpawnPlan) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(plan.program);
    cmd.args(&plan.args)
        .stdin(Stdio::null());
    for (k, v) in &plan.env_set {
        cmd.env(k, v);
    }
    for k in &plan.env_unset {
        cmd.env_remove(k);
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&plan.log_file)
        .map_err(|e| format!("打开 {} 失败: {e}", plan.log_file.display()))?;
    let err = out
        .try_clone()
        .map_err(|e| format!("clone log fd 失败: {e}"))?;
    cmd.stdout(Stdio::from(out)).stderr(Stdio::from(err));

    unsafe {
        match libc::fork() {
            0 => {
                libc::setsid();
                match libc::fork() {
                    0 => {
                        // 孙进程 exec；失败（PATH 竞态/权限）退 127。
                        let _ = cmd.exec();
                        libc::_exit(127);
                    }
                    -1 => libc::_exit(1),
                    gpid => {
                        // 中间子进程：记录孙 pid 到 pidfile 后立即退出（被 wait 回收）。
                        let _ = std::fs::write(&plan.pidfile, gpid.to_string());
                        libc::_exit(0);
                    }
                }
            }
            -1 => return Err(format!("fork() 失败: {}", std::io::Error::last_os_error())),
            _ => {
                let mut status: i32 = 0;
                libc::wait(&mut status);
            }
        }
    }
    Ok(())
}

/// /proc 探活：pid 存在且 comm 前缀匹配（防 pid 复用误判）。
pub fn proc_comm_matches(pid: u32, comm_prefix: &str) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        Ok(c) => c.trim() == comm_prefix || c.starts_with(comm_prefix),
        Err(_) => false,
    }
}

pub fn kill_pid(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) == 0 }
}

// TODO(端到端实测清单——逻辑与 wire 层已由 tests.rs 模拟 client 全测):
// 1. 隔离 HOME 首次 profile 只含 keyboard-us；需用户把宿主 fcitx5 profile
//    （含 pinyin）拷入 <home>/config/fcitx5/ 或在隔离 HOME 下跑 configtool。
// 2. fcitx5 在 --disable=dbus 下 pinyin 引擎加载/候选行为（备选方案见
//    FCITX5_ARGS 注释）。
// 3. 嵌套应用（firefox）ti3 + 嵌套 fcitx5 im2 + VK 直通全链路真机验证。
// 4. ibus-daemon 路预计不 bind im2 → Failed 回退；若发行版 ibus 带 im2
//    前端再调整。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_accepts_the_three_values() {
        assert_eq!(NestedMode::parse("auto"), Some(NestedMode::Auto));
        assert_eq!(NestedMode::parse("ON"), Some(NestedMode::On));
        assert_eq!(NestedMode::parse(" off "), Some(NestedMode::Off));
        assert_eq!(NestedMode::parse("yes"), None);
        assert_eq!(NestedMode::parse(""), None);
    }

    #[test]
    fn pick_daemon_prefers_fcitx5() {
        let f = Some(PathBuf::from("/usr/bin/fcitx5"));
        let i = Some(PathBuf::from("/usr/bin/ibus-daemon"));
        assert_eq!(pick_daemon(&f, &i), Some(DaemonKind::Fcitx5));
        assert_eq!(pick_daemon(&None, &i), Some(DaemonKind::Ibus));
        assert_eq!(pick_daemon(&None, &None), None);
    }

    #[test]
    fn find_binary_finds_self_and_misses_garbage() {
        assert!(find_binary("sh").is_some(), "sh 必须可在 PATH 里找到");
        assert!(find_binary("definitely-not-a-binary-xyz-42").is_none());
    }

    fn fake_home() -> PathBuf {
        PathBuf::from("/tmp/wlc-test-ime-nested")
    }

    #[test]
    fn spawn_plan_is_isolated_and_never_replaces() {
        let plan = build_spawn_plan(DaemonKind::Fcitx5, OsStr::new("wayland-99"), &fake_home());
        assert_eq!(plan.program, "fcitx5");
        // 隔离环境
        let want = |k: &str| plan.env_set.iter().find(|(ek, _)| ek == k).map(|(_, ev)| ev.clone());
        assert_eq!(want("WAYLAND_DISPLAY").as_deref(), Some("wayland-99"));
        assert_eq!(want("HOME").as_deref(), Some("/tmp/wlc-test-ime-nested"));
        assert_eq!(
            want("XDG_CONFIG_HOME").as_deref(),
            Some("/tmp/wlc-test-ime-nested/config")
        );
        assert!(plan.env_unset.contains(&"DBUS_SESSION_BUS_ADDRESS"));
        assert!(plan.env_unset.contains(&"DISPLAY"));
        // 绝不 --replace/-r 顶掉宿主实例：
        assert!(
            !plan.args.iter().any(|a| a == "--replace" || a == "-r"),
            "spawn plan 出现 --replace/-r: {:?}",
            plan.args
        );
        assert!(plan.args.iter().any(|a| a == "--disable=dbus"));
        assert_eq!(plan.pidfile, fake_home().join("fcitx5.pid"));
    }

    #[test]
    fn ibus_plan_uses_independent_address_and_no_replace() {
        let plan = build_spawn_plan(DaemonKind::Ibus, OsStr::new("wayland-99"), &fake_home());
        assert_eq!(plan.program, "ibus-daemon");
        assert!(plan.args.iter().any(|a| a.starts_with("--address=unix:path=")));
        assert!(!plan.args.iter().any(|a| a == "-d" || a == "--daemonize"),
            "ibus 必须前台由我们管理");
        assert!(!plan.args.iter().any(|a| a == "-r" || a == "--replace"));
        assert!(plan.args.iter().any(|a| a == "--panel=disable"));
    }

    fn configured(n: &mut NestedIme, now: Instant) {
        n.set_mode("on");
        n.home = Some(fake_home());
        n.socket = Some(OsString::from("wayland-99"));
        n.probed = Some(Some(DaemonKind::Fcitx5));
        n.phase = NestedPhase::Unconfigured;
        let _ = now;
    }

    #[test]
    fn unconfigured_waits_for_java_config() {
        let mut n = NestedIme::default();
        let now = Instant::now();
        assert!(n.decide(now, false, |_, _| false).is_none());
        assert_eq!(n.phase, NestedPhase::Unconfigured);
        // mode=off 优先于一切。
        n.set_mode("off");
        assert!(n.decide(now, true, |_, _| true).is_none());
        assert_eq!(n.phase, NestedPhase::Disabled);
    }

    #[test]
    fn mode_off_never_spawns_even_when_bound() {
        let mut n = NestedIme::default();
        let now = Instant::now();
        configured(&mut n, now);
        n.set_mode("off");
        assert!(n.decide(now, false, |_, _| false).is_none());
        assert_eq!(n.phase, NestedPhase::Disabled);
    }

    #[test]
    fn starting_binds_to_active_and_resets_attempts() {
        let mut n = NestedIme::default();
        let now = Instant::now();
        configured(&mut n, now);
        n.attempts = 2;
        let req = n.decide(now, false, |_, _| false).expect("应请求 spawn");
        assert_eq!(req.kind, DaemonKind::Fcitx5);
        assert_eq!(n.phase, NestedPhase::Starting);
        assert!(n.decide(now, true, |_, _| true).is_none());
        assert_eq!(n.phase, NestedPhase::Active);
        assert_eq!(n.attempts, 0);
    }

    #[test]
    fn bind_timeout_counts_attempts_and_caps_at_max() {
        let mut n = NestedIme::default();
        let t0 = Instant::now();
        configured(&mut n, t0);
        n.decide(t0, false, |_, _| false).unwrap();
        // 时钟逐次推进：每轮 = spawn(late_{i-1}) → 超时(t_i) → 退避检查 → 重试。
        let mut t = t0 + BIND_TIMEOUT + Duration::from_millis(1);
        for i in 1..=MAX_ATTEMPTS {
            assert!(n.decide(t, false, |_, _| true).is_none(), "spawn 成功但没 bind → Starting 超时");
            assert_eq!(n.attempts, i);
            // 退避间隔内不重试。
            let early = t + RETRY_DELAY / 2;
            assert!(n.decide(early, false, |_, _| true).is_none());
            // 间隔后重试（除非封顶）。
            let late = t + RETRY_DELAY + Duration::from_millis(1);
            let req = n.decide(late, false, |_, _| true);
            if i < MAX_ATTEMPTS {
                assert!(req.is_some(), "第 {i} 次失败后应退避重试");
                t = late + BIND_TIMEOUT + Duration::from_millis(1);
            } else {
                assert!(req.is_none(), "封顶后绝不继续 spawn");
                assert!(matches!(&n.phase, NestedPhase::Failed(m) if m.contains("停")));
            }
        }
    }

    #[test]
    fn daemon_death_falls_back_and_schedules_retry() {
        let mut n = NestedIme::default();
        let t0 = Instant::now();
        configured(&mut n, t0);
        n.decide(t0, false, |_, _| false).unwrap();
        n.phase = NestedPhase::Active;
        n.pid = Some(424242); // 不存在的 pid
        // im2 client 断开 + pid 死 → Failed + 退避。
        let req = n.decide(t0 + Duration::from_millis(1), false, |_, _| false);
        assert!(req.is_none(), "先标 Failed，退避到期再 spawn");
        assert_eq!(n.phase, NestedPhase::Failed("daemon 进程消失".into()));
        let req = n.decide(t0 + RETRY_DELAY + Duration::from_millis(2), false, |_, _| false);
        assert!(req.is_some(), "退避到期 → 重新 spawn");
    }

    #[test]
    fn manual_start_resets_failure_state() {
        let mut n = NestedIme::default();
        let now = Instant::now();
        configured(&mut n, now);
        n.phase = NestedPhase::Failed("x".into());
        n.attempts = MAX_ATTEMPTS;
        let msg = n.request_start(now);
        assert!(msg.contains("启动"));
        assert_eq!(n.attempts, 0);
        assert!(n.decide(now, false, |_, _| false).is_some(), "手动 start 后立即可 spawn");
    }

    #[test]
    fn manual_stop_disables_routing() {
        let mut n = NestedIme::default();
        let now = Instant::now();
        configured(&mut n, now);
        n.phase = NestedPhase::Active;
        n.pid = Some(999999);
        let msg = n.request_stop();
        assert!(msg.contains("停止"));
        assert_eq!(n.phase, NestedPhase::Disabled);
        // stop 后即使 im2 没 client 也不再自动拉。
        assert!(n.decide(now, false, |_, _| false).is_none());
        // stop 锁：im2 client 还没来得及断（同帧 dispatch 之前）也不翻回 Active。
        assert!(n.decide(now + Duration::from_secs(1), true, |_, _| true).is_none());
        assert_eq!(n.phase, NestedPhase::Disabled);
        // 手动 start 解除锁。
        n.request_start(now + Duration::from_secs(2));
        assert!(n.decide(now + Duration::from_secs(2), false, |_, _| false).is_some());
    }

    #[test]
    fn status_line_reports_everything() {
        let mut n = NestedIme::default();
        configured(&mut n, Instant::now());
        let line = n.status_line(true, true, 1);
        assert!(line.contains("mode=On"));
        assert!(line.contains("im2=bound"));
        assert!(line.contains("grab=on"));
        assert!(line.contains("vk=1"));
    }
}
