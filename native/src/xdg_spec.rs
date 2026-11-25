use crate::process::spawn;
use cosmic_freedesktop_icons::lookup;
use freedesktop_desktop_entry::{
    DesktopEntry, desktop_entries, find_app_by_id, get_languages_from_env,
    unicase::Ascii,
};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct XDGSpecHelper {
    locales: Vec<String>,
    entries: Vec<DesktopEntry>,
    // upstream f23fdcb: 首选终端命令（Terminal=true 的 entry 用它包一层 -e 启动）
    preferred_terminal: String,
}

pub struct RawDesktopEntry {
    pub app_id: String,
    pub name: Option<String>,
    pub generic_name: Option<String>,
    pub exec: Option<String>,
    pub exec_terminal: bool,
    pub comment: Option<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    pub visible: bool,
    pub icon_path: Option<String>,
}

impl XDGSpecHelper {
    pub fn init() -> Self {
        let locales = get_languages_from_env();
        let entries = desktop_entries(&locales);

        XDGSpecHelper {
            locales,
            entries,
            preferred_terminal: String::new(),
        }
    }

    // upstream f23fdcb
    pub fn set_preferred_terminal(&mut self, cmd: String) {
        self.preferred_terminal = cmd;
    }

    fn to_raw(&self, entry: &DesktopEntry) -> RawDesktopEntry {
        let icon = self.resolve_icon_path(entry);
        let mut visible = true;
        if entry.hidden() || entry.no_display() {
            visible = false;
        }

        let keywords = entry
            .keywords(&self.locales)
            .unwrap_or_default()
            .iter_mut()
            .map(|k| k.to_mut().clone())
            .collect();

        let categories = entry
            .categories()
            .unwrap_or_default()
            .iter()
            .map(|c| (*c).into())
            .collect();

        RawDesktopEntry {
            app_id: entry.id().into(),
            name: entry.name(&self.locales).map(|c| c.into_owned()),
            generic_name: entry
                .generic_name(&self.locales)
                .map(|c| c.into_owned()),
            exec: entry.exec().map(|s| s.into()),
            exec_terminal: entry.terminal(),
            comment: entry.comment(&self.locales).map(|c| c.into_owned()),
            keywords,
            categories,
            visible,
            icon_path: icon.map(|p| p.into_os_string().into_string().unwrap()),
        }
    }

    pub fn load_entry(&self, path: PathBuf) -> Option<RawDesktopEntry> {
        let entry = DesktopEntry::from_path(path, Some(&self.locales)).ok()?;
        Some(self.to_raw(&entry))
    }

    pub fn get_raw_entries(&self) -> Vec<RawDesktopEntry> {
        self.entries.iter().map(|e| self.to_raw(e)).collect()
    }

    fn resolve_icon_path(&self, entry: &DesktopEntry) -> Option<PathBuf> {
        let icon = entry.icon()?;

        // Absolute icon path
        let abspath = PathBuf::from(icon);
        if abspath.is_absolute() && abspath.is_file() {
            return Some(abspath);
        }

        // Lookup 128x128 icons
        let path = lookup(icon).with_size(128).with_scale(1).find();
        if path.is_some() {
            return path;
        }

        // Lookup 64x64 icons
        let path = lookup(icon).with_size(64).with_scale(1).find();
        if path.is_some() {
            return path;
        }

        // Fallback to any icon paths
        lookup(icon).find()
    }

    pub fn exec_app(
        &self,
        app_id: String,
        env: Vec<(OsString, OsString)>,
    ) -> bool {
        self._exec_app(app_id, env).is_some()
    }

    fn _exec_app(
        &self,
        app_id: String,
        env: Vec<(OsString, OsString)>,
    ) -> Option<()> {
        let entry = find_app_by_id(&self.entries, Ascii::new(&app_id));
        if entry.is_none() {
            eprintln!("[waylandcraft] _exec_app: app_id={:?} NOT FOUND in {} entries", app_id, self.entries.len());
            return None;
        }
        let entry = entry.unwrap();
        let exec = entry.exec();
        if exec.is_none() {
            eprintln!("[waylandcraft] _exec_app: app_id={:?} found but no exec line", app_id);
            return None;
        }
        let exec = exec.unwrap();
        eprintln!("[waylandcraft] _exec_app: app_id={:?}, exec={:?}", app_id, exec);
        let mut args = split_exec(exec).ok()?;

        if args.is_empty() {
            eprintln!("[waylandcraft] _exec_app: args empty after split");
            return None;
        }

        // upstream f23fdcb: Terminal=true 的 entry 用首选终端包一层（-e）直启；
        // 未配置终端时不启动（与上游行为一致）
        if entry.terminal() {
            if self.preferred_terminal.is_empty() {
                eprintln!("[waylandcraft] _exec_app: app_id={:?} requires terminal, none configured", app_id);
                return None;
            }
            args.insert(0, "-e".to_string());
            args.insert(0, self.preferred_terminal.clone());
        }

        let cmd = args.remove(0);
        eprintln!("[waylandcraft] _exec_app: spawning cmd={:?}, args={:?}", cmd, args);

        // 从 env 中提取 WAYLAND_DISPLAY 和 XDG_RUNTIME_DIR
        let wayland_display = env.iter()
            .find(|(k, _)| k == "WAYLAND_DISPLAY")
            .map(|(_, v)| v.to_string_lossy().to_string())
            .unwrap_or_else(|| "wayland-0".to_string());
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| "/run/user/1000".to_string());

        spawn(cmd, args, env, wayland_display, runtime_dir).ok()?;

        Some(())
    }

    /// 检测应用是否可以启动（静态检查，不真正启动进程）。
    /// 返回一个简短状态串：
    ///   "ok"                    — 可启动
    ///   "not-found"             — desktop entry 不存在
    ///   "no-exec"               — 没有 Exec 行
    ///   "empty"                 — Exec 解析后为空
    ///   "missing:<cmd>"         — 可执行文件不在 PATH / 不存在
    ///   "flatpak-missing:<id>"  — flatpak 应用未安装
    pub fn check_app(&self, app_id: String) -> String {
        let entry = find_app_by_id(&self.entries, Ascii::new(&app_id));
        let entry = match entry {
            Some(e) => e,
            None => return "not-found".to_string(),
        };
        let exec = match entry.exec() {
            Some(e) => e,
            None => return "no-exec".to_string(),
        };
        let mut args = match split_exec(exec) {
            Ok(a) => a,
            Err(_) => return "no-exec".to_string(),
        };
        if args.is_empty() {
            return "empty".to_string();
        }
        let cmd = args.remove(0);

        // flatpak: 可执行文件是 flatpak 运行器，检查目标应用是否已安装
        let cmd_lower = cmd.to_lowercase();
        if cmd_lower.ends_with("/flatpak") || cmd_lower == "flatpak" {
            // args 形如: run [options] <app-id>
            let mut app_id_arg: Option<String> = None;
            let mut after_run = false;
            for a in &args {
                if a == "run" {
                    after_run = true;
                    continue;
                }
                if after_run && !a.starts_with('-') {
                    app_id_arg = Some(a.clone());
                    break;
                }
            }
            if let Some(app) = app_id_arg {
                let installed = std::process::Command::new(&cmd)
                    .arg("info")
                    .arg(&app)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !installed {
                    return format!("flatpak-missing:{}", app);
                }
                return "ok".to_string();
            }
            // 找不到 app-id 参数，退化为检查 flatpak 运行器本身
            if find_in_path(&cmd).is_some() {
                return "ok".to_string();
            }
            return format!("missing:{}", cmd);
        }

        if find_in_path(&cmd).is_some() {
            "ok".to_string()
        } else {
            format!("missing:{}", cmd)
        }
    }
}

/// 在 PATH 中查找可执行文件；cmd 含路径分隔符时直接检查该路径。
fn find_in_path(cmd: &str) -> Option<PathBuf> {
    let candidate = Path::new(cmd);
    if candidate.components().count() > 1 || cmd.starts_with('/') {
        if is_executable(candidate) {
            return Some(candidate.to_path_buf());
        }
        return None;
    }
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in path_var.split(':') {
        if dir.is_empty() {
            continue;
        }
        let p = PathBuf::from(dir).join(cmd);
        if is_executable(&p) {
            return Some(p);
        }
    }
    None
}

fn is_executable(p: &Path) -> bool {
    match p.metadata() {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

fn split_exec(exec: &str) -> Result<Vec<String>, ()> {
    let parts = shlex::split(exec).ok_or(())?;
    let mut uparts = vec![];
    for part in parts {
        if let Some(p) = unpercent(&part)? {
            uparts.push(p);
        }
    }
    Ok(uparts)
}

// Undo percent sign escaping and check for field codes
// This implementation is strict. A properly formatted argument may either be
// a single field code by itself or a normal argument with properly escaped
// percent signs.
fn unpercent(arg: &str) -> Result<Option<String>, ()> {
    // Check if argument is a field code like %f
    let mut iter = arg.chars();
    if arg.len() >= 2
        && iter.next() == Some('%')
        && iter.next().is_some_and(|c| c.is_ascii_alphabetic())
    {
        if arg.len() > 2 {
            // Not a normal field code if longer than a single percent sign
            // and single ascii alphabetic character.
            return Err(());
        }
        // Correct field code
        return Ok(None);
    }

    // Field codes handled, now assemble output while checking for properly
    // escaped percent signs.
    let mut output = String::new();
    let mut iter = arg.chars();
    while let Some(c) = iter.next() {
        if c == '%' {
            // Next character has to follow, otherwise error
            let c2 = match iter.next() {
                Some(a) => a,
                None => return Err(()),
            };
            if c2 == '%' {
                output.push('%');
            } else {
                return Err(());
            }
        } else {
            // Normal character
            output.push(c);
        }
    }

    Ok(Some(output))
}
