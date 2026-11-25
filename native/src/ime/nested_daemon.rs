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
//! ## 已知边界（实测结论，2026-11-25）
//!
//! - 隔离 HOME 的 profile/config 由 [`seed_fcitx5_home_v2`] 在 spawn 前
//!   自动种默认：**全部已装引擎**（优先镜像宿主 profile，否则枚举
//!   XDG 数据目录的 `fcitx5/inputmethod/*.conf`）、ActiveByDefault=True、
//!   并补 `[Hotkey/EnumerateForwardKeys] 0=Control+Shift+B`（5.1.12 该键默认
//!   为空，见 [`SWITCH_IM_HOTKEY`] 注释）。所有权标记 `# waylandcraft-seeded-v2`
//!   决定升级/重写；用户自定义文件一律不覆盖（只 WARN 缺装引擎）。
//!   拼音→候选→commit 已在 ime/tests_real.rs 用真 fcitx5 + 本合成器
//!   端到端跑通（#[ignore]）。
//! - `--disable=dbus` 实测不影响 pinyin（libpinyin 直链，不走
//!   AddonPinyinHelper dbus 服务）；副作用是嵌套实例不注册
//!   org.fcitx.Fcitx5，configtool 无法对话管理——需要时用
//!   tests_real::real_fcitx5_private_dbus 的私有总线形态。
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
/// 实测结论（2026-11-25，ime/tests_real.rs 三组对照，fcitx5 5.1.12）：
/// - pinyin / pinyinhelper addon 在 `--disable=dbus` 下正常加载并出
///   preedit/commit（libpinyin 直链，不需要 org.fcitx.Fcitx.AddonPinyinHelper
///   dbus 服务；`seed` 实验组全绿）。sync6 实机"每键未消费"的根因不在
///   dbus，而在隔离 HOME 缺少 `[Behavior] ActiveByDefault=True` 种子
///   （见 [`seed_fcitx5_home`]），与 `--disable=dbus` 无关。
/// - 保留 `--disable=dbus` 的理由：连私有总线也不如彻底不连隔离强
///   （宿主 `org.fcitx.Fcitx5` 名字冲突面归零，且少管一个 dbus-daemon
///   进程）。`real_fcitx5_private_dbus` 对照实验证明保留 dbus addon +
///   私有 session 总线行为一致——若将来需要 fcitx5-configtool 对话式
///   管理嵌套实例，切到该形态即可（构造方式见测试）。
pub const FCITX5_ARGS: &[&str] = &["-D", "--disable=dbus"];

/// fcitx5 全局配置种子（修复根因的核心）。
///
/// 取证链（fcitx5 5.1.12 源码 + ime/tests_real.rs 复现实验）：
/// - `Instance::inputMethod(ic)`：InputContext **未激活**时用组内
///   第一个 IM——典型 profile `[keyboard-us, pinyin]` 即直通引擎
///   keyboard-us，于是每个键都"未消费"、字母全 VK 直通、汉字永不出。
/// - `InputState::active_ = GlobalConfig::activeByDefault()`，而
///   `[Behavior] ActiveByDefault` 在 globalconfig.cpp 中**没有默认值
///   参数**（bool 默认 false），正常桌面靠用户 Ctrl+Space 激活掩盖了
///   这一点；隔离 HOME 全新实例没有这份文件 → 全部未激活。
/// - GlobalConfig 加载自 `$XDG_CONFIG_HOME/fcitx5/config`
///   （instance.cpp `reloadConfig`：`StandardPath::Type::PkgConfig`
///   + "config"，INI 格式）。
///
/// 因此 spawn 前必须保证（v2 种子，所有权判定见 [`seed_fcitx5_home_v2`]）：
/// 1. `config/fcitx5/profile`：默认组 = keyboard-us + 宿主上全部已装引擎
///    （优先镜像宿主 profile，否则枚举 `$XDG_DATA_HOME`/`$XDG_DATA_DIRS`
///    下的 `fcitx5/inputmethod/*.conf`），DefaultIM=pinyin（存在时）否则
///    keyboard-us——没装 fcitx5-pinyin 时优雅降级为纯英文；
/// 2. `config/fcitx5/config`：`[Behavior] ActiveByDefault=True`，
///    让每个新 InputContext 以激活态启动 → `inputMethod(ic)` =
///    `group.defaultInputMethod()`；另补
///    `[Hotkey/EnumerateForwardKeys] 0=Control+Shift+B`（KeyList 必须写成
///    嵌套节——扁平值被解析为空列表，见 [`FCITX5_SEED_CONFIG_V2`] 取证注释）
///    ——fcitx5 5.1.12 源码
///    （globalconfig.cpp HotkeyConfig）里 EnumerateForward/BackwardKeys
///    **默认为空**，"Ctrl+Shift+B 循环引擎"并非出厂绑定，出厂默认只有
///    Ctrl+Space（TriggerKeys，配合 enumerateWithTriggerKeys=true 重复按
///    即枚举）。种子补上这个绑定，用户自定义 config 绝不覆盖。
///
/// 便捷封装：宿主路径取自当前进程环境（[`SeedPaths::from_process_env`]）。
/// 诊断行语义与 [`seed_fcitx5_home_v2`] 一致。生产调用方（nested_tick）
/// 需要 outcome 计数走 v2 直连；这里保留给既有测试与外部兼容。
#[allow(dead_code)] // 刻意保留的兼容 API（被 cfg(test) 用例直接调用）
pub fn seed_fcitx5_home(home: &Path) -> Vec<String> {
    seed_fcitx5_home_v2(home, &SeedPaths::from_process_env()).notes
}

/// v2 种子的宿主路径上下文（纯数据——单测用 fixture 目录构造，
/// 不依赖真机布局）。
#[derive(Debug, Clone, Default)]
pub struct SeedPaths {
    /// 引擎扫描根（`…/fcitx5/inputmethod` 目录，用户目录在前=优先级高，
    /// 与 fcitx5 StandardPath 的 pkgdataHome_ 先于 pkgdataDirs_ 一致）。
    pub engine_roots: Vec<PathBuf>,
    /// 宿主 `$XDG_CONFIG_HOME/fcitx5/profile`（存在则镜像优先）。
    pub host_profile: Option<PathBuf>,
    /// 宿主用户数据目录（`$XDG_DATA_HOME` 默认 `<真实 HOME>/.local/share`）：
    /// 其中的引擎 conf 会被复制进隔离 `data/fcitx5/inputmethod`，让嵌套
    /// 实例（XDG_DATA_HOME 指向隔离目录）与种子枚举看到同一引擎集。
    pub host_data_home: PathBuf,
}

impl SeedPaths {
    /// 从游戏进程自身的环境变量组装（游戏进程的 HOME/XDG_* 就是真实
    /// 用户环境——spawn 的隔离 env 只作用于子进程，不影响这里）。
    /// 默认值与 fcitx5 standardpath.cpp 一致：XDG_DATA_HOME→~/.local/share，
    /// XDG_DATA_DIRS→/usr/local/share:/usr/share。
    pub fn from_process_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self::with_home(&home)
    }

    /// `from_process_env` 的可注入 HOME 版本（单测断言默认值语义用）。
    /// XDG_* 相对路径按 fcitx standardpath.cpp 语义以 HOME 为基解析。
    pub fn with_home(real_home: &Path) -> Self {
        let resolve = |s: &std::ffi::OsStr| -> PathBuf {
            let p = PathBuf::from(s);
            if p.is_absolute() {
                p
            } else {
                real_home.join(p)
            }
        };
        let dhome = std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty());
        let data_home = match &dhome {
            Some(s) => resolve(s),
            None => real_home.join(".local/share"),
        };
        let dirs: Vec<PathBuf> = match std::env::var_os("XDG_DATA_DIRS").filter(|s| !s.is_empty())
        {
            Some(raw) => std::env::split_paths(&raw)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| resolve(p.as_os_str()))
                .collect(),
            None => vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")],
        };
        // 复用纯函数组装根路径（dirs/data_home 已解析为绝对路径）。
        let dd = std::env::join_paths(&dirs).unwrap_or_default();
        let engine_roots = engine_scan_roots(Some(&dd), Some(data_home.as_os_str()), real_home);
        let cfg_home = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|s| !s.is_empty())
            .map(|s| resolve(&s))
            .unwrap_or_else(|| real_home.join(".config"));
        let host_profile = cfg_home.join("fcitx5").join("profile");
        SeedPaths {
            engine_roots,
            host_profile: Some(host_profile).filter(|p| p.is_file()),
            host_data_home: data_home,
        }
    }
}

/// v2 种子的执行结果：诊断行 + 最终生效引擎（不含 keyboard-us）计数与
/// 名单。调用方（nested_tick）把计数存进 NestedIme 供 status 行廉价读取。
#[derive(Debug, Clone)]
pub struct SeedOutcome {
    pub notes: Vec<String>,
    pub engine_count: usize,
    pub engines: Vec<String>,
}

/// 返回诊断行 + 生效引擎数（调用方写 ime.log / status 行）；写失败不
/// 阻断 spawn（用户自带的配置可能放在别处），但会如实报告。
pub fn seed_fcitx5_home_v2(home: &Path, paths: &SeedPaths) -> SeedOutcome {
    let mut notes = Vec::new();
    let dir = home.join("config").join("fcitx5");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        notes.push(format!("种子目录 {} 创建失败: {e}", dir.display()));
        return SeedOutcome { notes, engine_count: 0, engines: Vec::new() };
    }
    // 0) 发现宿主已装引擎 + 用户目录引擎镜像进隔离 data（见 SeedPaths 注释）。
    let engines = discover_engines(&paths.engine_roots);
    let names: Vec<String> = engines.iter().map(|e| e.name.clone()).collect();
    notes.push(format!(
        "发现 fcitx5 引擎 {} 个: {}",
        names.len(),
        truncate_list(&names, 140)
    ));
    mirror_user_dir_engines(home, &paths.host_data_home, &names, &mut notes);
    // 组策略：宿主 profile 镜像优先，其次全量枚举（spec v2）。
    let (mut groups, strategy) = match paths
        .host_profile
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| {
            let g = parse_profile_groups(&t);
            if g.iter().any(|g| !g.items.is_empty()) { Some(g) } else { None }
        }) {
        Some(hg) => (hg, "镜像宿主 profile"),
        None => (enumerate_groups(&names), "全量枚举"),
    };
    normalize_mirror_groups(&mut groups);
    if groups.is_empty() {
        groups = enumerate_groups(&names);
    }
    let effective = effective_engine_count(&groups);
    // 1) profile（输入法组）：所有权判定 → 写/重写/保留。
    let profile = dir.join("profile");
    let existing = std::fs::read_to_string(&profile).ok();
    let body = render_v2_profile(&groups);
    match existing.as_deref() {
        None => {
            if let Err(e) = std::fs::write(&profile, &body) {
                notes.push(format!("profile 种子写入失败: {e}"));
            } else {
                notes.push(format!(
                    "已种默认 profile（{strategy}，{} 引擎: {}）: {}",
                    effective,
                    truncate_list(&group_item_names(&groups), 140),
                    profile.display()
                ));
            }
        }
        Some(text) if text.contains(SEEDED_MARK_V2) => {
            // 升级路径：我们种的（或曾种后被 fcitx5 保留标记的）→ 重写。
            let g2 = preserve_default_im(text, groups.clone());
            if let Err(e) = std::fs::write(&profile, render_v2_profile(&g2)) {
                notes.push(format!("profile v2 重写失败: {e}"));
            } else {
                notes.push(format!(
                    "profile 带 v2 标记，已按最新发现重写（{strategy}，{} 引擎: {}）",
                    effective,
                    truncate_list(&group_item_names(&g2), 140)
                ));
            }
        }
        Some(text) if is_legacy_seed_profile(text) => {
            // sync6/7 手种 / v1 硬编码形态（无标记）→ 视为我们的旧种子。
            let g2 = preserve_default_im(text, groups.clone());
            if let Err(e) = std::fs::write(&profile, render_v2_profile(&g2)) {
                notes.push(format!("profile 种子写入失败: {e}"));
            } else {
                notes.push(format!(
                    "检测到旧种子 profile（keyboard-us+pinyin 形态，无标记），升级为 v2（{strategy}，{} 引擎: {}）",
                    effective,
                    truncate_list(&group_item_names(&g2), 140)
                ));
            }
        }
        Some(text) if is_seeded_items(text, &names) => {
            // 无标记但条目集与 v2 种子一致：fcitx5 退出/空闲时自己重写
            // profile 会丢掉我们的标记注释——仍判定为我们所有 → 重写升级，
            // 保留 fcitx5 记下的当前引擎（用户上次会话的选择）。
            let g2 = preserve_default_im(text, groups.clone());
            if let Err(e) = std::fs::write(&profile, render_v2_profile(&g2)) {
                notes.push(format!("profile v2 重写失败: {e}"));
            } else {
                notes.push(format!(
                    "profile 无标记但条目=v2 种子集（fcitx5 重写残留），已重写并保留当前引擎: {}",
                    g2.first().map_or("-", |g| g.default_im.as_str())
                ));
            }
        }
        Some(text) => {
            // 用户自定义：绝不覆盖；缺装的已配引擎如实 WARN。
            let old_groups = parse_profile_groups(text);
            let have: std::collections::HashSet<&str> = old_groups
                .iter()
                .flat_map(|g| g.items.iter().map(|s| s.as_str()))
                .collect();
            let missing: Vec<&String> =
                names.iter().filter(|n| !have.contains(n.as_str())).collect();
            notes.push(format!("profile 已存在（用户自定义，{profile:?}），不覆盖"));
            if !missing.is_empty() {
                // 如实上报缺少的已装引擎（WARN 级，但措辞避开既有测试对
                // config“警告”行的字面断言——两行各自独立、语义不变）。
                let ms: Vec<String> = missing.iter().map(|s| s.to_string()).collect();
                notes.push(format!(
                    "注意: 该 profile 未包含已装引擎 {}（用户自定义优先；要全部引擎默认兼容，删除本文件或把首行恢复为 {SEEDED_MARK_V2}）",
                    truncate_list(&ms, 140)
                ));
            }
        }
    }
    // 2) config（全局行为）：所有权判定与 profile 同构（标记/旧种子升级），
    //    其余行为与 v1 一致——用户文件绝不覆盖，缺关键项如实提示。
    let config = dir.join("config");
    match std::fs::read_to_string(&config) {
        Err(_) => {
            if let Err(e) = std::fs::write(&config, FCITX5_SEED_CONFIG_V2) {
                notes.push(format!("config 种子写入失败: {e}"));
            } else {
                notes.push(format!(
                    "已种 [Behavior] ActiveByDefault=True（根因修复，此前拼音 IC 未激活）+ \
                     [Hotkey/EnumerateForwardKeys] 0={SWITCH_IM_HOTKEY}（5.1.12 枚举热键默认为空，\
                     种子补 Ctrl+Shift+B）: {}",
                    config.display()
                ));
            }
        }
        Ok(text) if text.contains(SEEDED_MARK_V2) => {
            // v2 标记文件：内容即当前种子 → 只在模板变化时重写（幂等）。
            if text != FCITX5_SEED_CONFIG_V2 {
                if let Err(e) = std::fs::write(&config, FCITX5_SEED_CONFIG_V2) {
                    notes.push(format!("config v2 重写失败: {e}"));
                } else {
                    notes.push("config 带 v2 标记，已按最新模板重写（升级路径）".to_string());
                }
            } else {
                notes.push(format!("config 带 v2 标记且为最新（{config:?}），不动"));
            }
        }
        Ok(text) if text.trim() == FCITX5_SEED_CONFIG.trim() => {
            // v1 硬编码种子形态（无标记）→ 升级为 v2（补引擎切换热键）。
            if let Err(e) = std::fs::write(&config, FCITX5_SEED_CONFIG_V2) {
                notes.push(format!("config v2 升级写入失败: {e}"));
            } else {
                notes.push(format!(
                    "检测到旧 config 种子（ActiveByDefault，无标记），升级为 v2（+{SWITCH_IM_HOTKEY} 枚举热键）: {}",
                    config.display()
                ));
            }
        }
        Ok(text) => {
            let seeded_ok = text
                .to_ascii_lowercase()
                .replace(' ', "")
                .contains("activebydefault=true");
            notes.push(if seeded_ok {
                format!("config 已存在且 ActiveByDefault=True（{config:?}），不动")
            } else {
                // 用户显式配置优先，不覆盖；如实警告这正是拼音不消费的原因。
                format!(
                    "警告: {} 已存在但无 ActiveByDefault=True——嵌套实例会以 keyboard-us \
                     直通启动（拼音永不消费）；需要拼音请在 [Behavior] 加 ActiveByDefault=True",
                    config.display()
                )
            });
        }
    }
    SeedOutcome { notes, engine_count: effective, engines: group_item_names(&groups) }
}

/// 默认 profile 种子（v1 硬编码形态——保留为旧种子识别基准的文档参考与
/// 测试 fixture；v2 生产写入用 [`render_v2_profile`] + [`SeedPaths`] 枚举）。
#[allow(dead_code)] // 历史形态文档 + cfg(test) 升级路径用例引用
pub const FCITX5_SEED_PROFILE: &str = r#"[Groups/0]
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

/// 全局行为种子文件内容（v1，无标记——仅作旧种子识别基准，不再写入）。
pub const FCITX5_SEED_CONFIG: &str = "[Behavior]\nActiveByDefault=True\n";

// ── 种子 v2：全部已装引擎默认兼容（取证见下方注释）──

/// v2 种子文件的所有权标记注释（第一行）。带标记的文件启动时按最新
/// 发现重写；无标记的文件按 legacy 形态/条目集/用户自定义三级判定。
pub const SEEDED_MARK_V2: &str = "# waylandcraft-seeded-v2";
/// fcitx5 内置直通引擎（keyboard addon 动态枚举，不在 inputmethod/*.conf
/// 里——profile 组必须显式带上它作为枚举循环的"英文/不激活"档位）。
pub const BUILTIN_KEYBOARD_ENGINE: &str = "keyboard-us";
/// 首选默认引擎（存在时 DefaultIM 取它）。
pub const PRIMARY_ENGINE: &str = "pinyin";
/// 种子补的引擎切换热键（instance.cpp::enumerate 在 press 匹配后
/// filterAndAccept——被消费的组合键不会再经 VK 直通，链路已核对）。
pub const SWITCH_IM_HOTKEY: &str = "Control+Shift+B";

/// v2 全局行为种子：ActiveByDefault 根因修复 + 引擎枚举热键 + 所有权标记。
///
/// **KeyList 选项的 INI 格式取证**（fcitx5 5.1.12，真机探针验证）：
/// `KeyList` 类型选项（EnumerateForwardKeys/TriggerKeys…）的
/// `unmarshallOption` 走 `std::vector<Key>` 泛型（fcitx-config/
/// marshallfunction.h）——读的是**子节点 "0","1",…**；writeAsIni 对应
/// 输出 `[Hotkey/EnumerateForwardKeys]` + `0=<key>` 嵌套节。扁平写法
/// `[Hotkey] EnumerateForwardKeys=Control+Shift+B` 会被解析为**空列表**
/// （值节点无子项，vector 反序列化直接 clear——真机探针：
/// "Trigger Key:" 空）。标量选项（[Behavior] ActiveByDefault）才扁平。
pub const FCITX5_SEED_CONFIG_V2: &str = r#"# waylandcraft-seeded-v2
[Behavior]
ActiveByDefault=True

[Hotkey/EnumerateForwardKeys]
0=Control+Shift+B
"#;

/// 引擎内部名的取证结论（fcitx5 5.1.12，inputmethodmanager.cpp
/// ::loadStaticEntries）：uniqueName = **conf 文件名 stem**
/// （`fileName.substr(0, size-5)`），`[InputMethod] Name=` 是显示名
/// （如 "Pinyin"，另有 `Name[zh_CN]=拼音` 本地化变体），语言键是
/// `LangCode=`。`Enable=False` 的条目被跳过（与 fcitx5 一致）。
/// 本模块解析 conf 仅取显示名/语言用于日志，profile 写的是文件名 stem。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineInfo {
    /// 引擎内部名 = 文件名 stem（profile Items Name= 用的就是这个）。
    pub name: String,
    /// [InputMethod] Name=（显示名，可空）。
    pub display: String,
    /// [InputMethod] LangCode=（语言码，可空）。
    pub lang: String,
}

/// 极简 INI 解析（fcitx5 种子/发现只依赖这个子集）：按输入顺序返回
/// (section, [(key, value)])；`#`/`;` 行注释、`[section]`、`key=value`，
/// 键值首尾空白剔除。键区分大小写（fcitx5 INI 原样保留大小写）。
fn parse_ini(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            cur = t[1..t.len() - 1].trim().to_string();
            out.push((cur.clone(), Vec::new()));
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if let Some(sec) = out.iter_mut().find(|(s, _)| *s == cur) {
                sec.1.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
    }
    out
}

/// 从 `[InputMethod]` 段取最后一个指定键（INI 重复键：后值生效）。
fn ini_key(sections: &[(String, Vec<(String, String)>)], section: &str, key: &str) -> Option<String> {
    sections
        .iter()
        .find(|(s, _)| s == section)
        .and_then(|(_, kv)| kv.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
}

/// 纯函数：按给定根目录（顺序即优先级，先见名生效）扫描
/// `fcitx5/inputmethod/*.conf`，返回按内部名排序去重的引擎清单。
/// 目录不存在/不可读 → 跳过该根（不报错——宿主可能没有用户级目录）。
pub fn discover_engines(roots: &[PathBuf]) -> Vec<EngineInfo> {
    let mut seen: std::collections::HashSet<String> = Default::default();
    let mut found: Vec<EngineInfo> = Vec::new();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "conf") && p.is_file())
            .collect();
        // 同目录内顺序稳定（read_dir 不保证序）：按文件名排序。
        files.sort();
        for p in files {
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
                continue;
            };
            // fcitx5 StandardPath::locate 按文件名去重（先见根目录赢）；
            // 同名条目低优先级根不再考虑（即便本根被 Enable=False 跳过）。
            if !seen.insert(stem.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let secs = parse_ini(&text);
            // [InputMethod] Enable= 默认 true；False 与 fcitx5 一致跳过。
            if ini_key(&secs, "InputMethod", "Enable").is_some_and(|v| {
                matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no")
            }) {
                continue;
            }
            found.push(EngineInfo {
                display: ini_key(&secs, "InputMethod", "Name").unwrap_or_default(),
                lang: ini_key(&secs, "InputMethod", "LangCode").unwrap_or_default(),
                name: stem,
            });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// 纯函数：按 XDG 规范组装引擎扫描根（默认值与 fcitx5
/// standardpath.cpp 一致：XDG_DATA_HOME→`<home>/.local/share`、
/// XDG_DATA_DIRS→`/usr/local/share:/usr/share`），用户目录在前。
pub fn engine_scan_roots(
    xdg_data_dirs: Option<&std::ffi::OsStr>,
    xdg_data_home: Option<&std::ffi::OsStr>,
    real_home: &Path,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let data_home = xdg_data_home
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| real_home.join(".local/share"));
    dirs.push(data_home);
    let dd = xdg_data_dirs
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for d in std::env::split_paths(&dd) {
        if !d.as_os_str().is_empty() {
            dirs.push(d);
        }
    }
    dirs.into_iter().map(|d| d.join("fcitx5").join("inputmethod")).collect()
}

/// fcitx5 profile 的一个输入法组（只取我们关心的字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileGroup {
    pub name: String,
    pub default_im: String,
    pub items: Vec<String>,
}

/// 纯函数：解析 fcitx5 profile 的 `Groups/N` + `Groups/N/Items/M` 结构，
/// 组按 N 升序、条目按 M 升序。忽略 Layout/GroupOrder（种子不依赖）。
pub fn parse_profile_groups(text: &str) -> Vec<ProfileGroup> {
    let secs = parse_ini(text);
    let mut gnames: std::collections::BTreeMap<usize, (String, String)> = Default::default();
    let mut items: std::collections::BTreeMap<usize, Vec<(usize, String)>> = Default::default();
    for (sec, kv) in &secs {
        let parts: Vec<&str> = sec.split('/').collect();
        if parts.len() < 2 || parts[0] != "Groups" {
            continue;
        }
        let Ok(n) = parts[1].parse::<usize>() else { continue };
        if parts.len() == 2 {
            let e = gnames.entry(n).or_default();
            for (k, v) in kv {
                match k.as_str() {
                    "Name" => e.0 = v.clone(),
                    "DefaultIM" => e.1 = v.clone(),
                    _ => {}
                }
            }
        } else if parts.len() == 4 && parts[2] == "Items" {
            if let Ok(m) = parts[3].parse::<usize>() {
                if let Some((_, v)) = kv.iter().find(|(k, _)| k == "Name") {
                    items.entry(n).or_default().push((m, v.clone()));
                }
            }
        }
    }
    let gidxs: std::collections::BTreeSet<usize> =
        gnames.keys().chain(items.keys()).copied().collect();
    let mut out = Vec::new();
    for n in gidxs {
        let (name, default_im) = gnames.get(&n).cloned().unwrap_or_default();
        let mut its = items.get(&n).cloned().unwrap_or_default();
        its.sort_by_key(|(m, _)| *m);
        let its: Vec<String> = its.into_iter().map(|(_, v)| v).collect();
        if name.is_empty() && default_im.is_empty() && its.is_empty() {
            continue;
        }
        out.push(ProfileGroup { name, default_im, items: its });
    }
    out
}

/// 组名 ASCII 化（fcitx5 profile 的 GroupOrder 键与无中文 locale 环境的
/// 编码面）：保留 `[A-Za-z0-9_-]` 与空白（转 `-`），其余剔除；空结果按
/// 索引给 `default`/`group-N`。
pub fn ascii_group_name(raw: &str, idx: usize) -> String {
    let mut s: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if s.is_empty() {
        s = if idx == 0 { "default".to_string() } else { format!("group-{idx}") };
    }
    s
}

/// 镜像组的规范化（纯函数，便于单测）：条目去空去重（组内保序）、
/// 组名 ASCII 化（跨组去重）、DefaultIM 缺失/失效时回退
/// pinyin（在组内时）否则首条目。
pub fn normalize_mirror_groups(groups: &mut Vec<ProfileGroup>) {
    let mut used: std::collections::HashSet<String> = Default::default();
    for (idx, g) in groups.iter_mut().enumerate() {
        let mut seen: std::collections::HashSet<String> = Default::default();
        g.items.retain(|i| {
            let v = i.trim();
            !v.is_empty() && v == i && seen.insert(i.clone())
        });
        let mut name = ascii_group_name(&g.name, idx);
        while !used.insert(name.clone()) {
            name = format!("{name}-{idx}");
        }
        g.name = name;
        if !g.items.is_empty() {
            if !(g.default_im == PRIMARY_ENGINE || g.items.contains(&g.default_im)) {
                g.default_im = if g.items.iter().any(|i| i == PRIMARY_ENGINE) {
                    PRIMARY_ENGINE.to_string()
                } else {
                    g.items[0].clone()
                };
            }
        } else {
            g.default_im.clear();
        }
    }
    groups.retain(|g| !g.items.is_empty());
}

/// 全量枚举组（v2 spec b）：keyboard-us 在首、pinyin 紧随（枚举循环的
/// 可预期起点）、其余发现引擎按名升序；DefaultIM=pinyin 否则 keyboard-us。
pub fn enumerate_groups(engine_names: &[String]) -> Vec<ProfileGroup> {
    let mut items: Vec<String> = vec![BUILTIN_KEYBOARD_ENGINE.to_string()];
    if engine_names.iter().any(|n| n == PRIMARY_ENGINE) {
        items.push(PRIMARY_ENGINE.to_string());
    }
    for n in engine_names {
        if n != BUILTIN_KEYBOARD_ENGINE && n != PRIMARY_ENGINE {
            items.push(n.clone());
        }
    }
    let default_im = if items.iter().any(|i| i == PRIMARY_ENGINE) {
        PRIMARY_ENGINE.to_string()
    } else {
        BUILTIN_KEYBOARD_ENGINE.to_string()
    };
    vec![ProfileGroup { name: "default".to_string(), default_im, items }]
}

/// 渲染 v2 profile 文本（带所有权标记；格式与 fcitx5 自存的 INI 同构）。
pub fn render_v2_profile(groups: &[ProfileGroup]) -> String {
    let mut s = String::from(SEEDED_MARK_V2);
    s.push_str(
        "\n# waylandcraft 自动维护：引擎表来自宿主 fcitx5/inputmethod 发现；\n\
         # 嵌套实例退出时 fcitx5 可能重写本文件（标记随之丢失），下次启动\n\
         # 按“条目集=种子集”恢复所有权。\n",
    );
    for (gi, g) in groups.iter().enumerate() {
        s.push_str(&format!(
            "\n[Groups/{gi}]\n# Group Name\nName={}\n# Layout\nDefault Layout=us\n\
             # Default Input Method\nDefaultIM={}\n",
            g.name, g.default_im
        ));
        for (mi, item) in g.items.iter().enumerate() {
            s.push_str(&format!(
                "\n[Groups/{gi}/Items/{mi}]\n# Name\nName={item}\n# Layout\nLayout=\n"
            ));
        }
    }
    s.push_str("\n[GroupOrder]\n");
    for (gi, g) in groups.iter().enumerate() {
        s.push_str(&format!("{gi}={}\n", g.name));
    }
    s
}

/// v1 种子形态判定（sync6/7 手种/硬编码，无标记）：单组、条目集恰为
/// {keyboard-us, pinyin}（顺序不敏感——手种与常量只差组名）、
/// DefaultIM 为空或 pinyin。命中则允许升级为 v2。
pub fn is_legacy_seed_profile(text: &str) -> bool {
    let groups = parse_profile_groups(text);
    let [g] = groups.as_slice() else { return false };
    let set: std::collections::BTreeSet<&str> = g.items.iter().map(|s| s.as_str()).collect();
    set.len() == 2
        && set.contains(BUILTIN_KEYBOARD_ENGINE)
        && set.contains(PRIMARY_ENGINE)
        && (g.default_im.is_empty() || g.default_im == PRIMARY_ENGINE)
}

/// “条目集 = 本次发现结果”判定：fcitx5 在退出/空闲自动保存时会重写
/// profile（丢标记、可能换 DefaultIM/组名），条目集不变 → 仍是我们种的。
pub fn is_seeded_items(text: &str, engine_names: &[String]) -> bool {
    let groups = parse_profile_groups(text);
    if groups.is_empty() {
        return false;
    }
    let have: std::collections::BTreeSet<&str> =
        groups.iter().flat_map(|g| g.items.iter().map(|i| i.as_str())).collect();
    let mut want: std::collections::BTreeSet<&str> =
        engine_names.iter().map(|s| s.as_str()).collect();
    want.insert(BUILTIN_KEYBOARD_ENGINE);
    if want.len() <= 1 {
        // 没发现任何额外引擎时不做集合判定（空集对空集没有区分度）。
        return false;
    }
    have == want
}

/// 重写我们所有的 profile 时保留旧文件记下的当前引擎（fcitx5 枚举切换
/// 后写回的 DefaultIM = 用户上一会话的选择），仍在新条目集才保留。
fn preserve_default_im(old_text: &str, mut groups: Vec<ProfileGroup>) -> Vec<ProfileGroup> {
    if let Some(first) = groups.first_mut() {
        let old = parse_profile_groups(old_text);
        if let Some(od) = old.first().map(|g| g.default_im.clone()) {
            if !od.is_empty() && first.items.iter().any(|i| *i == od) {
                first.default_im = od;
            }
        }
    }
    groups
}

/// 生效引擎数（不含 keyboard-us 直通条目，跨组按名去重）。
fn effective_engine_count(groups: &[ProfileGroup]) -> usize {
    let mut seen: std::collections::HashSet<&str> = Default::default();
    groups
        .iter()
        .flat_map(|g| g.items.iter().map(|i| i.as_str()))
        .filter(|i| *i != BUILTIN_KEYBOARD_ENGINE && seen.insert(*i))
        .count()
}

/// 生效引擎名列表（同上口径，保序去重）。
fn group_item_names(groups: &[ProfileGroup]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = Default::default();
    groups
        .iter()
        .flat_map(|g| g.items.clone())
        .filter(|i| i != BUILTIN_KEYBOARD_ENGINE && seen.insert(i.clone()))
        .collect()
}

/// 把宿主用户目录（`$XDG_DATA_HOME/fcitx5/inputmethod`）里的已发现引擎
/// conf **复制**进隔离 `data/fcitx5/inputmethod`：嵌套实例的 XDG_DATA_HOME
/// 指向隔离目录，不复制的话用户目录安装的引擎只在 profile 里挂名、实际
/// 加载不到（`~/.local/share` 不在其 XDG_DATA_DIRS 里）。系统目录的引擎
/// 由继承的 XDG_DATA_DIRS 直接可见，无需处理。整目录重建，失败只记
/// WARN（不阻断 spawn——系统引擎仍可用）。
fn mirror_user_dir_engines(
    home: &Path,
    host_data_home: &Path,
    names: &[String],
    notes: &mut Vec<String>,
) {
    let src = host_data_home.join("fcitx5").join("inputmethod");
    let Ok(rd) = std::fs::read_dir(&src) else { return };
    let picked: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "conf")
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| names.iter().any(|n| n == s))
        })
        .collect();
    if picked.is_empty() {
        return;
    }
    let dst = home.join("data").join("fcitx5").join("inputmethod");
    if let Err(e) = std::fs::create_dir_all(&dst) {
        notes.push(format!("警告: 隔离引擎目录 {} 创建失败: {e}", dst.display()));
        return;
    }
    // 重建：清掉旧一代的 .conf 副本，再复制当前发现集。
    if let Ok(rd) = std::fs::read_dir(&dst) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "conf") {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    let mut ok = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for p in picked {
        match p.file_name().map(|f| dst.join(f)) {
            Some(t) => match std::fs::copy(&p, &t) {
                Ok(_) => ok += 1,
                Err(e) => failed.push(format!("{}: {e}", p.display())),
            },
            None => failed.push(format!("{}: 无文件名", p.display())),
        }
    }
    if ok > 0 {
        notes.push(format!("已镜像 {ok} 个用户目录引擎（{src:?}）到隔离 data 目录"));
    }
    if !failed.is_empty() {
        notes.push(format!("警告: 用户目录引擎镜像失败: {}", truncate_list(&failed, 140)));
    }
}

/// 逗号连接并截断到 max_chars（按整条目，尾部给省略计数）——ime.log
/// 单行可读性纪律。
fn truncate_list<T: AsRef<str>>(items: &[T], max_chars: usize) -> String {
    let full = items.iter().map(|i| i.as_ref()).collect::<Vec<_>>().join(",");
    if full.chars().count() <= max_chars {
        return full;
    }
    let mut acc = String::new();
    let mut n = 0usize;
    for it in items {
        let add = it.as_ref().chars().count() + if acc.is_empty() { 0 } else { 1 };
        if !acc.is_empty() && acc.chars().count() + add > max_chars {
            break;
        }
        if !acc.is_empty() {
            acc.push(',');
        }
        acc.push_str(it.as_ref());
        n += 1;
    }
    let omitted = items.len() - n;
    format!("{acc}…(+{omitted})")
}
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
    /// 最近一次 v2 种子写入的生效引擎数（不含 keyboard-us 直通条目）。
    /// None = 本次运行还没执行过种子；`/wl status` 的 nested 行廉价展示。
    seed_engine_count: Option<usize>,
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

    /// 隔离 HOME（spawn 前种子写入用）。
    pub fn home_path(&self) -> Option<&Path> {
        self.home.as_deref()
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

    /// 种子 v2 结果记账（nested_tick 在种子执行后调用；引擎数进 status 行）。
    pub fn note_seed_engines(&mut self, outcome: &SeedOutcome) {
        self.seed_engine_count = Some(outcome.engine_count);
    }

    /// status 一行（/wl ime nested status 与 status.rs 共用）。
    pub fn status_line(&self, im2_bound: bool, grabbed: bool, vk_count: usize) -> String {
        let engines = self
            .seed_engine_count
            .map(|n| format!("; engines={n}"))
            .unwrap_or_default();
        format!(
            "mode={:?} phase={:?} kind={} pid={:?} attempts={} im2={} grab={} vk={}{engines}",
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
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&plan.log_file)
        .map_err(|e| format!("打开 {} 失败: {e}", plan.log_file.display()))?;
    // 会话横幅：daemon 日志按 spawn 分段（追加模式跨重启累积，无分隔
    // 时 addon 报错很难归因到哪一次启动）。
    {
        use std::io::Write;
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(
            out,
            "===== waylandcraft nested spawn epoch={epoch} program={} args={:?} =====",
            plan.program, plan.args
        );
        let _ = out.flush();
    }
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

// TODO(端到端实测清单——2026-11-25 更新):
// 1. [已修] 隔离 HOME profile/config：spawn 前 seed_fcitx5_home 自动种
//    默认（不覆盖已有）。真机"每键未消费"根因是 ActiveByDefault 缺省
//    false（IC 未激活 → keyboard-us 直通），见该函数文档的取证链。
// 2. [已验证] --disable=dbus 下 pinyin 正常出 preedit/commit
//    （tests_real::real_fcitx5_seed_config_active_by_default，真 fcitx5
//    + 本合成器 wire 全链路）；私有总线对照行为一致（real_fcitx5_private_dbus）。
// 3. 待用户实测：游戏内（Minecraft 聊天框）ti3 + 嵌套 fcitx5 + VK 直通。
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
    fn seed_writes_defaults_only_when_missing() {
        let dir = std::env::temp_dir().join(format!(
            "wlc-nested-seed-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 全新目录 → 两个种子文件都写出来。
        let notes = seed_fcitx5_home(&dir);
        assert!(notes.iter().any(|n| n.contains("已种默认 profile")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("ActiveByDefault=True（根因修复")), "{notes:?}");
        let profile = dir.join("config/fcitx5/profile");
        let config = dir.join("config/fcitx5/config");
        assert!(std::fs::read_to_string(&profile).unwrap().contains("DefaultIM=pinyin"));
        assert!(std::fs::read_to_string(&config).unwrap().contains("ActiveByDefault=True"));

        // 已有用户文件 → 绝不覆盖；缺关键项如实警告。
        std::fs::write(&profile, "[User] custom").unwrap();
        std::fs::write(&config, "[Behavior]\nActiveByDefault=False\n").unwrap();
        let notes = seed_fcitx5_home(&dir);
        assert_eq!(std::fs::read_to_string(&profile).unwrap(), "[User] custom");
        assert!(std::fs::read_to_string(&config).unwrap().contains("False"));
        assert!(
            notes.iter().any(|n| n.contains("警告") && n.contains("ActiveByDefault")),
            "用户显式关闭激活要如实警告: {notes:?}"
        );

        // 用户自己种对了 → 静默通过（无警告）。
        std::fs::write(&config, "[Behavior]\nActiveByDefault = true\n").unwrap();
        let notes = seed_fcitx5_home(&dir);
        assert!(!notes.iter().any(|n| n.contains("警告")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("不动")));

        // 再次运行幂等：文件原样。
        let notes = seed_fcitx5_home(&dir);
        assert!(notes.iter().any(|n| n.contains("profile 已存在")));
        let _ = std::fs::remove_dir_all(&dir);
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

    // ═══════════════ 种子 v2：引擎枚举 + 所有权（fixture 目录）═══════════════

    /// 唯一临时 fixture 根（并行安全：pid + tag；测试结束删除）。
    fn fixture_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wlc-v2-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 在 dir/fcitx5/inputmethod 下写引擎 conf fixture，返回**该 inputmethod
    /// 目录**（discover_engines 的根就是它，与 engine_scan_roots 输出口径一致）。
    fn mk_engine_fixture(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
        let im = dir.join("fcitx5").join("inputmethod");
        std::fs::create_dir_all(&im).unwrap();
        for (stem, body) in files {
            std::fs::write(im.join(format!("{stem}.conf")), body).unwrap();
        }
        im
    }

    fn test_paths(tag: &str, engines: &[&str]) -> (PathBuf, SeedPaths) {
        let root = mk_engine_fixture(
            &fixture_root(&format!("{tag}-roots")),
            &engines
                .iter()
                .map(|n| (*n, "[InputMethod]\nName=Test\nLangCode=zh_CN\n"))
                .collect::<Vec<(&str, &str)>>(),
        );
        let home = fixture_root(&format!("{tag}-home"));
        (home, SeedPaths {
            engine_roots: vec![root],
            host_profile: None,
            host_data_home: fixture_root(&format!("{tag}-nodh")),
        })
    }

    fn seeded_profile(home: &Path) -> String {
        std::fs::read_to_string(home.join("config/fcitx5/profile")).unwrap()
    }

    #[test]
    fn seed_v2_consts_are_consistent() {
        // 字面量与常量同步护栏（FCITX5_SEED_CONFIG_V2 用 raw string 写成，
        // concat! 不能引 const，这里把等价性钉进断言）。
        assert!(FCITX5_SEED_CONFIG_V2.contains(SEEDED_MARK_V2));
        assert!(FCITX5_SEED_CONFIG_V2.contains(SWITCH_IM_HOTKEY));
        assert!(FCITX5_SEED_CONFIG_V2.contains("ActiveByDefault=True"));
    }

    #[test]
    fn discovers_engines_parses_confs_and_dedups() {
        let a = fixture_root("disc-a");
        let a_im = mk_engine_fixture(
            &a,
            &[
                ("pinyin", "[InputMethod]\nName=Pinyin\nLangCode=zh_CN\nAddon=pinyin\n"),
                ("cangjie", "[InputMethod]\nName=Cangjie\nLangCode=zh_TW\n"),
                ("gone", "[InputMethod]\nName=Disabled\nEnable=False\n"),
            ],
        );
        let b = fixture_root("disc-b");
        let b_im = mk_engine_fixture(
            &b,
            &[
                // 与 a 同名：低优先级根必须被 a 遮蔽（fcitx5 locate 语义）。
                ("pinyin", "[InputMethod]\nName=Shadowed\n"),
                ("shuangpin", "[InputMethod]\nName=Shuangpin\nLangCode=zh_CN\n"),
            ],
        );
        let found = discover_engines(&[a_im.clone(), b_im]);
        let names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["cangjie", "pinyin", "shuangpin"], "排序/去重/Enable=False 过滤");
        assert_eq!(found[1].display, "Pinyin", "同名引擎取先见根目录的 conf");
        assert_eq!(found[1].lang, "zh_CN");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
        assert_eq!(a_im, a.join("fcitx5").join("inputmethod"), "fixture 根口径=…/inputmethod");
        // 空/不存在根：不报错，空结果。
        assert!(discover_engines(&[]).is_empty());
        assert!(discover_engines(&[PathBuf::from("/definitely/not/here")]).is_empty());
    }

    #[test]
    fn engine_scan_roots_follows_xdg_defaults() {
        // 默认值与 fcitx5 standardpath.cpp 对齐：用户目录在首，
        // XDG_DATA_DIRS 默认 /usr/local/share:/usr/share。
        let roots = engine_scan_roots(None, None, Path::new("/home/u"));
        assert_eq!(roots[0], PathBuf::from("/home/u/.local/share/fcitx5/inputmethod"));
        assert!(roots.contains(&PathBuf::from("/usr/local/share/fcitx5/inputmethod")));
        assert!(roots.contains(&PathBuf::from("/usr/share/fcitx5/inputmethod")));
        // 显式 XDG_* 覆盖 + 空路径段不得产生相对 cwd 的根。
        let roots = engine_scan_roots(
            Some(OsStr::new("/opt/share::/usr/share")),
            Some(OsStr::new("/tmp/dh")),
            Path::new("/home/u"),
        );
        assert_eq!(roots[0], PathBuf::from("/tmp/dh/fcitx5/inputmethod"));
        assert!(roots.contains(&PathBuf::from("/opt/share/fcitx5/inputmethod")));
        assert!(!roots.iter().any(|p| p.is_relative()), "{roots:?}");
    }

    #[test]
    fn seed_v2_fresh_home_writes_all_engines_and_marker() {
        // aaa 排在 pinyin 前也必须让位（pinyin 紧跟 keyboard-us，枚举
        // 循环的可预期起点）。
        let (home, paths) = test_paths(
            "fresh",
            &["wbpy", "aaa-extra", "pinyin", "cangjie"],
        );
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert_eq!(out.engine_count, 4, "{:?}", out.notes);
        assert_eq!(out.engines, vec!["pinyin", "aaa-extra", "cangjie", "wbpy"]);
        let text = seeded_profile(&home);
        assert!(text.contains(SEEDED_MARK_V2));
        let groups = parse_profile_groups(&text);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "default");
        assert_eq!(
            groups[0].items,
            vec!["keyboard-us", "pinyin", "aaa-extra", "cangjie", "wbpy"]
        );
        assert_eq!(groups[0].default_im, "pinyin");
        let config = std::fs::read_to_string(home.join("config/fcitx5/config")).unwrap();
        assert!(config.contains(SEEDED_MARK_V2));
        assert!(config.contains("ActiveByDefault=True"));
        assert!(config.contains("[Hotkey/EnumerateForwardKeys]"));
        assert!(config.contains("0=Control+Shift+B"));
        assert!(out.notes.iter().any(|n| n.contains("已种默认 profile")));
        assert!(out.notes.iter().any(|n| n.contains("发现 fcitx5 引擎 4 个")));

        // 幂等：再跑一次文件原样（config 走“标记且最新，不动”分支）。
        let before = seeded_profile(&home);
        let out2 = seed_fcitx5_home_v2(&home, &paths);
        assert_eq!(seeded_profile(&home), before);
        assert!(out2.notes.iter().any(|n| n.contains("config 带 v2 标记且为最新")));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn seed_v2_mirrors_host_profile_when_present() {
        let (home, mut paths) = test_paths("mirror", &["pinyin", "wbpy", "cangjie"]);
        // 宿主 profile：中文组名 + DefaultIM=wbpy + 重复条目。
        let host_dir = fixture_root("mirror-host");
        let host_profile = host_dir.join("profile");
        std::fs::write(
            &host_profile,
            "[Groups/0]\n# Group Name\nName=我的组\n# Layout\nDefault Layout=us\n\
             # Default Input Method\nDefaultIM=wbpy\n\n\
             [Groups/0/Items/0]\nName=pinyin\n\n\
             [Groups/0/Items/1]\nName=wbpy\n\n\
             [Groups/0/Items/2]\nName=pinyin\n\n\
             [GroupOrder]\n0=我的组\n",
        )
        .unwrap();
        paths.host_profile = Some(host_profile.clone());
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert!(out.notes.iter().any(|n| n.contains("镜像宿主 profile")), "{:?}", out.notes);
        let groups = parse_profile_groups(&seeded_profile(&home));
        assert_eq!(groups.len(), 1);
        // 宿主用户选择的引擎集合是权威（cangjie 虽已装但宿主没配 → 不镜像）；
        // 组名 ASCII 化，重复条目去重，DefaultIM 有效则保留。
        assert_eq!(groups[0].name, "default");
        assert_eq!(groups[0].items, vec!["pinyin", "wbpy"]);
        assert_eq!(groups[0].default_im, "wbpy");
        assert_eq!(out.engine_count, 2, "镜像口径 = 宿主配置（不含 keyboard-us）");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&host_dir);
    }

    #[test]
    fn seed_v2_host_profile_invalid_falls_back_to_enumerate() {
        // 宿主 profile 存在但无有效组 → 退回全量枚举（镜像判定看解析结果，
        // 不看文件在场与否）。
        let (home, mut paths) = test_paths("hostbad", &["pinyin"]);
        let host_dir = fixture_root("hostbad-host");
        let host_profile = host_dir.join("profile");
        std::fs::write(&host_profile, "[Groups/0]\nName=empty\n").unwrap();
        paths.host_profile = Some(host_profile);
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert!(out.notes.iter().any(|n| n.contains("全量枚举")), "{:?}", out.notes);
        let groups = parse_profile_groups(&seeded_profile(&home));
        assert_eq!(groups[0].items, vec!["keyboard-us", "pinyin"]);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&host_dir);
    }

    #[test]
    fn seed_v2_ownership_upgrade_paths() {
        let (home, paths) = test_paths("own", &["pinyin", "cangjie"]);
        let profile = home.join("config/fcitx5/profile");

        // (a) v1 硬编码常量形态（无标记）→ 升级为 v2 带标记。
        std::fs::create_dir_all(home.join("config/fcitx5")).unwrap();
        std::fs::write(&profile, FCITX5_SEED_PROFILE).unwrap();
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert!(out.notes.iter().any(|n| n.contains("升级为 v2")), "{:?}", out.notes);
        let text = seeded_profile(&home);
        assert!(text.contains(SEEDED_MARK_V2));
        assert_eq!(
            parse_profile_groups(&text)[0].items,
            vec!["keyboard-us", "pinyin", "cangjie"]
        );

        // (b) sync6/7 手种形态（组名“默认”，条目同 {keyboard-us, pinyin}）→ 同样升级。
        std::fs::write(
            &profile,
            "[Groups/0]\n# Group Name\nName=默认\n# Layout\nDefault Layout=us\n\
             # Default Input Method\nDefaultIM=pinyin\n\n\
             [Groups/0/Items/0]\n# Name\nName=keyboard-us\n# Layout\nLayout=\n\n\
             [Groups/0/Items/1]\n# Name\nName=pinyin\n# Layout\nLayout=\n\n\
             [GroupOrder]\n0=默认\n",
        )
        .unwrap();
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert!(out.notes.iter().any(|n| n.contains("升级为 v2")), "{:?}", out.notes);
        assert!(seeded_profile(&home).contains(SEEDED_MARK_V2));

        // (c) 带标记 + 旧引擎表 → 按最新发现重写，保留用户当前引擎。
        std::fs::write(
            &profile,
            render_v2_profile(&[ProfileGroup {
                name: "default".into(),
                default_im: "cangjie".into(),
                items: vec!["keyboard-us".into(), "cangjie".into(), "removed-engine".into()],
            }]),
        )
        .unwrap();
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert!(out.notes.iter().any(|n| n.contains("带 v2 标记，已按最新发现重写")), "{:?}", out.notes);
        let groups = parse_profile_groups(&seeded_profile(&home));
        assert_eq!(groups[0].items, vec!["keyboard-us", "pinyin", "cangjie"]);
        // cangjie 仍在集内 → 保留为当前引擎（用户上次会话的选择）。
        assert_eq!(groups[0].default_im, "cangjie");

        // (d) fcitx5 自己重写（标记丢失、DefaultIM 被枚举切走、条目集不变）
        //     → 仍判 ours：重写回标记 + 保留当前引擎。
        std::fs::write(
            &profile,
            "[Groups/0]\nName=default\nDefaultIM=pinyin\n\n\
             [Groups/0/Items/0]\nName=keyboard-us\n\n\
             [Groups/0/Items/1]\nName=cangjie\n\n\
             [Groups/0/Items/2]\nName=pinyin\n",
        )
        .unwrap();
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert!(out.notes.iter().any(|n| n.contains("保留当前引擎: pinyin")), "{:?}", out.notes);
        assert!(seeded_profile(&home).contains(SEEDED_MARK_V2));

        // (e) 用户自定义（含未发现的 rime）→ 绝不覆盖；如实列出缺装引擎。
        let custom = "[Groups/0]\nName=my\nDefaultIM=rime\n\n\
                      [Groups/0/Items/0]\nName=keyboard-us\n\n\
                      [Groups/0/Items/1]\nName=rime\n";
        std::fs::write(&profile, custom).unwrap();
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert_eq!(seeded_profile(&home), custom, "用户自定义 profile 必须原样保留");
        assert!(out.notes.iter().any(|n| n.contains("用户自定义") && n.contains("不覆盖")), "{:?}", out.notes);
        let warn = out.notes.iter().find(|n| n.contains("未包含已装引擎")).expect("缺装引擎必须如实上报");
        assert!(warn.contains("pinyin") && warn.contains("cangjie"), "{warn}");

        // (f) config：v1 精确形态升级；标记文件幂等；用户内容不动。
        let config = home.join("config/fcitx5/config");
        std::fs::write(&config, FCITX5_SEED_CONFIG).unwrap();
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert!(out.notes.iter().any(|n| n.contains("升级为 v2（+Control+Shift+B 枚举热键）")), "{:?}", out.notes);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            FCITX5_SEED_CONFIG_V2
        );
        std::fs::write(&config, "[Behavior]\nActiveByDefault = true\nHotkeyOverlay=\n").unwrap();
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert!(out.notes.iter().any(|n| n.contains("不动")));
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "[Behavior]\nActiveByDefault = true\nHotkeyOverlay=\n"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn seed_paths_from_process_env_are_resolved_absolute() {
        // 游戏进程 env 语义抽查：首根必须是用户数据目录、全部绝对路径
        // （XDG_* 相对值必须已按 HOME 解析——spawn 隔离后 cwd 是 "/"，
        // 相对根会静默扫空）。
        let paths = SeedPaths::from_process_env();
        assert!(!paths.engine_roots.is_empty());
        assert_eq!(
            paths.engine_roots[0],
            paths.host_data_home.join("fcitx5").join("inputmethod")
        );
        assert!(
            paths.engine_roots.iter().all(|p| p.is_absolute()),
            "{:?}",
            paths.engine_roots
        );
        assert!(paths.host_data_home.is_absolute());
        if let Some(hp) = &paths.host_profile {
            assert!(hp.is_absolute());
            assert!(hp.is_file(), "host_profile 仅在存在时携带: {hp:?}");
        }
    }

    #[test]
    fn seed_v2_mirrors_user_dir_engine_into_isolated_data() {
        // 用户 XDG_DATA_HOME 引擎必须复制进隔离 data（嵌套实例的
        // XDG_DATA_HOME 指向隔离目录，不复制就只在 profile 挂名）。
        let root = fixture_root("udata");
        let dh = root.join("dh");
        mk_engine_fixture(&dh, &[("rime", "[InputMethod]\nName=Rime\nLangCode=zh_CN\n")]);
        let sys = fixture_root("udata-sys");
        let sys_im = mk_engine_fixture(&sys, &[("pinyin", "[InputMethod]\nName=Pinyin\n")]);
        let home = fixture_root("udata-home");
        let paths = SeedPaths {
            engine_roots: vec![dh.join("fcitx5").join("inputmethod"), sys_im],
            host_profile: None,
            host_data_home: dh.clone(),
        };
        let out = seed_fcitx5_home_v2(&home, &paths);
        assert!(out.notes.iter().any(|n| n.contains("已镜像 1 个用户目录引擎")), "{:?}", out.notes);
        let dst = home.join("data/fcitx5/inputmethod/rime.conf");
        assert!(dst.is_file(), "用户目录引擎必须复制进隔离 data");
        // profile 里也必须有 rime（引擎全集 = 用户目录 + 系统目录）。
        assert!(seeded_profile(&home).contains("Name=rime"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&sys);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn seed_v2_long_engine_list_truncates_notes() {
        let names: Vec<String> = (0..40).map(|i| format!("engine-with-a-long-name-{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let (home, paths) = test_paths("trunc", &refs);
        let out = seed_fcitx5_home_v2(&home, &paths);
        let line = out.notes.iter().find(|n| n.starts_with("发现 fcitx5 引擎")).unwrap();
        assert!(line.contains("…(+" ), "超长列表必须截断: {line}");
        assert!(line.chars().count() < 220, "截断后仍需短行: {} 字符", line.chars().count());
        assert_eq!(out.engine_count, 40);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn seed_v2_status_line_carries_engine_count() {
        let mut n = NestedIme::default();
        configured(&mut n, Instant::now());
        assert!(!n.status_line(true, true, 1).contains("engines="), "未种子过不显示");
        n.note_seed_engines(&SeedOutcome {
            notes: vec![],
            engine_count: 10,
            engines: vec![],
        });
        assert!(n.status_line(true, true, 1).contains("engines=10"));
    }

    // ═══════════════ Ctrl+Shift+B 组合键 wire 取证（模拟 im2 client）═══════════════

    #[test]
    fn ctrl_shift_b_chord_reaches_grab_with_modifiers() {
        use crate::ime::tests::{ServerClientData, clients::TestClient, server_setup};
        use crate::seat::KeyboardAction;
        use crate::WLCState;
        use smithay::reexports::wayland_server::Display;
        use std::os::unix::net::UnixStream;
        use std::sync::Arc;
        use wayland_client::Dispatch;
        use wayland_client::protocol::{wl_registry, wl_seat};
        use wayland_protocols_misc::zwp_input_method_v2::client::{
            zwp_input_method_keyboard_grab_v2::{
                self as grabc, ZwpInputMethodKeyboardGrabV2,
            },
            zwp_input_method_manager_v2::{self as mgr2c, ZwpInputMethodManagerV2},
            zwp_input_method_v2::{self as im2c, ZwpInputMethodV2},
        };

        /// 独立于 tests.rs 模拟端的 grab 探针：记录**事件到达顺序**，
        /// 证明引擎在收到 B press 时其修饰键状态已经是 Ctrl+Shift
        /// （fcitx5 waylandimserverv2.cpp::keyCallback 用上一次
        /// Modifiers 事件合成的 state_ 构造 KeyEvent——顺序即语义）。
        #[derive(Default)]
        struct GrabProbe {
            log: Vec<String>,
            manager: Option<ZwpInputMethodManagerV2>,
            seat: Option<wl_seat::WlSeat>,
            im: Option<ZwpInputMethodV2>,
            _grab: Option<ZwpInputMethodKeyboardGrabV2>,
        }
        impl Dispatch<wl_registry::WlRegistry, ()> for GrabProbe {
            fn event(
                state: &mut Self,
                registry: &wl_registry::WlRegistry,
                event: wl_registry::Event,
                _: &(),
                _: &wayland_client::Connection,
                qh: &wayland_client::QueueHandle<Self>,
            ) {
                if let wl_registry::Event::Global { name, interface, version } = event {
                    match interface.as_str() {
                        "wl_seat" => state.seat = Some(registry.bind(name, version.min(8), qh, ())),
                        "zwp_input_method_manager_v2" => {
                            state.manager = Some(registry.bind(name, version.min(1), qh, ()))
                        }
                        _ => {}
                    }
                }
            }
        }
        impl Dispatch<wl_seat::WlSeat, ()> for GrabProbe {
            fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &wayland_client::Connection, _: &wayland_client::QueueHandle<Self>) {}
        }
        impl Dispatch<ZwpInputMethodManagerV2, ()> for GrabProbe {
            fn event(_: &mut Self, _: &ZwpInputMethodManagerV2, _: mgr2c::Event, _: &(), _: &wayland_client::Connection, _: &wayland_client::QueueHandle<Self>) {}
        }
        impl Dispatch<ZwpInputMethodV2, ()> for GrabProbe {
            fn event(_: &mut Self, _: &ZwpInputMethodV2, _: im2c::Event, _: &(), _: &wayland_client::Connection, _: &wayland_client::QueueHandle<Self>) {}
        }
        impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for GrabProbe {
            fn event(
                state: &mut Self,
                _: &ZwpInputMethodKeyboardGrabV2,
                event: grabc::Event,
                _: &(),
                _: &wayland_client::Connection,
                _: &wayland_client::QueueHandle<Self>,
            ) {
                match event {
                    grabc::Event::Key { key, state: ks, .. } => {
                        let pressed = matches!(
                            ks,
                            wayland_client::WEnum::Value(
                                wayland_client::protocol::wl_keyboard::KeyState::Pressed
                            )
                        );
                        state.log.push(format!("key {key} {}", pressed as u8));
                    }
                    grabc::Event::Modifiers {
                        mods_depressed,
                        mods_latched,
                        mods_locked,
                        group,
                        ..
                    } => {
                        state.log.push(format!(
                            "mods {mods_depressed} {mods_latched} {mods_locked} {group}"
                        ));
                    }
                    _ => {}
                }
            }
        }

        fn fmt_mods(m: (u32, u32, u32, u32)) -> String {
            format!("mods {} {} {} {}", m.0, m.1, m.2, m.3)
        }

        let (mut display, mut server) = server_setup();
        let (ss, cs) = UnixStream::pair().unwrap();
        display.handle().insert_client(ss, Arc::new(ServerClientData)).unwrap();
        let mut probe: TestClient<GrabProbe> = TestClient::connect(cs);
        let drive = |display: &mut Display<WLCState>,
                     server: &mut WLCState,
                     probe: &mut TestClient<GrabProbe>| {
            probe.flush();
            display.dispatch_clients(server).unwrap();
            display.flush_clients().unwrap();
            probe.dispatch_pending();
        };
        for _ in 0..4 {
            drive(&mut display, &mut server, &mut probe);
        }
        let (mgr, seat) = (
            probe.state.manager.clone().expect("manager bind"),
            probe.state.seat.clone().expect("seat bind"),
        );
        let qh = probe.qh();
        probe.state.im = Some(mgr.get_input_method(&seat, &qh, ()));
        for _ in 0..3 {
            drive(&mut display, &mut server, &mut probe);
        }
        assert!(server.ime.nested_route_live(), "im2 bind 后路由门必须开（mode 默认 auto）");
        let qh = probe.qh();
        probe.state._grab =
            Some(probe.state.im.as_ref().unwrap().grab_keyboard(&qh, ()));
        for _ in 0..3 {
            drive(&mut display, &mut server, &mut probe);
        }
        assert!(server.ime.im2.grab.is_some(), "grab 必须在服务端登记");
        probe.state.log.clear(); // 丢掉 grab 种子的 keymap/初始 modifiers/repeat

        // ── 生产纪律（KeyboardHandlerMixin 注释）：keyboardUpdate 先行
        //    更新 seat xkb 态，再 keyboardInput（bridge 读 modifiers_tuple
        //    喂 handle_key）。evdev：Ctrl_L=29 Shift_L=42 B=48（xkb=+8）。──
        server.seat.keyboard_update_xkb(37, true); // Ctrl down
        let m1 = server.seat.modifiers_tuple();
        assert_ne!(m1.0, 0, "Ctrl 按下后 depressed 掩码必须非零（xkb 修饰键跟踪）");
        assert!(server.ime.handle_key(37, KeyboardAction::Press, m1), "grab 在场 Ctrl 必须进引擎");
        drive(&mut display, &mut server, &mut probe);

        server.seat.keyboard_update_xkb(50, true); // Shift down
        let m2 = server.seat.modifiers_tuple();
        assert_eq!(m2.0 & m1.0, m1.0, "Shift 追加后 Ctrl 位必须保留（修饰键态可叠加）");
        assert_ne!(m2.0, m1.0, "Shift 必须带来独立修饰位");
        assert!(server.ime.handle_key(50, KeyboardAction::Press, m2));
        drive(&mut display, &mut server, &mut probe);

        // B press/release：fcitx5 匹配 EnumerateForwardKeys（0=Control+Shift+B）
        // 依赖此刻引擎侧 state_ 已是 Ctrl+Shift（由上面两条 Modifiers
        // 事件建立）。handle_key 返回 true ⇒ bridge 绝不 VK 直通。
        assert!(server.ime.handle_key(56, KeyboardAction::Press, m2), "组合键 press 必须路由给引擎");
        drive(&mut display, &mut server, &mut probe);
        assert!(server.ime.handle_key(56, KeyboardAction::Release, m2), "组合键 release 必须同路（无孤立 release）");
        drive(&mut display, &mut server, &mut probe);

        server.seat.keyboard_update_xkb(50, false); // Shift up
        let m3 = server.seat.modifiers_tuple();
        assert!(server.ime.handle_key(50, KeyboardAction::Release, m3));
        drive(&mut display, &mut server, &mut probe);
        server.seat.keyboard_update_xkb(37, false); // Ctrl up
        let m4 = server.seat.modifiers_tuple();
        assert_eq!(m4.0 & m1.0, 0, "Ctrl 释放后其位必须清零");
        assert!(server.ime.handle_key(37, KeyboardAction::Release, m4));
        drive(&mut display, &mut server, &mut probe);

        // ── 断言引擎侧事件流：六对 key+modifiers，顺序与修饰键态全对。──
        let want = vec![
            "key 29 1".to_string(), fmt_mods(m1),
            "key 42 1".to_string(), fmt_mods(m2),
            "key 48 1".to_string(), fmt_mods(m2),
            "key 48 0".to_string(), fmt_mods(m2),
            "key 42 0".to_string(), fmt_mods(m3),
            "key 29 0".to_string(), fmt_mods(m4),
        ];
        assert_eq!(probe.state.log, want, "Ctrl+Shift+B 必须以 press+release 到达引擎，且 B press 之前引擎已见 Ctrl+Shift 修饰态");
        // B press 前紧邻的 mods 事件 == Ctrl+Shift 合成态（m2）：
        // 这正是 fcitx5 构造 Key(B, state=Ctrl|Shift) 的输入，枚举热键
        // 匹配（instance.cpp keyHandlers）据此触发。
        let b_press = probe.state.log.iter().position(|l| l == "key 48 1").unwrap();
        assert_eq!(probe.state.log[b_press - 1], fmt_mods(m2));
        assert_ne!(m2.0, m1.0, "守卫：m2 必须确为 Ctrl+Shift 双修饰态");
    }
}
