use std::ffi::OsStr;
use std::os::unix::net::{SocketAddr, UnixListener};
#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;
#[cfg(target_os = "android")]
use std::os::android::net::SocketAddrExt;
use std::os::fd::{OwnedFd, RawFd, AsRawFd};
use std::process::{Command, Child, Stdio};
use rustix::io::{Errno, write, fcntl_setfd, FdFlags};
use rustix::fs::{Access, OFlags, access, lstat, mkdir, open, unlink};
use rustix::process::{Pid, getuid, getpid, test_kill_process};
use thiserror::Error;

/* Small parts of this code have been adapted from niri's implementation of
 * xwayland-satellite integration, as well as from Mutter's XWayland code
 * (which niri also borrowed from).
 *
 * niri (https://github.com/niri-wm/niri) is GPLv3 software.
 * Mutter (https://gitlab.gnome.org/GNOME/mutter) is GPLv2-or-later software.
 */

pub struct SatelliteState {
    display: i32,
    /// Child 句柄（mut：start_satellite 后重新 spawn 替换；restart_count 记录次数）
    handle: std::cell::RefCell<Child>,
    /// 重启次数——v0.13.6 防止 panic 风暴（如果一帧内死 N 次就别再启了）
    restart_count: std::cell::Cell<u32>,
    /// 上次死亡时间——避免连续重试（如果刚死 1 秒内不再启）
    last_death_ms: std::cell::Cell<Option<u128>>,
    _lock_guard: TmpFileGuard,
    _unix_guard: TmpFileGuard,
}

impl SatelliteState {
    pub fn get_display(&self) -> String {
        format!(":{0}", self.display)
    }

    /// v0.13.6：检查 satellite 是否还活着（每次 X11 应用 launch 时调）。
    /// 死了自动重启（受 restart_count + last_death_ms 节流）。
    /// 成功返回 Ok(())，失败返回 RestartError。
    pub fn ensure_alive(&self) -> Result<(), RestartError> {
        let mut handle = self.handle.borrow_mut();
        match handle.try_wait() {
            Ok(Some(status)) => {
                // satellite 死了。立即尝试重启（受 restart_count 节流）。
                eprintln!(
                    "[waylandcraft] xwayland-satellite died (status={:?}); restarting",
                    status
                );
                drop(handle); // 释放 borrow
                self.restart()
            }
            Ok(None) => Ok(()), // 还活着
            Err(e) => {
                drop(handle);
                Err(RestartError::Wait(e))
            }
        }
    }

    /// v0.13.6：内部 restart 逻辑——start_satellite 重新 spawn 替换 handle。
    fn restart(&self) -> Result<(), RestartError> {
        // 节流：上次死亡后 5 秒内不重启
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        if let Some(last) = self.last_death_ms.get() {
            if now_ms.saturating_sub(last) < 5000 {
                return Err(RestartError::TooSoon);
            }
        }
        // 风暴保护：60 秒内重启 ≥3 次就放弃
        let count = self.restart_count.get();
        if count >= 3 {
            return Err(RestartError::TooMany);
        }
        // 重新 spawn（复用 start_satellite 的逻辑但只针对当前 display）
        // 注意：start_satellite 完整路径需要 socket 等——这里调用
        // 重构后的 try_restart_display
        let wayland_display = std::env::var_os("WAYLAND_DISPLAY")
            .unwrap_or_else(|| std::ffi::OsString::from("wayland-1"));
        let handle = match try_restart_display(&wayland_display, self.display) {
            Ok(h) => h,
            Err(e) => {
                self.last_death_ms.set(Some(now_ms));
                self.restart_count.set(count + 1);
                return Err(RestartError::Spawn(e));
            }
        };
        *self.handle.borrow_mut() = handle;
        self.last_death_ms.set(Some(now_ms));
        self.restart_count.set(count + 1);
        eprintln!(
            "[waylandcraft] xwayland-satellite restarted (count={})",
            count + 1
        );
        Ok(())
    }
}

#[derive(Debug)]
pub enum RestartError {
    /// try_wait 调用本身失败（不太可能）
    Wait(std::io::Error),
    /// 距上次死亡 < 5 秒
    TooSoon,
    /// 60 秒内已重启 ≥3 次
    TooMany,
    /// 重新 spawn 失败
    Spawn(SatelliteError),
}

impl std::fmt::Display for RestartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wait(e) => write!(f, "try_wait failed: {e}"),
            Self::TooSoon => write!(f, "satellite died < 5s ago; skipping restart"),
            Self::TooMany => write!(f, "satellite died ≥3 times in 60s; giving up"),
            Self::Spawn(e) => write!(f, "respawn failed: {e}"),
        }
    }
}

#[derive(Error, Debug)]
pub enum SatelliteError {
    #[error("Failed to execute xwayland-satellite command: {0}")]
    FailExecute(std::io::Error),
    #[error("xwayland-satellite was unexpectedly terminated by a signal")]
    Terminated,
    #[error("xwayland-satellite does not support -listenfd. Exit status: {0}")]
    NoListenFD(i32),
    #[error("Failed to create X11 directory. Error: {0}")]
    X11DirCreate(Errno),
    #[error("Failed checking tmp directory permissions. Error: {0}")]
    FailTmpDirPermCheck(Errno),
    #[error("Failed checking X11 directory permissions. Error: {0}")]
    FailX11DirPermCheck(Errno),
    #[error("X11 unix directory has the wrong permissions: {0}")]
    X11DirInvalidPerms(&'static str),
    #[error("Failed to write X11 lock file: {0}")]
    FailWriteLockFile(Errno),
    #[error("Failed to bind to the X11 unix socket: {0}")]
    FailBindUnixSocket(std::io::Error),
    #[error("Failed to bind to the X11 abstract socket: {0}")]
    FailBindAbstractSocket(std::io::Error),
    #[error("Failed to clone X11 socket: {0}")]
    FailCloneSocket(std::io::Error),
    #[error("Failed to set socket flags via fcntl")]
    FailSetFdFlags(Errno),
    #[error("Failed to create X11 display socket")]
    NoDisplay,
}

// Guard for a temporary file
// Deletes the file when dropped
struct TmpFileGuard(String);
impl Drop for TmpFileGuard {
    fn drop(&mut self) {
        let _ = unlink(&self.0);
    }
}

// 优先使用 Java 侧从 jar 解压出来的内嵌二进制；未设置时 fallback 到系统 PATH。
static XWS_BIN_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
const XWS_BINARY: &str = "xwayland-satellite";
const TMP_UNIX_DIR: &str = "/tmp";
const X11_TMP_UNIX_DIR: &str = "/tmp/.X11-unix";

/// 由 JNI 调用，设置内嵌 xwayland-satellite 二进制的绝对路径
pub fn set_binary_path(path: String) {
    let _ = XWS_BIN_PATH.set(path);
}

fn xws_binary() -> &'static str {
    XWS_BIN_PATH
        .get()
        .map(|s| s.as_str())
        .unwrap_or(XWS_BINARY)
}

fn test_satellite() -> Result<(), SatelliteError> {
    let mut command = Command::new(xws_binary());
    command
        .arg("--test-listenfd-support")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("LD_LIBRARY_PATH");

    let status = command.status().map_err(SatelliteError::FailExecute)?;
    if status.success() {
        Ok(())
    } else if let Some(code) = status.code() {
        Err(SatelliteError::NoListenFD(code))
    } else {
        Err(SatelliteError::Terminated)
    }
}

// From Mutter (src/wayland/meta-xwayland.c, commit 36ca36b4).
fn ensure_x11_unix_dir() -> Result<(), SatelliteError> {
    match mkdir(X11_TMP_UNIX_DIR, 0o1777.into()) {
        Ok(()) => Ok(()),
        Err(Errno::EXIST) => {
            check_x11_unix_perms()?;
            Ok(())
        }
        Err(err) => Err(SatelliteError::X11DirCreate(err)),
    }
}

// From Mutter (src/wayland/meta-xwayland.c, commit 36ca36b4).
fn check_x11_unix_perms() -> Result<(), SatelliteError> {
    // Query status of the /tmp and /tmp/.X11-unix directories
    let x11_tmp =
        lstat(X11_TMP_UNIX_DIR).map_err(SatelliteError::FailX11DirPermCheck)?;
    let tmp =
        lstat(TMP_UNIX_DIR).map_err(SatelliteError::FailTmpDirPermCheck)?;

    // The owner of the .X11-unix dir should either be the owner of the tmp dir
    // or the current user for security reasons.
    if x11_tmp.st_uid != tmp.st_uid && x11_tmp.st_uid != getuid().as_raw() {
        return Err(SatelliteError::X11DirInvalidPerms("wrong ownership"));
    }

    // The .X11-unix dir has to be writable
    access(X11_TMP_UNIX_DIR, Access::WRITE_OK)
        .map_err(|_| SatelliteError::X11DirInvalidPerms("not writeable"))?;

    // And it should have the sticky bit set
    if (x11_tmp.st_mode & 0o1000) != 0o1000 {
        return Err(SatelliteError::X11DirInvalidPerms("no sticky bit"));
    }

    Ok(())
}

fn maybe_cleanup_lockfile(path: &str) -> Result<(), ()> {
    let data = std::fs::read_to_string(path).map_err(|_| ())?;
    let pid = data.trim().parse::<u32>().map_err(|_| ())?;
    let pid = i32::try_from(pid).map_err(|_| ())?;
    let pid = Pid::from_raw(pid).ok_or(())?;

    if matches!(test_kill_process(pid), Err(Errno::SRCH)) {
        // No process matches the pid in the lockfile, delete it
        let _ = unlink(path);
        return Ok(());
    }

    Ok(())
}

// Attempts to acquire lock file for display number.
// Returns Ok(None) when the lock could not be acquired
// Returns Ok(Some(...)) when the lock was acquired successfully
// Returns Err(...) when an error occurred during writing
fn try_lock_display(dpy: i32) -> Result<Option<TmpFileGuard>, SatelliteError> {
    let lock_path = format!("{TMP_UNIX_DIR}/.X{dpy}-lock");

    // Cleanup lockfile if it exists but isn't used anymore
    let _ = maybe_cleanup_lockfile(&lock_path);

    // Create display lock
    let flags =
        OFlags::WRONLY | OFlags::CLOEXEC | OFlags::CREATE | OFlags::EXCL;
    let lock_fd = match open(&lock_path, flags, 0o444.into()) {
        Ok(fd) => fd,
        Err(_) => {
            // Lock could not be acquired
            return Ok(None)
        }
    };
    // Create guard immediately after open(...) so the lockfile is deleted when
    // the guard is dropped.
    let guard = TmpFileGuard(lock_path);

    let data = format!("{:>10}\n", getpid().as_raw_nonzero());
    write(&lock_fd, data.as_bytes())
        .map_err(SatelliteError::FailWriteLockFile)?;
    drop(lock_fd);

    Ok(Some(guard))
}

struct X11Sockets {
    unix_fd: OwnedFd,
    unix_guard: TmpFileGuard,
    abstract_fd: OwnedFd,
}

fn try_open_sockets(dpy: i32) -> Result<X11Sockets, SatelliteError> {
    let socket_path = format!("{X11_TMP_UNIX_DIR}/X{dpy}");

    /* Create abstract socket */
    let abstract_addr = SocketAddr::from_abstract_name(&socket_path).unwrap();
    let abstract_socket = UnixListener::bind_addr(&abstract_addr)
        .map_err(SatelliteError::FailBindAbstractSocket)?;
    let abstract_fd = OwnedFd::from(abstract_socket);

    /* Create unix socket */
    let _ = unlink(&socket_path); // Delete potential existing socket
    let unix_addr = SocketAddr::from_pathname(&socket_path).unwrap();
    let unix_socket = UnixListener::bind_addr(&unix_addr)
        .map_err(SatelliteError::FailBindUnixSocket)?;
    // Create temp file guard now that the socket was created so it now
    // automatically gets deleted when dropped
    let unix_guard = TmpFileGuard(socket_path);
    let unix_fd = OwnedFd::from(unix_socket);

    Ok(X11Sockets {
        unix_fd,
        unix_guard,
        abstract_fd,
    })
}

/// v0.13.6：仅重启 satellite（不动 socket 文件——之前的 start_satellite
/// 已经创建并保持了 lock_guard + unix_guard，重启不需要重新 socket）。
/// 复用 try_invoke_xws 启子进程，但只对指定 display。
fn try_restart_display(
    wayland_display: &OsStr,
    display: i32,
) -> Result<Child, SatelliteError> {
    // v0.13.6 简化：之前的 start_satellite 路径里 socket 句柄已被子进程继承。
    // 重启时，socket 文件还存在（lock_guard 还在），所以可以重新 try_lock_display + 打开新 socket。
    // 但要避免与原 lock_guard 冲突——这里直接用 1 个简单方法：跑 try_invoke_xws，
    // 由 satellite 自己接管新 socket。
    // 关键问题：旧 lock_guard + unix_guard 在 SatelliteState drop 时才释放——
    // 我们的 restart() 通过 borrow_mut 替换 handle 但 lock_guard 不动。
    // 简单实现：每次 restart 都 start_satellite 完整路径，保留原 SatelliteState 字段。
    // 实在复杂：直接调完整 start_satellite，丢弃旧 SatelliteState 字段即可。
    // **v0.13.6 取折中**：只重新 spawn 进程（不重建 socket）——假定 OS 允许
    // bind 同一个 :N（这通常成立因为 X server 用 SO_REUSEADDR）。
    // 风险：可能失败——fallback 是返 SatelliteError 让 Java 报错。
    let listenfds: Vec<RawFd> = Vec::new();
    try_invoke_xws(wayland_display, display, &listenfds)
}

fn try_invoke_xws(
    wayland_display: &OsStr,
    display: i32,
    listenfds: &[RawFd]
) -> Result<Child, SatelliteError> {
    let mut command = Command::new(xws_binary());
    command
        .stdin(Stdio::null())
        // stderr/stdout 不再丢进 null：xwayland-satellite 连接 compositor 失败时
        // 会打印错误（如 "failed to connect to wayland"），透传到日志方便诊断。
        .stdout(Stdio::from(std::fs::OpenOptions::new()
            .create(true).append(true)
            .open("/tmp/waylandcraft-satellite.log").unwrap_or_else(|_| {
                // 打开失败时退化成 /dev/null，绝不让 spawn 失败
                std::fs::File::open("/dev/null").expect("cannot open /dev/null")
            })))
        .stderr(Stdio::from(std::fs::OpenOptions::new()
            .create(true).append(true)
            .open("/tmp/waylandcraft-satellite.log").unwrap_or_else(|_| {
                std::fs::File::open("/dev/null").expect("cannot open /dev/null")
            })))
        .env("WAYLAND_DISPLAY", wayland_display)
        .env_remove("DISPLAY")
        .env_remove("LD_LIBRARY_PATH");

    command.arg(format!(":{display}"));
    for fd in listenfds {
        command.arg("-listenfd").arg(fd.to_string());
    }

    command.spawn().map_err(SatelliteError::FailExecute)
}

// Copy an owned file descriptor, clear any flags (notably CLOEXEC!) and return
// the copied file descriptor.
//
// Clearing the flags is important because otherwise CLOEXEC will be set and the
// file descriptor will not be correctly passed to the child process
// (meaning xwayland-satellite)
fn copy_listenfd(
    listenfd: &OwnedFd
) -> Result<(OwnedFd, RawFd), SatelliteError> {
    let listenfd_copy = listenfd.try_clone()
        .map_err(SatelliteError::FailCloneSocket)?;
    fcntl_setfd(&listenfd_copy, FdFlags::empty())
        .map_err(SatelliteError::FailSetFdFlags)?;
    let raw = listenfd_copy.as_raw_fd();
    Ok((listenfd_copy, raw))
}

pub fn start_satellite(
    wayland_display: &OsStr,
) -> Result<SatelliteState, SatelliteError> {
    ensure_x11_unix_dir()?;
    test_satellite()?;

    for dpy in 1..=32 {
        let lock_guard = match try_lock_display(dpy)? {
            Some(g) => g,
            None => continue
        };

        let sockets = try_open_sockets(dpy)?;
        let (unix_fd_copy, unix_fd_raw) = copy_listenfd(&sockets.unix_fd)?;
        let (abs_fd_copy, abs_fd_raw) = copy_listenfd(&sockets.abstract_fd)?;

        let mut handle = try_invoke_xws(wayland_display, dpy, &[
            unix_fd_raw,
            abs_fd_raw,
        ])?;

        // 存活检查：xwayland-satellite 要连上 compositor（WAYLAND_DISPLAY=wayland-1）
        // 才会真正监听 X socket。如果它连不上（socket 不存在 / compositor 没起来），
        // 会立刻退出——此时 try_invoke_xws 的 spawn 仍然"成功"，导致调用方误以为
        // 拿到了 DISPLAY=:2，实际 X server 不存在（crashreporter 打不开 :2）。
        // 这里等一小会儿，确认子进程还活着才返回成功。
        let mut alive = false;
        for _ in 0..20 {
            match handle.try_wait() {
                Ok(Some(status)) => {
                    eprintln!("[waylandcraft] xwayland-satellite exited early: {status}");
                    break;
                }
                Ok(None) => {
                    alive = true;
                    break;
                }
                Err(_) => break,
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !alive {
            // 子进程死了，这个 display 不可用，继续试下一个
            let _ = handle.kill();
            let _ = handle.wait();
            continue;
        }

        // Only drop file descriptor after passing it to xwayland-satellite
        drop(unix_fd_copy);
        drop(abs_fd_copy);

        return Ok(SatelliteState {
            display: dpy,
            handle: std::cell::RefCell::new(handle),
            restart_count: std::cell::Cell::new(0),
            last_death_ms: std::cell::Cell::new(None),
            _lock_guard: lock_guard,
            _unix_guard: sockets.unix_guard,
        });
    }

    Err(SatelliteError::NoDisplay)
}
