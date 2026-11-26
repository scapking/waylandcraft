#![allow(non_snake_case)]

use crate::egl::{EGLDisplay, EGLHelper};
use crate::java_types::*;
use crate::seat::KeyboardAction;
use crate::utils::get_time;
use crate::xdg_spec::RawDesktopEntry;
use crate::{WaylandCraft, wlc_init};
use jni::objects::{JByteArray, JIntArray, JLongArray, JObject, JObjectArray, JPrimitiveArray};
use jni::{
    Env, bind_java_type,
    objects::{JClass, JString},
    sys::{jboolean, jbyte, jdouble, jint, jlong},
};
use smithay::{
    backend::allocator::{Buffer, dmabuf::WeakDmabuf},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Resource,
            protocol::{
                wl_buffer::WlBuffer,
                wl_pointer::{Axis, ButtonState},
                wl_surface::WlSurface,
            },
        },
    },
    utils::{Logical, Point, Size},
    wayland::{
        compositor::{
            BufferAssignment, Damage, SubsurfaceCachedState, SurfaceAttributes,
            SurfaceData, TraversalAction, with_states,
            with_states as with_surface_data, with_surface_tree_upward,
        },
        dmabuf::get_dmabuf,
        shell::xdg::{
            PopupSurface, SurfaceCachedState, ToplevelSurface,
            XdgToplevelSurfaceData,
        },
        shm::{self, with_buffer_contents},
        single_pixel_buffer::get_single_pixel_buffer,
        viewporter::{ViewportCachedState, ensure_viewport_valid},
    },
};
use std::ops::DerefMut;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use thiserror::Error;

// ═════════════ 日志分级（单一开关 VERBOSE_LOG）═════════════
//
// 用户诉求：**debug 模式全部日志；发布模式只留少量必要日志**。
//
// 分级规则（完整分级表见 docs/LOGGING.md）：
// * **发布档保留（永远打）**：启动摘要（版本/平台/子系统探测结果）、
//   WARN/ERROR 行、error ring（status::note_error，独立通道不受本门）、
//   IME nested ⑥ 决策行（spawn/bind/主路径激活/失败回退）、会话
//   start/stop 结果、崩溃取证行。缓冲日志的"关键行即时 flush"合同
//   （is_important_line，sync15 性能批）不受分级影响——失败行落盘
//   时机不因门控变差。
// * **verbose 门控（发布档在写入点整体跳过）**：kb-debug 逐键盘键行、
//   popup gate/attach 细节、IME 逐事件（ti3 commit/enable/disable、
//   apply_ti3_outcome 细节、relay set_app_enabled 每事件、宿主桥
//   每键/每信号行）、audio 周期性行（process 计数、watchdog 轮次）、
//   status 采样（metrics 窗口报告）。
//
// 机制：调用点**逐处标注分类**——细节行走带门宏（ime_log_v! 定义于
// 本文件、host_bridge/ime/* 全路径引用；audio_capture.rs audio_log_v!；
// seat.rs 逐键行因需短路"调用点先算"的 mods 序列化改为内联门），
// 里程碑行不动。
// 门控判定发生在 format 之前：发布档下细节行零格式化、零写开销
// （组字热路径每键省 2-6 次字符串构造）。
//
// 开关流转：settings.verboseLogging（dev 环境默认 true）→ Java
// WcLogging（生效档，-Dwc.verbose 系统属性可覆盖）→
// setVerboseLoggingNative 启动与变更时各下发一次。默认 true——
// native 单测/bench 环境（无 Java 下发）保持全开，既有日志断言
// 行为不漂移。
pub(crate) static VERBOSE_LOG: AtomicBool = AtomicBool::new(true);

/// 当前档位（relaxed：切换瞬间的可见性窗口只影响个别日志行，
/// 无正确性依赖——绝不允许业务逻辑读它做判定）。
#[inline]
pub fn verbose_logging() -> bool {
    VERBOSE_LOG.load(Ordering::Relaxed)
}

/// Java 下发生效档（setVerboseLoggingNative）。档位切换行本身是
/// 里程碑级：无条件写 ime.log 并立即 flush，跨层对时可追溯切点。
pub fn set_verbose_logging(v: bool) {
    let prev = VERBOSE_LOG.swap(v, Ordering::Relaxed);
    ime_log_write(&format!(
        "[waylandcraft][logging] 档位切换: verbose {} → {}（发布档只保留里程碑与失败行）",
        prev, v
    ));
    ime_log_flush();
}

/// 细节级 IME 日志带门宏（全 crate 唯一定义点；host_bridge 亦经此
/// 路径转发）。发布档（VERBOSE_LOG=false）在判定处短路——**不
/// format、不写 stderr、不进缓冲**，组字/轮询热路径零开销。
/// 哪些行算细节见本文件顶部注释与 docs/LOGGING.md 分级表。
macro_rules! ime_log_v {
    ($($arg:tt)*) => {{
        if $crate::bridge::verbose_logging() {
            $crate::bridge::ime_log_write(&::std::format!($($arg)*));
        }
    }};
}

pub(crate) use ime_log_v;

/// Rust 侧键盘调试日志文件（Java 通过 setKbLogFile 设置路径）。
/// 所有 [kb-debug] 行同时写 stderr 和这个文件，用户上传即可定位
/// Rust 侧是否真的收到/发出按键（eprintln 只进 stderr，不进 latest.log）。
static KB_LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// ⑥ 统一日志时间戳：本地时 [HH:MM:SS.mmm]（失败回退 epoch 秒）。
/// 没有它，Java latest.log（带时间戳）与 Rust 侧文件（不带）无法对时；
/// IME/按键/音频跨层链路只能靠行序人肉对齐。统一后所有 Rust 日志行可对齐。
pub fn log_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d,
        Err(e) => return format!("[epoch=-{}]", e.duration().as_secs()),
    };
    let secs = dur.as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::localtime_r(&secs, &mut tm) };
    if ok.is_null() {
        return format!("[epoch={}]", dur.as_secs());
    }
    format!("[{:02}:{:02}:{:02}.{:03}]", tm.tm_hour, tm.tm_min, tm.tm_sec, dur.subsec_millis())
}

/// 追加一行到 KB_LOG_FILE（若已设置），失败静默忽略。
pub fn kb_log_write(s: &str) {
    use std::io::Write;
    let mut guard = match KB_LOG_FILE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(f) = guard.as_mut() {
        let _ = writeln!(f, "{} {s}", log_timestamp());
        let _ = f.flush();
    }
}

/// Rust 侧音频捕获日志文件（Java 通过 setAudioLogFile 设置路径）。
/// 全链路：PID → 拓扑枚举 → 节点匹配 → stream 建连 → pw-link → process 回调。
/// 所有 [audio] 行同时写 stderr 和此文件（eprintln 只进 stderr，不进 latest.log）。
static AUDIO_LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// 追加一行到 AUDIO_LOG_FILE（若已设置），失败静默忽略。同时写 stderr。
pub fn audio_log_write(s: &str) {
    eprintln!("{s}");
    use std::io::Write;
    let mut guard = match AUDIO_LOG_FILE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(f) = guard.as_mut() {
        let _ = writeln!(f, "{} {s}", log_timestamp());
        let _ = f.flush();
    }
}

/// Rust 侧系统输入法穿透日志文件（Java 通过 setImeLogFile 设置路径）。
/// 全链路：probe → connect → registry/globals → enter/leave → enable 状态机 →
/// commit/preedit → 错误。所有 [system_ime] 行同时写 stderr 和此文件
/// （eprintln 只进 stderr，不进 latest.log——诊断必须靠这个文件）。
/// sync15（性能修复 C）日志写入器：`BufWriter` 缓冲 + 相邻重复行合并。
///
/// 病灶（用户实测"输入法特别卡"的日志侧贡献）：旧实现**每行一次
/// write(2)+flush**（`writeln!+flush` 在 File 上就是每行一条系统调用），
/// 且 `eprintln!` 未缓冲也逐行一条——组字热路径每键 2-6 行（vk modifiers
/// 恒定刷屏行、引擎直通、ti3 State…），全部发生在渲染线程/事件回调栈上。
/// bench50 基线实测：9.4s 打字场景 220 行 = 218 次 fd write。
///
/// 修复语义：
/// 1. **文件写缓冲**（BufWriter 默认 8KiB）：fd write 次数从"每行 1 次"
///    降到"每 8KiB 1 次 + 心跳 1 次"；
/// 2. **相邻重复行合并**（metrics::RepeatCollapse）：内容相同的连排行只写
///    首行本体，段闭合时补一行 `原行 (xN)` 摘要——诊断信息不丢（首尾可见
///    + 总次数），热刷屏行的 stderr/编码/写入全部省掉；
/// 3. **重要行即时落盘**：WARN/ERROR/IME_STATE 行写后立即 flush（崩溃取证
///    的最后防线），其余行由 10s 心跳（`ime_log_flush`，metrics_tick 驱动）
///    兜底——最坏丢失窗口 = 心跳间隔 + 缓冲余量，且只影响尾部计数行，
///    最后一条本体早已写出。
/// stderr 行为保持"逐行即时"（与旧版一致，launcher 控制台实时性不降级）；
/// 被抑制的重复行**不**再写 stderr（正是省掉的主要成本）。
static IME_LOG_FILE: Mutex<Option<ImeLogWriter>> = Mutex::new(None);

/// 日志 IO 全局计数（relaxed 原子——DBus worker 线程与主循环都会
/// 走 ime_log_write，只做报告不做判定）。
/// - lines/bytes：实际写出的行数（含摘要行，不含被抑制的重复）与文件字节；
/// - write_ops：CountingWriter 观察到的 fd write 次数（缓冲化后 ≪ lines，
///   修复前后对比的核心读数）；
/// - suppressed_repeats：相邻重复行合并抑制次数。
pub(crate) static IME_LOG_IO: crate::ime::metrics::LogIoCounters =
    crate::ime::metrics::LogIoCounters::new();

/// 计数包装：BufWriter 每次真正向 fd 写入都记一次 write_ops——
/// write_ops/lines 比值就是缓冲生效的直接读数（修复前恒 ≈1.0）。
struct CountingWriter(std::fs::File);

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = std::io::Write::write(&mut self.0, buf)?;
        IME_LOG_IO.add_write_op();
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.0)
    }
}

pub(crate) struct ImeLogWriter {
    buf: std::io::BufWriter<Box<dyn std::io::Write + Send>>,
    collapse: crate::ime::metrics::RepeatCollapse,
    /// 生产 writer 恒 true（保留逐行 stderr 的既有诊断合同）；
    /// 单测注入 false 以免污染测试输出。
    stderr: bool,
}

/// 崩溃取证敏感的"必须立刻在文件里"的行（其余行允许心跳窗口）。
#[inline]
fn is_important_line(s: &str) -> bool {
    s.contains("WARN") || s.contains("ERROR") || s.starts_with("IME_STATE")
}

impl ImeLogWriter {
    fn new(file: std::fs::File) -> Self {
        Self {
            buf: std::io::BufWriter::new(Box::new(CountingWriter(file))),
            collapse: crate::ime::metrics::RepeatCollapse::default(),
            stderr: true,
        }
    }

    /// 写一行本体（不含重复判定）：stderr（可选）+ 时间戳入缓冲。
    fn write_line(&mut self, s: &str) {
        use std::io::Write;
        if self.stderr {
            eprintln!("{s}");
        }
        let text = format!("{} {s}
", log_timestamp());
        IME_LOG_IO.add_line(text.len() as u64);
        let _ = self.buf.write_all(text.as_bytes());
        if is_important_line(s) {
            let _ = self.buf.flush();
        }
    }

    /// 闭合未决重复段并冲刷缓冲（心跳/会话边界/测试读档点）。
    fn settle(&mut self) {
        use std::io::Write;
        if let Some((line, n)) = self.collapse.flush() {
            self.write_line(&format!("{line} (x{n})"));
        }
        let _ = self.buf.flush();
    }

    /// feed + 落子的组合（ime_log_write 生产路径与单测共用，杜绝
    /// "生产逻辑测试复刻漂移"）。
    fn ingest(&mut self, s: &str) {
        use crate::ime::metrics::Feed;
        match self.collapse.feed(s) {
            Feed::Absorbed => {
                IME_LOG_IO.add_suppressed();
            }
            Feed::NewLine { closed } => {
                // 旧段若有重复，先补摘要行再写本体。
                if let Some((line, n)) = closed {
                    self.write_line(&format!("{line} (x{n})"));
                }
                self.write_line(s);
            }
        }
    }
}

/// 追加一行到 IME 日志（缓冲 + 相邻重复合并，语义见 [`IME_LOG_FILE`] 文档）。
/// 未安装文件（早期启动/纯单测环境）时退化为逐行 stderr——与旧行为一致。
pub fn ime_log_write(s: &str) {
    let mut guard = match IME_LOG_FILE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    match guard.as_mut() {
        Some(w) => w.ingest(s),
        None => {
            eprintln!("{s}");
            IME_LOG_IO.add_line(s.len() as u64 + 1);
        }
    }
}

/// 心跳/边界落盘：闭合重复段 + 冲刷缓冲。metrics_tick 每 10s 与
/// setImeLogFile 换文件时调用；bench 测试在读取计数前调用。
pub fn ime_log_flush() {
    let mut guard = match IME_LOG_FILE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(w) = guard.as_mut() {
        w.settle();
    }
}

#[cfg(test)]
mod ime_log_tests {
    use super::*;

    #[derive(Clone)]
    struct SharedSink(std::sync::Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn test_writer() -> (ImeLogWriter, std::sync::Arc<Mutex<Vec<u8>>>) {
        let shared = std::sync::Arc::new(Mutex::new(Vec::new()));
        let w = ImeLogWriter {
            // 大缓冲保证测试期间只由 settle 触发落盘（验证"非每行 flush"）。
            buf: std::io::BufWriter::with_capacity(64 * 1024, Box::new(SharedSink(shared.clone()))),
            collapse: crate::ime::metrics::RepeatCollapse::default(),
            stderr: false,
        };
        (w, shared)
    }

    #[test]
    fn ime_log_writer_collapses_repeats_into_summary_line() {
        // sync15 修复 C 核心语义：50 连排重复行 = 1 本体 + 1 摘要 (x50)，
        // 而非 50 次编码 + 50 次缓冲写入 +（旧实现）50 次 fd write。
        let (mut w, shared) = test_writer();
        for _ in 0..50 {
            w.ingest("SPAM identical line");
        }
        w.ingest("different line");
        w.settle();
        let text = String::from_utf8(shared.lock().unwrap().clone()).unwrap();
        assert_eq!(text.matches("SPAM identical line").count(), 2, "{text}");
        assert!(text.contains("SPAM identical line (x50)"), "{text}");
        assert_eq!(text.lines().count(), 3, "SPAM 本体 + 摘要 + different 本体：{text}");
        assert!(w.collapse.pending().is_none(), "settle 后无未闭合段");
    }

    #[test]
    fn important_lines_flush_immediately_others_buffer() {
        // 崩溃取证合同：WARN 行写后必达 sink（fd）；普通行留在缓冲，
        // settle 才落——write_ops 与 lines 解耦的机制本身。
        let (mut w, shared) = test_writer();
        w.ingest("normal line without flush");
        assert!(shared.lock().unwrap().is_empty(), "普通行必须留在 BufWriter 缓冲里");
        w.ingest("[waylandcraft][ime][relay] WARN serial mismatch 文本照常交付");
        assert!(!shared.lock().unwrap().is_empty(), "WARN 行必须即时冲刷落盘");
        let text = String::from_utf8(shared.lock().unwrap().clone()).unwrap();
        assert!(text.contains("normal line without flush"), "冲刷带上此前的缓冲内容");
    }

    #[test]
    fn non_important_lines_do_not_touch_fd_per_line() {
        // 缓冲生效的直接读数：300 行非重要行在 settle 前 0 次 fd 写入
        // （旧实现 = 每行 1 次 write+flush）。
        let (mut w, shared) = test_writer();
        for i in 0..300 {
            w.ingest(&format!("distinct line {i}")); // 内容各异，排除重复合并干扰
        }
        assert!(shared.lock().unwrap().is_empty(), "64KiB 缓冲未写满前不得触达 fd");
        w.settle();
        let text = String::from_utf8(shared.lock().unwrap().clone()).unwrap();
        assert_eq!(text.lines().count(), 300);
    }
}

/// 测试/基准专用：把 File 装进 IME_LOG_FILE（等价 setImeLogFile 的 JNI
/// 路径，但免 JNIEnv）。真机 bench 用它开启文件 IO 以获得 write_ops/bytes
/// 的真实基线（无文件时 write_ops 恒 0，只有 stderr）。
#[cfg(test)]
pub(crate) fn set_ime_log_file_for_test(f: std::fs::File) {
    let _ = IME_LOG_FILE.lock().map(|mut g| *g = Some(ImeLogWriter::new(f)));
}

/// 日志分级门行为单测（注入式）：VERBOSE 开关决定细节行写/跳，
/// 里程碑行与档位切换行任何档位都写。内存 sink 注入 IME_LOG_FILE，
/// 断言按唯一 marker（其他并发测试的行只进 sink 不干扰判定）。
#[cfg(test)]
mod verbose_gate_tests {
    use super::*;

    /// 串行化本模块用例（并发互踩全局档位/全局 writer）；毒锁忽略——
    /// 上一用例断言失败不该级联炸出后续用例。
    static GATE_LOCK: Mutex<()> = Mutex::new(());

    struct MarkerSink(std::sync::Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for MarkerSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// RAII：用例结束（含 panic）恢复全局档位并卸下 sink。
    struct GateGuard {
        prev: bool,
    }

    impl Drop for GateGuard {
        fn drop(&mut self) {
            VERBOSE_LOG.store(self.prev, Ordering::Relaxed);
            let mut g = IME_LOG_FILE.lock().unwrap_or_else(|e| e.into_inner());
            *g = None;
        }
    }

    /// 装一个无 stderr 污染的内存 sink + 档位还原守卫。返回 (sink, guard)。
    fn installed() -> (std::sync::Arc<Mutex<Vec<u8>>>, GateGuard) {
        let shared = std::sync::Arc::new(Mutex::new(Vec::new()));
        let w = ImeLogWriter {
            buf: std::io::BufWriter::with_capacity(64 * 1024, Box::new(MarkerSink(shared.clone()))),
            collapse: crate::ime::metrics::RepeatCollapse::default(),
            stderr: false,
        };
        let mut g = IME_LOG_FILE.lock().unwrap_or_else(|e| e.into_inner());
        *g = Some(w);
        drop(g);
        let guard = GateGuard { prev: VERBOSE_LOG.load(Ordering::Relaxed) };
        (shared, guard)
    }

    fn sink_text(shared: &std::sync::Arc<Mutex<Vec<u8>>>) -> String {
        ime_log_flush();
        String::from_utf8(shared.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn verbose_switch_decides_macro_write_or_skip() {
        let _g = GATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (sink, _guard) = installed();

        // 关档（发布档）：带门宏细节行不落盘……
        set_verbose_logging(false);
        assert!(!verbose_logging());
        ime_log_v!("[wc-gate] detail-line-off");
        // ……非宏路径的细节语义判定同样短路（metrics_tick 同款 inline 门）。
        if verbose_logging() {
            ime_log_write("[wc-gate] inline-detail-off");
        }
        let text = sink_text(&sink);
        assert!(!text.contains("[wc-gate] detail-line-off"), "发布档细节行必须被跳过：{text}");
        assert!(!text.contains("[wc-gate] inline-detail-off"), "发布档内联门细节行必须被跳过：{text}");
        // 里程碑行（不经门直接 ime_log_write）发布档照常写。
        ime_log_write("[wc-gate] milestone-line-off");
        let text = sink_text(&sink);
        assert!(text.contains("[wc-gate] milestone-line-off"), "发布档里程碑行必须保留：{text}");
        // 档位切换行本身是里程碑级：无条件写（跨层对时锚点）。
        assert!(text.contains("[waylandcraft][logging] 档位切换"), "档位切换行必须落盘：{text}");

        // 开档（verbose）：细节行照常写。
        set_verbose_logging(true);
        assert!(verbose_logging());
        ime_log_v!("[wc-gate] detail-line-on");
        let text = sink_text(&sink);
        assert!(text.contains("[wc-gate] detail-line-on"), "verbose 档细节行必须写：{text}");
    }

    #[test]
    fn toggle_roundtrip_restores_caller_value() {
        let _g = GATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 全局默认 true：native 单测/bench 环境（无 Java 下发）全开，
        // 既有日志行为不漂移。
        assert!(VERBOSE_LOG.load(Ordering::Relaxed), "Rust 默认档必须是 verbose=true");
        let prev = verbose_logging();
        set_verbose_logging(!prev);
        assert_eq!(verbose_logging(), !prev);
        set_verbose_logging(prev);
        assert_eq!(verbose_logging(), prev);
    }
}

#[allow(clippy::vec_box)]
pub(crate) struct BridgeState {
    /* Handle collections */
    pub(crate) toplevels: Vec<Box<ToplevelSurface>>,
    pub(crate) popups: Vec<Box<PopupSurface>>,
    pub(crate) surfaces: Vec<Box<WlSurface>>,
    pub(crate) dmabufs: Vec<Box<WeakDmabuf>>,
}

impl BridgeState {
    pub fn new() -> Self {
        BridgeState {
            toplevels: vec![],
            popups: vec![],
            surfaces: vec![],
            dmabufs: vec![],
        }
    }
}

fn jptr_to_ref<T>(ptr: jlong) -> Option<&'static T> {
    let ptr = (ptr as usize) as *const T;
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

fn jptr_to_mut<T>(ptr: jlong) -> Option<&'static mut T> {
    let ptr = (ptr as usize) as *mut T;
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &mut *ptr })
    }
}

bind_java_type! {
    rust_type = WaylandCraftBridge,
    java_type = dev.evvie.waylandcraft.bridge.WaylandCraftBridge,

    type_map {
        WLCSurface => dev.evvie.waylandcraft.bridge.WLCSurface,
        JRawDesktopEntry => dev.evvie.waylandcraft.desktop.RawDesktopEntry,
    },

    methods {
        fn get_or_create_surface(jlong) -> WLCSurface,
    },

    native_methods {
        static extern fn init {
            sig = (glfw_get_proc_address: jlong, egl_display: jlong, wayland_display: jlong) -> jlong,
            fn = init,
        },
        static extern fn update {
            sig = (instance: jlong),
            fn = update,
        },
        static extern fn socket {
            sig = (instance: jlong) -> JString,
            fn = socket,
        },
        static extern fn get_satellite_display {
            sig = (instance: jlong) -> JString,
            fn = get_satellite_display,
        },
        static extern fn send_frame {
            sig = (surface_handle: jlong),
            fn = send_frame,
        },
        static extern fn update_surface_data {
            sig = (instance: jlong, surface: WLCSurface),
            fn = update_surface_data,
        },
        static extern fn toplevels {
            sig = (instance: jlong) -> jlong[],
            fn = toplevels,
        },
        static extern fn toplevel_surface {
            sig = (instance: jlong, toplevel_handle: jlong) -> jlong,
            fn = toplevel_surface,
        },
        static extern fn toplevel_title {
            sig = (toplevel_handle: jlong) -> JString,
            fn = toplevel_title,
        },
        static extern fn toplevel_app_id {
            sig = (toplevel_handle: jlong) -> JString,
            name = "toplevelAppID",
            fn = toplevel_app_id,
        },
        static extern fn toplevel_pid {
            sig = (instance: jlong, toplevel_handle: jlong) -> jint,
            name = "toplevelPid",
            fn = toplevel_pid,
        },
        static extern fn toplevel_resize {
            sig = (
                toplevel_handle: jlong,
                width: jint,
                height: jint,
                interactive: jboolean
            ),
            fn = toplevel_resize,
        },
        static extern fn toplevel_resize_ovr {
            sig = (toplevel_handle: jlong, width: jint, height: jint),
            fn = toplevel_resize_ovr,
        },
        static extern fn minimize_req {
            sig = (instance: jlong) -> jlong[],
            fn = minimize_req,
        },
        static extern fn maximize_req {
            sig = (instance: jlong) -> jlong[],
            fn = maximize_req,
        },
        static extern fn unmaximize_req {
            sig = (instance: jlong) -> jlong[],
            fn = unmaximize_req,
        },
        static extern fn fullscreen_req {
            sig = (instance: jlong) -> jlong[],
            fn = fullscreen_req,
        },
        static extern fn unfullscreen_req {
            sig = (instance: jlong) -> jlong[],
            fn = unfullscreen_req,
        },
        static extern fn move_request {
            sig = (instance: jlong) -> jint[],
            fn = move_request,
        },
        static extern fn resize_request {
            sig = (instance: jlong) -> jint[],
            fn = resize_request,
        },
        static extern fn fullscreened {
            sig = (instance: jlong) -> jlong[],
            fn = fullscreened,
        },
        static extern fn toplevel_maximize {
            sig = (instance: jlong, toplevel_handle: jlong),
            fn = toplevel_maximize,
        },
        static extern fn toplevel_fullscreen {
            sig = (instance: jlong, toplevel_handle: jlong),
            fn = toplevel_fullscreen,
        },
        static extern fn popups {
            sig = (instance: jlong) -> jlong[],
            fn = popups,
        },
        static extern fn popup_surface {
            sig = (instance: jlong, popup_handle: jlong) -> jlong,
            fn = popup_surface,
        },
        static extern fn popup_parent {
            sig = (instance: jlong, popup_handle: jlong) -> jlong,
            fn = popup_parent,
        },
        static extern fn popup_offset {
            sig = (popup_handle: jlong) -> jint[],
            fn = popup_offset,
        },
        static extern fn surface_xdg_geometry {
            sig = (surface_handle: jlong) -> jint[],
            name = "surfaceXDGGeometry",
            fn = surface_xdg_geometry,
        },
        static extern fn dmabufs {
            sig = (instance: jlong) -> jlong[],
            fn = dmabufs
        },
        extern fn update_surface_tree {
            sig = (instance: jlong, surface: WLCSurface) -> WLCSurface,
            fn = update_surface_tree,
        },
        static extern fn check_input_region {
            sig = (surface_handle: jlong, x: jdouble, y: jdouble) -> jboolean,
            fn = check_input_region,
        },
        static extern fn pointer_motion {
            sig = (instance: jlong, x: jdouble, y: jdouble),
            fn = pointer_motion,
        },
        static extern fn pointer_motion_focus {
            sig = (
                instance: jlong,
                surface_handle: jlong,
                x: jdouble,
                y: jdouble
            ),
            fn = pointer_motion_focus,
        },
        static extern fn pointer_rel_motion {
            sig = (instance: jlong, dx: jdouble, dy: jdouble),
            fn = pointer_rel_motion,
        },
        static extern fn maybe_pointer_lock {
            sig = (instance: jlong, surface_handle: jlong) -> jboolean,
            fn = maybe_pointer_lock,
        },
        static extern fn pointer_unlock {
            sig = (instance: jlong),
            fn = pointer_unlock,
        },
        static extern fn pointer_leave {
            sig = (instance: jlong),
            fn = pointer_leave,
        },
        static extern fn pointer_button {
            sig = (instance: jlong, button: jint, state: jint) -> jint,
            fn = pointer_button,
        },
        static extern fn pointer_axis {
            sig = (instance: jlong, axis: jint, value: jdouble),
            fn = pointer_axis,
        },
        static extern fn cursor_shape {
            sig = (instance: jlong) -> jint,
            fn = cursor_shape,
        },
        static extern fn keyboard_focus {
            sig = (instance: jlong, surface_handle: jlong),
            fn = keyboard_focus,
        },
        static extern fn keyboard_activate {
            sig = (instance: jlong),
            fn = keyboard_activate,
        },
        static extern fn keyboard_deactivate {
            sig = (instance: jlong),
            fn = keyboard_deactivate,
        },
        static extern fn keyboard_input {
            sig = (instance: jlong, scancode: jint, action: jint),
            fn = keyboard_input,
        },
        static extern fn keyboard_update {
            sig = (instance: jlong, scancode: jint, pressed: jboolean),
            fn = keyboard_update,
        },
        static extern fn set_kb_log_file {
            sig = (path: JString),
            name = "setKbLogFileNative",
            fn = set_kb_log_file,
        },
        static extern fn set_audio_log_file {
            sig = (path: JString),
            name = "setAudioLogFileNative",
            fn = set_audio_log_file,
        },
        static extern fn set_ime_log_file {
            sig = (path: JString),
            name = "setImeLogFileNative",
            fn = set_ime_log_file,
        },
        static extern fn run_ime_diagnostic {
            sig = (instance: jlong) -> JString,
            name = "runImeDiagnosticNative",
            fn = run_ime_diagnostic,
        },
        static extern fn native_version {
            sig = () -> JString,
            name = "nativeVersionNative",
            fn = native_version,
        },
        // v0.14 嵌套输入法守护进程：/wl ime nested [status|start|stop]。
        static extern fn ime_source_command {
            sig = (instance: jlong, sub: JString) -> JString,
            name = "imeSourceCommandNative",
            fn = ime_source_command,
        },
        static extern fn set_ime_source {
            sig = (instance: jlong, mode: JString),
            name = "setImeSourceNative",
            fn = set_ime_source,
        },
        static extern fn ime_builtin_candidates {
            sig = (instance: jlong) -> JString,
            name = "imeBuiltinCandidatesNative",
            fn = ime_builtin_candidates,
        },
        static extern fn ime_builtin_select {
            sig = (instance: jlong, page_index: jint) -> JString,
            name = "imeBuiltinSelectNative",
            fn = ime_builtin_select,
        },
        static extern fn ime_builtin_page {
            sig = (instance: jlong, delta: jint) -> jboolean,
            name = "imeBuiltinPageNative",
            fn = ime_builtin_page,
        },
        static extern fn ime_eng_list {
            sig = (instance: jlong) -> JString,
            name = "imeEngListNative",
            fn = ime_eng_list,
        },
        static extern fn ime_nested_command {
            sig = (instance: jlong, sub: JString) -> JString,
            name = "imeNestedCommandNative",
            fn = ime_nested_command,
        },
        // v0.14 嵌套输入法 settings 下发（mode auto|on|off + 隔离 HOME）。
        static extern fn set_ime_nested {
            sig = (instance: jlong, mode: JString, home: JString),
            name = "setImeNestedNative",
            fn = set_ime_nested,
        },
        static extern fn get_status_report {
            sig = (instance: jlong, java_thread: JString) -> JString,
            name = "getStatusReportNative",
            fn = get_status_report,
        },
        static extern fn set_mod_version {
            sig = (version: JString),
            name = "setModVersionNative",
            fn = set_mod_version,
        },
        // 日志分级：Java 下发生效档（settings/dev 环境/-Dwc.verbose 综合），
        // 全局开关不依赖 instance（与 setModVersionNative 同型）。
        static extern fn set_verbose_logging {
            sig = (verbose: jboolean),
            name = "setVerboseLoggingNative",
            fn = set_verbose_logging_jni,
        },
        static extern fn notify_host_focus_gained {
            sig = (instance: jlong),
            name = "notifyHostFocusGainedNative",
            fn = notify_host_focus_gained,
        },
        static extern fn candidate_nav {
            sig = (instance: jlong, action: jint, arg: jint),
            name = "candidateNavNative",
            fn = candidate_nav,
        },
        static extern fn take_lookup_table {
            sig = (instance: jlong) -> JString,
            name = "takeLookupTableNative",
            fn = take_lookup_table,
        },
        // Phase 1 候选窗帧轮询：byte[] = [w i32][h i32][seq u32][visible u8]
        // [argb w*h*4]；**null = 无新状态**（零拷贝廉价路径，见 ime_popup_frame）。
        static extern fn ime_popup_frame {
            sig = (instance: jlong) -> byte[],
            name = "imePopupFrameNative",
            fn = ime_popup_frame,
        },
        // 状态透明度：中文引擎工作态（预编辑流在场）——角标判定原料，纯读。
        static extern fn ime_popup_work_state {
            sig = (instance: jlong) -> jboolean,
            name = "imePopupWorkStateNative",
            fn = ime_popup_work_state,
        },
        // 世界形态候选窗锚定（sync14）：最近一次 ti3 光标矩形 int[4]=[x,y,w,h]
        // （聚焦应用 surface 像素坐标）；null = 应用尚未上报。独立只读通道，
        // 不触碰 ime_popup_frame 的 13 字节头编码合同、不消耗 epoch。
        static extern fn ime_popup_anchor {
            sig = (instance: jlong) -> jint[],
            name = "imePopupAnchorNative",
            fn = ime_popup_anchor,
        },
        // /wl ime info：nested/种子/候选窗统计/工作态 多行报告，纯读。
        static extern fn ime_info {
            sig = (instance: jlong) -> JString,
            name = "imeInfoNative",
            fn = ime_info,
        },
        // Phase 3 候选窗点选/滚轮回传（进帧序列见 seat popup pointer 通道）。
        static extern fn ime_popup_click {
            sig = (instance: jlong, x: jint, y: jint, button: jint, pressed: jboolean),
            name = "imePopupClickNative",
            fn = ime_popup_click,
        },
        static extern fn ime_popup_axis {
            sig = (instance: jlong, x: jint, y: jint, dy: jdouble),
            name = "imePopupAxisNative",
            fn = ime_popup_axis,
        },
        static extern fn update_cursor_rect {
            sig = (instance: jlong, x: jint, y: jint, w: jint, h: jint),
            name = "updateCursorRectNative",
            fn = update_cursor_rect,
        },
        static extern fn audio_capture_status {
            sig = (instance: jlong) -> JString,
            name = "audioCaptureStatusNative",
            fn = audio_capture_status,
        },
        static extern fn output_size {
            sig = (instance: jlong) -> jint[],
            fn = output_size,
        },
        static extern fn output_bounds {
            sig = (instance: jlong) -> jint[],
            fn = output_bounds,
        },
        static extern fn output_resize {
            sig = (instance: jlong, width: jint, height: jint),
            fn = output_resize,
        },
        static extern fn output_set_bounds {
            sig = (instance: jlong, width: jint, height: jint),
            fn = output_set_bounds,
        },
        static extern fn free_surface {
            sig = (instance: jlong, surface_handle: jlong),
            fn = free_surface,
        },
        static extern fn free_toplevel {
            sig = (instance: jlong, toplevel_handle: jlong),
            fn = free_toplevel,
        },
        static extern fn free_popup {
            sig = (instance: jlong, popup_handle: jlong),
            fn = free_popup,
        },
        static extern fn load_desktop_entry {
            sig = (instance: jlong, path: JString) -> JRawDesktopEntry,
            fn = load_desktop_entry,
        },
        static extern fn load_desktop_entries {
            sig = (instance: jlong) -> JRawDesktopEntry[],
            fn = load_desktop_entries,
        },
        static extern fn render_svg {
            sig = (
                path: JString,
                width: jint,
                height: jint,
                buffer_ptr: jlong
            ) -> jboolean,
            name = "renderSVG",
            fn = render_svg,
        },
        static extern fn exec_app {
            sig = (instance: jlong, app_id: JString) -> jboolean,
            fn = exec_app,
        },
        // upstream f23fdcb: 设置首选终端（Terminal=true 的 desktop entry 直启）
        static extern fn set_preferred_terminal {
            sig = (instance: jlong, cmd: JString),
            fn = set_preferred_terminal,
        },
        static extern fn set_satellite_binary {
            sig = (path: JString),
            name = "setSatelliteBinary",
            fn = set_satellite_binary,
        },
        static extern fn check_app {
            sig = (instance: jlong, app_id: JString) -> JString,
            fn = check_app,
        },
        static extern fn set_keymap_default {
            sig = (instance: jlong),
            fn = set_keymap_default,
        },
        static extern fn export_keymap {
            sig = (instance: jlong) -> JString,
            fn = export_keymap,
        },
        static extern fn set_keymap_from_str {
            sig = (instance: jlong, keymap: JString) -> jboolean,
            fn = set_keymap_from_str,
        },
        static extern fn check_dnd_request {
            sig = (instance: jlong) -> jint[],
            fn = check_dnd_request,
        },
        static extern fn check_dnd_active {
            sig = (instance: jlong) -> jboolean,
            fn = check_dnd_active,
        },
        static extern fn dnd_cancel {
            sig = (instance: jlong),
            fn = dnd_cancel,
        },
        static extern fn dnd_drop {
            sig = (instance: jlong),
            fn = dnd_drop,
        },
        static extern fn dnd_motion {
            sig = (
                instance: jlong,
                surface_handle: jlong,
                x: jdouble,
                y: jdouble
            ),
            fn = dnd_motion,
        },
        static extern fn dnd_icon {
            sig = (instance: jlong) -> jlong,
            fn = dnd_icon,
        },

        static extern fn get_desktop_windows {
            sig = (instance: jlong) -> JString[],
            fn = get_desktop_windows,
        },

        static extern fn portal_capture_start {
            sig = (instance: jlong) -> byte[],
            fn = portal_capture_start,
        },
        static extern fn portal_capture_frame {
            sig = (instance: jlong) -> byte[],
            fn = portal_capture_frame,
        },
        static extern fn portal_capture_stop {
            sig = (instance: jlong) -> void,
            fn = portal_capture_stop,
        },

        // macOS ScreenCaptureKit 捕获（docs/CAPTURE_MACOS.md）；非 macos 调用
        // 抛 unsupported，注册表按条目不可 cfg（同 win_capture_* 的处理）。
        static extern fn mac_capture_list {
            sig = (instance: jlong) -> JString[],
            fn = mac_capture_list,
        },
        static extern fn mac_capture_displays {
            sig = (instance: jlong) -> JString[],
            fn = mac_capture_displays,
        },
        static extern fn mac_capture_start_window {
            sig = (instance: jlong, window_id: jint) -> byte[],
            fn = mac_capture_start_window,
        },
        static extern fn mac_capture_start_display {
            sig = (instance: jlong, index: jint) -> byte[],
            fn = mac_capture_start_display,
        },
        static extern fn mac_capture_frame {
            sig = (instance: jlong, since_seq: jlong) -> byte[],
            fn = mac_capture_frame,
        },
        static extern fn mac_capture_stop {
            sig = (instance: jlong) -> void,
            fn = mac_capture_stop,
        },
        // Windows.Graphics.Capture 窗口/桌面捕获（对称 portal_capture_* 三件套 +
        // 目标枚举）。非 Windows 平台注册保留（bind_java_type! 表不支持按条目 cfg），
        // 调用一律抛 win_capture_unsupported——Java 侧只在 Windows 接线。
        // 接入清单见 docs/CAPTURE_WINDOWS.md。
        static extern fn win_capture_targets {
            sig = (instance: jlong) -> JString,
            name = "winCaptureTargets",
            fn = win_capture_targets,
        },
        static extern fn win_capture_start_window {
            sig = (instance: jlong, hwnd: jlong) -> byte[],
            name = "winCaptureStartWindow",
            fn = win_capture_start_window,
        },
        static extern fn win_capture_start_monitor {
            sig = (instance: jlong, index: jint) -> byte[],
            name = "winCaptureStartMonitor",
            fn = win_capture_start_monitor,
        },
        static extern fn win_capture_frame {
            sig = (instance: jlong) -> byte[],
            name = "winCaptureFrame",
            fn = win_capture_frame,
        },
        static extern fn win_capture_stop {
            sig = (instance: jlong) -> JString,
            name = "winCaptureStop",
            fn = win_capture_stop,
        },

        static extern fn audio_capture_start {
            sig = (instance: jlong, pid: jint) -> void,
            fn = audio_capture_start,
        },
        static extern fn audio_capture_poll {
            sig = (instance: jlong) -> byte[],
            fn = audio_capture_poll,
        },
        static extern fn audio_capture_stop {
            sig = (instance: jlong) -> void,
            fn = audio_capture_stop,
        },
    },
}

#[derive(Debug, Error)]
enum BridgeError {
    #[error(transparent)]
    JniError(#[from] jni::errors::Error),
    #[error(transparent)]
    Init(Box<dyn std::error::Error>),
    #[error("{0}")]
    Null(&'static str),
    #[error("audio capture failed: {0}")]
    AudioCapture(String),
    #[error("mac capture failed: {0}")]
    MacCapture(String),
    #[error("Null WLC instance handle given. Function: {0}")]
    NullInstancePtr(&'static str),
    #[error("Null wayland surface handle given. Function: {0}")]
    NullSurfacePtr(&'static str),
    #[error("Null toplevel surface handle given. Function: {0}")]
    NullToplevelPtr(&'static str),
    #[error("Null popup surface handle given. Function: {0}")]
    NullPopupPtr(&'static str),
    #[error("Error converting OS string, was not UTF8")]
    OsStringToUtf8,
    #[error("Unknown pointer button {0} received")]
    UnknownPointerButton(jint),
    #[error("Unknown scroll direction {0} received")]
    UnknownScrollDirection(jint),
    #[error("Unknown keyboard state {0} received")]
    UnknownKeyboardState(jint),
    #[error("Width cannot be below 1")]
    NonPositiveWidth,
    #[error("Height cannot be below 1")]
    NonPositiveHeight,
}

macro_rules! jptr_to_instance {
    ($jptr:expr, $location:literal) => {
        match jptr_to_mut::<WaylandCraft>($jptr) {
            None => Err(BridgeError::NullInstancePtr($location)),
            Some(wlc) => Ok(wlc),
        }
    };
}

macro_rules! jptr_to_wl_surface {
    ($jptr:expr, $location:literal) => {
        match jptr_to_mut::<WlSurface>($jptr) {
            None => Err(BridgeError::NullSurfacePtr($location)),
            Some(wl_surface) => Ok(wl_surface),
        }
    };
}

macro_rules! jptr_to_toplevel {
    ($jptr:expr, $location:literal) => {
        match jptr_to_mut::<ToplevelSurface>($jptr) {
            None => Err(BridgeError::NullToplevelPtr($location)),
            Some(toplevel) => Ok(toplevel),
        }
    };
}

macro_rules! jptr_to_popup {
    ($jptr:expr, $location:literal) => {
        match jptr_to_mut::<PopupSurface>($jptr) {
            None => Err(BridgeError::NullPopupPtr($location)),
            Some(popup) => Ok(popup),
        }
    };
}

fn init<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    glfw_get_proc_address: jlong,
    egl_display: jlong,
    wayland_display: jlong,
) -> Result<jlong, BridgeError> {
    let dpy: EGLDisplay = (egl_display as usize) as EGLDisplay;
    let egl = EGLHelper::new(dpy, glfw_get_proc_address as usize);

    let instance = wlc_init(egl, wayland_display as usize)
        .map_err(BridgeError::Init)?;
    let instance_box = Box::new(instance);
    let ptr = Box::into_raw(instance_box);

    Ok(ptr.addr() as jlong)
}

fn update<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<(), BridgeError> {
    jptr_to_instance!(instance, "update")?.update();

    Ok(())
}

fn socket<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "socket")?;
    let socket = instance
        .state
        .socket
        .to_str()
        .ok_or(BridgeError::OsStringToUtf8)?;

    Ok(JString::new(env, socket)?)
}

fn get_satellite_display<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "getSatelliteDisplay")?;
    // 返回 xwayland-satellite 的 X display（如 ":2"）；未启动 satellite 时返回空串
    let display = instance
        .state
        .satellite
        .as_ref()
        .map(|s| s.get_display())
        .unwrap_or_default();
    Ok(JString::new(env, &display)?)
}

fn send_frame<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    surface_handle: jlong,
) -> Result<(), BridgeError> {
    let surface = jptr_to_wl_surface!(surface_handle, "sendFrame")?;

    with_surface_data(surface, |data| {
        let mut attr_guard = data.cached_state.get::<SurfaceAttributes>();
        let attr = attr_guard.deref_mut().current();
        for c in attr.frame_callbacks.drain(..) {
            c.done(get_time());
        }
    });

    Ok(())
}

// Get or insert an element and return its handle
// P1-3: 表内缺失不再 unwrap panic（JNI 边界 panic 依赖包装层兜底，能不做就不做）——
// 返回 0，调用方 jptr_to_ref/`*` 判空路径会转成明确的 BridgeError（Java 异常）。
fn insert_get_handle<T>(vec: &mut Vec<Box<T>>, elem: &T) -> jlong
where
    T: Clone + PartialEq,
{
    if !vec.iter().any(|b| **b == *elem) {
        vec.push(Box::new(elem.clone()));
    }

    match vec.iter_mut().find(|r| ***r == *elem) {
        Some(ptr) => ((ptr.as_mut() as *mut T) as usize) as jlong,
        None => 0,
    }
}

// Get an element and return its handle
// P1-3: 元素不在表中（并发删除竞态下的合法情形）返回 0 句柄而不是 panic，
// 由上层转成 Java 异常；行为对调用方可区分（0 == 不存在）。
fn get_handle<T>(vec: &[Box<T>], elem: &T) -> jlong
where
    T: Clone + PartialEq,
{
    vec.iter()
        .find(|r| ***r == *elem)
        .map_or(0, |r| (r.as_ref() as *const T) as usize as jlong)
}

// Insert all elements that aren't in the list already
fn insert_all<T>(vec: &mut Vec<Box<T>>, elems: &[T])
where
    T: Clone + PartialEq,
{
    for elem in elems {
        if !vec.iter().any(|b| **b == *elem) {
            vec.push(Box::new(elem.clone()));
        }
    }
}

// Get handles of all elements in the list
fn get_all_handles<T>(vec: &mut [Box<T>]) -> Vec<jlong>
where
    T: Clone + PartialEq,
{
    vec.iter_mut()
        .map(|r| ((&mut **r) as *mut T) as usize as jlong)
        .collect()
}

// Remove element from list and free it
// P1-3: 旧实现**解引用** Java 传来的 handle——陈旧句柄（元素已被之前的调用释放、
// 或 Java 侧传错/传 0）就是 use-after-free UB：不是 panic，catch_unwind 救不了，
// 可能静默读到已释放内存并把野值当 PartialEq 比较。改为直接比较 Box 的指针地址：
// 这些资源类型（WlSurface/ToplevelSurface/...）的 PartialEq 是同一性比较，
// "与 *elem 不等价于地址不匹配"在语义上一致；地址不在表内 = 无操作，对脏输入安全。
fn remove_element<T>(vec: &mut Vec<Box<T>>, handle: jlong) {
    let addr = handle as usize;
    vec.retain(|e| (e.as_ref() as *const T) as usize != addr);
}

fn toplevels<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let instance = jptr_to_instance!(instance, "toplevels")?;

    insert_all(
        &mut instance.bridge.toplevels,
        instance.state.xdg_state.toplevel_surfaces(),
    );

    instance.bridge.toplevels.retain(|t| t.alive());

    let toplevels = get_all_handles(&mut instance.bridge.toplevels);
    let array = JLongArray::new(env, toplevels.len())?;
    array.set_region(env, 0, &toplevels)?;
    Ok(array)
}

fn popups<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let instance = jptr_to_instance!(instance, "popups")?;

    insert_all(
        &mut instance.bridge.popups,
        instance.state.xdg_state.popup_surfaces(),
    );

    instance.bridge.popups.retain(|t| t.alive());
    let popups = get_all_handles(&mut instance.bridge.popups);

    let array = JLongArray::new(env, popups.len())?;
    array.set_region(env, 0, &popups)?;
    Ok(array)
}

enum RequestsVec {
    Minimize,
    Maximize,
    Unmaximize,
    Fullscreen,
    Unfullscreen,
}

fn clear_requests<'local>(
    env: &mut Env<'local>,
    instance: &mut WaylandCraft,
    vec: RequestsVec,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let vec = match vec {
        RequestsVec::Minimize => &mut instance.state.requests.minimize,
        RequestsVec::Maximize => &mut instance.state.requests.maximize,
        RequestsVec::Unmaximize => &mut instance.state.requests.unmaximize,
        RequestsVec::Fullscreen => &mut instance.state.requests.fullscreen,
        RequestsVec::Unfullscreen => &mut instance.state.requests.unfullscreen,
    };

    let handles: Vec<jlong> = vec
        .iter()
        .filter(|t| t.alive())
        .map(|t| insert_get_handle(&mut instance.bridge.toplevels, t))
        .collect();

    vec.clear();

    let array = JLongArray::new(env, handles.len())?;
    array.set_region(env, 0, &handles)?;
    Ok(array)
}

fn minimize_req<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let instance = jptr_to_instance!(instance, "minimizeReq")?;
    clear_requests(env, instance, RequestsVec::Minimize)
}

fn maximize_req<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let instance = jptr_to_instance!(instance, "maximizeReq")?;
    clear_requests(env, instance, RequestsVec::Maximize)
}

fn unmaximize_req<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let instance = jptr_to_instance!(instance, "unmaximizeReq")?;
    clear_requests(env, instance, RequestsVec::Unmaximize)
}

fn fullscreen_req<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let instance = jptr_to_instance!(instance, "fullscreenReq")?;
    clear_requests(env, instance, RequestsVec::Fullscreen)
}

fn unfullscreen_req<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let instance = jptr_to_instance!(instance, "unfullscreenReq")?;
    clear_requests(env, instance, RequestsVec::Unfullscreen)
}

fn move_request<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jint>, BridgeError> {
    let instance = jptr_to_instance!(instance, "moveRequest")?;
    let serial = instance.state.requests.move_interactive.pop();

    let serial = match serial {
        Some(s) => s,
        None => return Ok(JPrimitiveArray::null()),
    };

    let serial = u32::from(serial) as jint;

    let array = JIntArray::new(env, 1)?;
    array.set_region(env, 0, &[serial])?;
    Ok(array)
}

fn resize_request<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jint>, BridgeError> {
    let instance = jptr_to_instance!(instance, "resizeRequest")?;
    let req = instance.state.requests.resize_interactive.pop();

    let (serial, edges) = match req {
        Some(r) => r,
        None => return Ok(JPrimitiveArray::null()),
    };

    let serial = u32::from(serial) as jint;
    let edges = u32::from(edges) as jint;

    let array = JIntArray::new(env, 2)?;
    array.set_region(env, 0, &[serial, edges])?;
    Ok(array)
}

enum BufferAttachResult {
    Success,
    Error,
    NotManaged,
}

fn try_attach_shm(
    _instance: &mut WaylandCraft,
    env: &mut Env,
    jsurface: &WLCSurface,
    buf: &WlBuffer,
    surf_data: &SurfaceData,
) -> BufferAttachResult {
    let r = with_buffer_contents(buf, |ptr, _len, metadata| {
        let width = metadata.width as jint;
        let height = metadata.height as jint;
        let format = (metadata.format as u32) as jint;
        let stride = metadata.stride as jint;
        ensure_viewport_valid(surf_data, Size::new(width, height));

        let ptr =
            unsafe { ptr.offset(metadata.offset as isize) }.addr() as jlong;

        jsurface
            .attach_shm_buffer(env, ptr, width, height, format, stride)
            .unwrap();
    });

    match r {
        Ok(_) => BufferAttachResult::Success,
        Err(shm::BufferAccessError::NotManaged) => {
            BufferAttachResult::NotManaged
        }
        Err(_) => BufferAttachResult::Error,
    }
}

fn try_attach_single_pixel(
    _instance: &mut WaylandCraft,
    env: &mut Env,
    jsurface: &WLCSurface,
    buf: &WlBuffer,
    surf_data: &SurfaceData,
) -> BufferAttachResult {
    let pix = match get_single_pixel_buffer(buf) {
        Ok(p) => p,
        Err(_) => {
            return BufferAttachResult::NotManaged;
        }
    };

    ensure_viewport_valid(surf_data, Size::new(1, 1));

    let [r, g, b, a] = pix.rgba8888();
    jsurface
        .attach_single_pixel_buffer(
            env, r as jbyte, g as jbyte, b as jbyte, a as jbyte,
        )
        .unwrap();

    BufferAttachResult::Success
}

fn try_attach_dmabuf(
    instance: &mut WaylandCraft,
    env: &mut Env,
    jsurface: &WLCSurface,
    buf: &WlBuffer,
    surf_data: &SurfaceData,
) -> BufferAttachResult {
    let dmabuf = match get_dmabuf(buf) {
        Ok(d) => d,
        Err(_) => return BufferAttachResult::NotManaged,
    };

    let width = dmabuf.width() as jint;
    let height = dmabuf.height() as jint;
    ensure_viewport_valid(surf_data, Size::new(width, height));

    let weak = dmabuf.weak();
    let handle = insert_get_handle(&mut instance.bridge.dmabufs, &weak);

    let already_attached = jsurface.attach_dmabuf(env, handle).unwrap();

    if already_attached {
        return BufferAttachResult::Success;
    }

    let image = match instance.egl.dmabuf_to_image(dmabuf) {
        Ok(img) => img,
        Err(_) => return BufferAttachResult::Error,
    };

    jsurface
        .attach_new_dmabuf(env, handle, image.addr() as jlong, width, height)
        .unwrap();

    BufferAttachResult::Success
}

fn dmabufs<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let instance = jptr_to_instance!(instance, "dmabufs")?;
    instance.bridge.dmabufs.retain(|d| !d.is_gone());

    let handles = get_all_handles(&mut instance.bridge.dmabufs);
    let array = JLongArray::new(env, handles.len())?;
    array.set_region(env, 0, &handles)?;
    Ok(array)
}

// Proxy to call the try_attach_* family of functions
fn try_attach_buffer(
    instance: &mut WaylandCraft,
    env: &mut Env,
    jsurface: &WLCSurface,
    buf: &WlBuffer,
    surf_data: &SurfaceData,
) -> Result<(), ()> {
    type TryAttachFn = fn(
        instance: &mut WaylandCraft,
        env: &mut Env,
        jsurface: &WLCSurface,
        buf: &WlBuffer,
        surf_data: &SurfaceData,
    ) -> BufferAttachResult;

    let funcs: [TryAttachFn; 3] =
        [try_attach_shm, try_attach_single_pixel, try_attach_dmabuf];
    for func in funcs {
        let result = func(instance, env, jsurface, buf, surf_data);
        match result {
            BufferAttachResult::NotManaged => continue,
            BufferAttachResult::Success => return Ok(()),
            BufferAttachResult::Error => return Err(()),
        }
    }

    unreachable!("Buffer did not match any attachment mechanism!")
}

fn update_surface_data<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    jsurface: WLCSurface<'local>,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "updateSurfaceData")?;

    let handle = jsurface.handle(env)?;
    // v0.12.3 修：之前 surfaceHandle is null 时返回 Err → Java 端
    // throw RuntimeException → MC 崩。改成 log + return Ok——surface 还没
    // 关联 wayland surface（创建中）时跳过本 surface，**不**是错误。
    let surface = match jptr_to_ref::<WlSurface>(handle) {
        Some(s) => s,
        None => {
            // 分级=细节（轮询期良性竞态：surface 创建中的每帧跳行）→ verbose 档。
            if verbose_logging() {
                eprintln!(
                    "[waylandcraft][bridge] updateSurfaceData: surface handle={} \
                     not yet associated with WlSurface（跳过此 surface）",
                    handle
                );
            }
            return Ok(());
        }
    };

    with_states(surface, |data| {
        let mut attr_guard = data.cached_state.get::<SurfaceAttributes>();
        let attr = attr_guard.deref_mut().current();

        let (maybe_buf, mut remove_buf) = if let Some(assign) = &attr.buffer {
            match assign {
                BufferAssignment::NewBuffer(b) => (Some(b), false),
                BufferAssignment::Removed => (None, true),
            }
        } else {
            (None, false)
        };

        if let Some(buf) = maybe_buf {
            let r = try_attach_buffer(instance, env, &jsurface, buf, data);
            if r.is_err() {
                eprintln!("Buffer attach failed!");
                remove_buf = true;
            }

            // Done with buffer attachment
            // All buffers are immediately released because at this point they
            // are all already written to an independent OpenGL texture.
            // (including the dmabufs)
            buf.release();
            attr.buffer = None;
        }

        if remove_buf {
            jsurface.remove_buffer(env).unwrap();
        }

        let mut vp_data_guard = data.cached_state.get::<ViewportCachedState>();
        let vp_data = vp_data_guard.deref_mut().current();

        if let Some(src) = vp_data.src {
            jsurface
                .set_viewport_src(
                    env, src.loc.x, src.loc.y, src.size.w, src.size.h,
                )
                .unwrap();
        }

        if let Some(dst) = vp_data.dst {
            jsurface.set_viewport_dst(env, dst.w, dst.h).unwrap();
        }

        jsurface.clear_damage(env).unwrap();
        for damage in &attr.damage {
            match damage {
                Damage::Surface(d) => {
                    jsurface
                        .add_surface_damage(
                            env, d.loc.x, d.loc.y, d.size.w, d.size.h,
                        )
                        .unwrap();
                }
                Damage::Buffer(d) => {
                    jsurface
                        .add_buffer_damage(
                            env, d.loc.x, d.loc.y, d.size.w, d.size.h,
                        )
                        .unwrap();
                }
            }
        }
        attr.damage.clear();
    });

    Ok(())
}

fn toplevel_surface<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    toplevel_handle: jlong,
) -> Result<jlong, BridgeError> {
    let instance = jptr_to_instance!(instance, "toplevelSurface")?;
    let toplevel = jptr_to_toplevel!(toplevel_handle, "toplevelSurface")?;

    let surface = toplevel.wl_surface();

    Ok(insert_get_handle(&mut instance.bridge.surfaces, surface))
}

fn popup_surface<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    popup_handle: jlong,
) -> Result<jlong, BridgeError> {
    let instance = jptr_to_instance!(instance, "popupSurface")?;
    let popup = jptr_to_popup!(popup_handle, "popupSurface")?;

    let surface = popup.wl_surface();

    Ok(insert_get_handle(&mut instance.bridge.surfaces, surface))
}

fn popup_parent<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    popup_handle: jlong,
) -> Result<jlong, BridgeError> {
    let instance = jptr_to_instance!(instance, "popupParent")?;
    let popup = jptr_to_popup!(popup_handle, "popupParent")?;

    let parent_surface = match popup.get_parent_surface() {
        None => return Ok(0),
        Some(parent_surface) => parent_surface,
    };

    for toplevel in &instance.bridge.toplevels {
        if *toplevel.wl_surface() == parent_surface {
            return Ok(get_handle(&instance.bridge.toplevels, toplevel));
        }
    }

    for popup in &instance.bridge.popups {
        if *popup.wl_surface() == parent_surface {
            return Ok(get_handle(&instance.bridge.popups, popup));
        }
    }

    Ok(0)
}

fn popup_offset<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    popup_handle: jlong,
) -> Result<JPrimitiveArray<'local, jint>, BridgeError> {
    let popup = jptr_to_popup!(popup_handle, "popupOffset")?;

    let mut offset: [jint; 2] = [0, 0];

    popup.with_cached_state(|state| {
        let position = state.last_acked.map(|c| c.state.geometry.loc);

        if let Some(pos) = position {
            offset[0] = pos.x;
            offset[1] = pos.y;
        }
    });

    let array = JIntArray::new(env, 2)?;
    array.set_region(env, 0, &offset)?;
    Ok(array)
}

fn update_surface_tree<'local>(
    env: &mut Env<'local>,
    this: WaylandCraftBridge<'local>,
    instance: jlong,
    surface: WLCSurface<'local>,
) -> Result<WLCSurface<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "updateSurfaceTrees")?;

    let handle = surface.handle(env)?;
    // v0.12.4 修：之前 `?` 在 surface 已死（两次 native 调用之间被 Wayland 销毁，
    // 比如窗口被关闭）时抛 `RuntimeException` → 一路冒到 MC tick → 致命崩溃。
    // 现实：surface 死亡在 wayland 异步模型下完全合法（用户按 X 关窗口）。
    // 这里把"surface not alive"降级——记日志后返回 null surface，让 Java 侧的
    // `toplevel.lastChild = null` 自然处理（外层 update 下一 tick 会清理）。
    // 配合 Java 侧 `updateSurfaceTree` 的 try/catch 双层防御，再也不会炸游戏。
    let surface = match jptr_to_ref::<WlSurface>(handle) {
        Some(s) => s,
        None => {
            // 分级=细节（轮询期良性竞态：toplevel 关闭帧每帧一跳）→ verbose 档。
            if verbose_logging() {
                eprintln!(
                    "[waylandcraft][bridge] updateSurfaceTrees: surface handle=0x{:x} not alive（跳过此 surface，常见于 toplevel 关闭帧）",
                    handle
                );
            }
            return Ok(WLCSurface::null());
        }
    };

    let mut last_child = WLCSurface::null();

    with_surface_tree_upward(
        surface,
        None,
        |surface, _data, _parent| {
            TraversalAction::DoChildren(Some(surface.clone()))
        },
        |surface, data, parent| {
            let handle =
                insert_get_handle(&mut instance.bridge.surfaces, surface);
            // v0.12.1 修：之前 `.unwrap()` 在 Java 端没提供 `get_or_create_surface`
            // 方法时 panic（NoSuchMethodErr）。改成错误吞掉——v0.11.x +
            // v0.10.x 之间 Java 端 jar 不兼容也只导致 surface tree 失败，
            // **不**导致 MC 崩溃。
            let surface = match this.get_or_create_surface(env, handle) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "[waylandcraft][bridge] updateSurfaceTrees: get_or_create_surface 失败: {e:?}（跳过此 surface）"
                    );
                    return;
                }
            };

            // Set the WLCSurface parentHandle
            let parent_handle = if let Some(p) = parent {
                insert_get_handle(&mut instance.bridge.surfaces, p)
            } else {
                0
            };

            if let Err(e) = surface.set_parent_handle(env, parent_handle) { eprintln!("[bridge] set_parent_handle: {e:?}"); };

            // Set last child to point to this current surface
            if !last_child.is_null() {
                if let Err(e) = last_child.set_next_child(env, &surface) { eprintln!("[bridge] set_next_child: {e:?}"); };
            }

            // Set this surfaces nextChild to null
            if let Err(e) = surface.set_next_child(env, WLCSurface::null()) { eprintln!("[bridge] set_next_child null: {e:?}"); };

            // Set this surfaces prevChild to the last child
            if let Err(e) = surface.set_prev_child(env, &last_child) { eprintln!("[bridge] set_prev_child: {e:?}"); };

            // Mark this surface as visited
            if let Err(e) = surface.set_visited(env, true) { eprintln!("[bridge] set_visited: {e:?}"); };

            // Set subsurface location
            let (sx, sy) = if data.cached_state.has::<SubsurfaceCachedState>() {
                let mut subattr_guard =
                    data.cached_state.get::<SubsurfaceCachedState>();
                let subattr = subattr_guard.deref_mut().current();
                (subattr.location.x, subattr.location.y)
            } else {
                (0, 0)
            };

            if let Err(e) = surface.set_xoff(env, sx) { eprintln!("[bridge] set_xoff: {e:?}"); };
            if let Err(e) = surface.set_yoff(env, sy) { eprintln!("[bridge] set_yoff: {e:?}"); };

            last_child = surface;
        },
        |_surface, _data, _parent| true,
    );

    Ok(last_child)
}

fn pointer_motion<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    x: jdouble,
    y: jdouble,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "pointerMotion")?;
    instance.state.seat.pointer_motion(x, y);

    Ok(())
}

fn pointer_motion_focus<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    surface_handle: jlong,
    x: jdouble,
    y: jdouble,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "pointerMotionFocus")?;
    let surface = jptr_to_ref(surface_handle);
    instance.state.seat.pointer_motion_focus(surface, x, y);

    Ok(())
}

fn pointer_rel_motion<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    dx: jdouble,
    dy: jdouble,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "pointerRelMotion")?;
    instance.state.seat.pointer_relative_motion(dx, dy);

    Ok(())
}

fn maybe_pointer_lock<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    surface_handle: jlong,
) -> Result<jboolean, BridgeError> {
    let instance = jptr_to_instance!(instance, "maybePointerLock")?;
    let Some(surface) = jptr_to_ref(surface_handle) else {
        return Ok(false);
    };

    Ok(instance.state.seat.pointer_lock(surface))
}

fn pointer_unlock<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "pointerUnlock")?;
    instance.state.seat.pointer_unlock();

    Ok(())
}

fn pointer_leave<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "pointerLeave")?;
    instance.state.seat.pointer_motion_focus(None, 0.0, 0.0);

    Ok(())
}

fn pointer_button<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    button: jint,
    state: jint,
) -> Result<jint, BridgeError> {
    let instance = jptr_to_instance!(instance, "pointerButton")?;

    let state = match state {
        0 => ButtonState::Released,
        1 => ButtonState::Pressed,
        _ => return Err(BridgeError::UnknownPointerButton(state)),
    };

    Ok(instance.state.seat.pointer_button(button as u32, state) as jint)
}

fn pointer_axis<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    axis: jint,
    value: jdouble,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "pointerAxis")?;

    let axis = match axis {
        0 => Axis::VerticalScroll,
        1 => Axis::HorizontalScroll,
        _ => {
            return Err(BridgeError::UnknownScrollDirection(axis));
        }
    };

    instance.state.seat.pointer_axis(axis, value);

    Ok(())
}

fn cursor_shape<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<jint, BridgeError> {
    let instance = jptr_to_instance!(instance, "cursorShape")?;

    let shape = match instance.state.seat.cursor_shape {
        Some(shape) => shape as jint,
        None => -1,
    };

    Ok(shape)
}

fn keyboard_focus<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    surface_handle: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "keyboardFocus")?;
    let toplevel: Option<&ToplevelSurface> = jptr_to_ref(surface_handle);

    let surface = toplevel.map(|t| t.wl_surface().clone());

    // Update the client gaining keyboard focus with the clipboard contents
    let client = surface.as_ref().and_then(|s| s.client());
    instance.state.data.update_clipboard_client(client);

    match surface {
        Some(s) => {
            // v0.13：set_focus 内部需要 &mut state（拿 host_bridge），
            // 但同时调用方已经在 `instance.state.ime.set_focus(...)` 里
            // 借了 state.ime。NLL 不允许——把 ime std::mem::take 出来，调
            // 完放回。
            let mut ime = std::mem::take(&mut instance.state.ime);
            ime.set_focus(&s, &mut instance.state);
            instance.state.ime = ime;
            instance.state.seat.keyboard_focus(s);
        }
        None => {
            // 同上拆分借用
            let mut ime = std::mem::take(&mut instance.state.ime);
            ime.clear_focus(&mut instance.state);
            instance.state.ime = ime;
            instance.state.seat.keyboard_unfocus();
        }
    };

    instance
        .state
        .xdg_state
        .toplevel_surfaces()
        .iter()
        .for_each(|t| {
            t.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Activated);
            });
        });

    if let Some(t) = toplevel {
        t.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        })
    }

    instance
        .state
        .xdg_state
        .toplevel_surfaces()
        .iter()
        .for_each(|t| {
            t.send_pending_configure();
        });

    Ok(())
}

fn keyboard_activate<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "keyboardActivate")?;
    instance.state.seat.activate_keyboard();
    // 游戏 focused 原料变化 → 候选窗可见性可能翻转，推进采集 epoch
    // （否则 kb_active false→true 而内容不变时，轮询看不到 visible=true）。
    instance.state.ime.popup_touch();

    Ok(())
}

fn keyboard_deactivate<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "keyboardDeactivate")?;
    instance.state.seat.deactivate_keyboard();
    instance.state.ime.popup_touch();

    Ok(())
}

/// keyboardInput(instance, scancode, action) —— Java 侧按 action 三态调用：
/// 0 = release（releaseKey）、1 = press（pressKey）、2 = repeat（repeatKey）。
/// Repeat 由 seat 层保证不改变 xkb 状态机（见 `WLCSeatState::keyboard_key`）。
fn keyboard_input<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    scancode: jint,
    action: jint,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "keyboardInput")?;

    let action = KeyboardAction::from_i32(action)
        .ok_or(BridgeError::UnknownKeyboardState(action))?;

    // sync15 仪表：keyboard_input 全程（JNI 入口→返回，含路由分发与
    // host 同步等待）。"输入法特别卡"（sync14 用户实测）的第一手读数——
    // 若 p95 远大于单帧预算，说明渲染线程在按键链路里被同步等待拖住。
    let t_total = std::time::Instant::now();

    // 1) v0.14 嵌套 IM 主路径：输入法 grab 在场（嵌套 daemon 已激活键盘）
    //    → 按键**只**发给 IME。引擎对按键的命运自行裁决：
    //      消费 → im2 preedit/commit 经 relay 推给应用 ti3；
    //      未消费 → fcitx5 自己经 zwp_virtual_keyboard_v1 重新注入
    //               （ime/virtual_keyboard.rs 落回 seat 原始路由）。
    let mods = instance.state.seat.modifiers_tuple();
    let mut handled = instance
        .state
        .ime
        .handle_key(scancode as u32, action, mods);
    if handled {
        instance.state.ime.metrics.route_im2_grab += 1;
    }

    // 2) ⑧ 协议正确的按键路由（修复实测"输入法不可用"根因）：
    //    旧 C 方案 fire-and-forget 吞键——宿主引擎未激活时 ProcessKeyEvent 返回
    //    consumed=false，键被吞掉且永远不会进应用：**英文字母/符号/Backspace 全丢**
    //    （GNOME/mutter 嵌套 focus 隔离使引擎常不激活；v1.2.5+ 实测每键 consumed=false）。
    //    IBus/fcitx5 协议语义：consumed=true 引擎接管（commit 经 ti3 到达窗口）；
    //    consumed=false / 同步超时 / 无就绪桥 → raw 直通焦点窗口。
    //    press/release 用 ime.raw_keys 保证同路，杜绝孤立 release 或丢 release。
    //    （历史：v0.11 为修"拼音+数字双路乱序"删掉了无条件 fall-through——方向对、
    //    力度错：应该按 consumed 分流，而不是全部吞掉。本实现保持单路不变式：
    //    引擎消费时绝无 raw，引擎未消费时绝无静默丢失。）
    //
    //    v0.14：nested 主路径在线（im2 客户端在位）但 grab 不在场 = 引擎
    //    未接管本键 → 直接 raw 直通，**跳过** host_bridge dbus 提交——
    //    避免同一键被宿主引擎与嵌套引擎两个通道同时裁决造成双发。
    if !handled {
        use crate::seat::KeyboardAction as KA;
        let keycode = scancode as u32;
        let nested_live = instance.state.ime.nested_route_live();
        // sync15 仪表：daemon Starting 期（fcitx5 spawn 后、im2 bind/grab
        // 就绪前——含 libpinyin 词典加载 4-5s）到达的按键计数。这些键
        // 必然落在 raw/host 回退路径；计数 + key_input 直方图共同回答
        // "词典加载期打字是否卡、卡在哪条路"。
        if matches!(instance.state.ime.nested.phase(), crate::ime::NestedPhase::Starting) {
            instance.state.ime.metrics.keys_during_starting += 1;
        }
        let mut key_result: Option<crate::host_bridge::KeyResult> = None;
        if !nested_live {
            if let Some(hb) = &mut instance.host_bridge {
                if hb.is_ready() {
                    instance.state.ime.metrics.route_host_submitted += 1;
                    // v0.10.2：xkb 解码 keysym（ibus/fcitx keyval，不是 evdev）
                    let keysym = instance
                        .state
                        .seat
                        .xkb_state
                        .key_get_one_sym(xkbcommon::xkb::Keycode::new(keycode))
                        .raw() as u32;
                    // 分级=细节（逐键）：⑧ 路由分布计数不受影响。
                    ime_log_v!(
                        "[waylandcraft][ime] bridge submit_key scancode={keycode} keysym={keysym:#x} action={action:?}"
                    );
                    let ke = crate::ime::KeyEvent {
                        keysym,
                        keycode,
                        action,
                        mods,
                    };
                    // sync15 仪表：submit_key_sync 在渲染线程上的实际等待
                    // （150ms 上限）。用户日志里桥不可用（portal no owner
                    // → is_ready false）时这条路根本不走，count=0 即可
                    // 排除"渲染线程被 dbus 等待卡住"假说；若 count>0 且
                    // p95 接近 150ms，就是真同步瓶颈，修排队不修语义。
                    let t_submit = std::time::Instant::now();
                    key_result = Some(hb.submit_key_sync(&crate::ime::DownEvent::Key(ke)));
                    instance.state.ime.metrics.submit_sync_wait.record(t_submit.elapsed());
                }
            }
        } else {
            // nested 主路径在线但 grab 不在场 = 引擎不接管本键，raw 直通。
            instance.state.ime.metrics.route_raw_nested += 1;
        }
        if !nested_live && !matches!(key_result, Some(_)) {
            // 无就绪桥的 raw 兜底（⑧ 原始回退路径）。
            instance.state.ime.metrics.route_raw_no_bridge += 1;
        }
        // None=没有就绪桥（⑧ 原始回退：纯 raw）；Unavailable=超时/通道断按未消费处理；
        // nested_live=true 时 key_result 恒 None——grab 缺席即引擎不接管，raw 直通。
        let raw_needed =
            nested_live || !matches!(key_result, Some(crate::host_bridge::KeyResult::Consumed));
        if matches!(action, KA::Release) {
            if instance.state.ime.raw_keys.remove(&keycode) || raw_needed {
                instance.state.seat.keyboard_key(keycode, action);
            }
        } else {
            if raw_needed {
                instance.state.ime.raw_keys.insert(keycode);
                // 分级=细节（逐键）→ verbose 档。
                ime_log_v!(
                    "[waylandcraft][ime] ⑧ raw forward scancode={keycode} key_result={key_result:?} route={}",
                    if nested_live { "nested-no-grab" } else { "host" }
                );
                instance.state.seat.keyboard_key(keycode, action);
            }
        }
        if key_result.is_some() {
            handled = true;
        }
    }
    instance.state.ime.metrics.key_input.record(t_total.elapsed());
    Ok(())
}

fn keyboard_update<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    scancode: jint,
    pressed: jboolean,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "keyboardUpdate")?;
    instance
        .state
        .seat
        .keyboard_update_xkb(scancode as u32, pressed);
    Ok(())
}

/// setKbLogFile(path) —— 让 Rust 侧 [kb-debug] 行同时写入文件（默认只进 stderr）。
/// Java 在 bridge 初始化后调用，路径通常是 .minecraft/waylandcraft-kb.log。
fn set_kb_log_file<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> Result<(), BridgeError> {
    let path_str: String = _env.get_string(&path)?.into();
    // v0.11.4 覆盖式日志（用户指令"覆盖不是追加"）：
    // 旧文件 → <path>.prev 保留（诊断对比）
    let _ = std::fs::rename(&path_str, &format!("{path_str}.prev"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path_str);
    match file {
        Ok(f) => {
            let mut guard = match KB_LOG_FILE.lock() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("[kb] setKbLogFile: mutex poisoned: {e}");
                    return Ok(());
                }
            };
            *guard = Some(f);
            eprintln!("[kb] setKbLogFile: 键盘调试日志 -> {path_str}");
            Ok(())
        }
        Err(e) => {
            eprintln!("[kb] setKbLogFile: 打开 {path_str} 失败: {e}（继续只写 stderr）");
            Ok(())
        }
    }
}

/// setAudioLogFile(path) —— 让 Rust 侧 [audio] 行同时写入文件（默认只进 stderr）。
fn set_audio_log_file<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> Result<(), BridgeError> {
    let path_str: String = env.get_string(&path)?.into();
    let _ = std::fs::rename(&path_str, &format!("{path_str}.prev"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path_str);
    match file {
        Ok(f) => {
            let mut guard = match AUDIO_LOG_FILE.lock() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("[audio] setAudioLogFile: mutex poisoned: {e}");
                    return Ok(());
                }
            };
            *guard = Some(f);
            eprintln!("[audio] setAudioLogFile: 音频全链路日志 -> {path_str}");
            Ok(())
        }
        Err(e) => {
            eprintln!("[audio] setAudioLogFile: 打开 {path_str} 失败: {e}（继续只写 stderr）");
            Ok(())
        }
    }
}

/// nativeVersion() —— 返回 native 构建标识 "0.1.0 (git <short>)"，
/// Java 加载后立即打印到 latest.log，诊断时无需翻 ime.log 即可确认版本。
fn native_version<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
) -> Result<JString<'local>, BridgeError> {
    Ok(env.new_string(format!(
        "{} (git {})",
        env!("CARGO_PKG_VERSION"),
        env!("WAYLANDCRAFT_GIT_HASH")
    ))?)
}

/// setModVersionNative(version) —— ⑥ Java 注入真实 mod 版本（Fabric metadata）。
fn set_mod_version<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    version: JString<'local>,
) -> Result<(), BridgeError> {
    let v: String = version.try_to_string(env).map_err(|e| BridgeError::JniError(e))?;
    crate::status::set_mod_version(&v);
    Ok(())
}

/// setVerboseLoggingNative(verbose) —— Java 下发日志生效档（单一开关
/// verboseLogging：dev 环境/settings/-Dwc.verbose 综合判定）。全局开关
/// 无需 instance；细节分级规则见文件顶部 VERBOSE_LOG 注释。
fn set_verbose_logging_jni(
    _env: &mut Env<'_>,
    _class: JClass<'_>,
    verbose: jboolean,
) -> Result<(), BridgeError> {
    // jni 0.22 的 jboolean 即 Rust bool（见本文件 pressed 参数同款用法）。
    set_verbose_logging(verbose);
    Ok(())
}

/// getStatusReportNative(instance, javaThread) —— 返回 JSON 状态报告。
///
/// Java 端每 N 帧调用一次，把结果写入覆盖式 `status.log`——一个文件包含
/// 全部子系统状态（native lib / egl / wayland globals / host_bridge / ime /
/// xwayland-satellite / audio / portal / ...），不必再切 4 个独立日志。
///
/// v0.13.4 新增。mod_version 硬编码 "1.2.13"（与 gradle.properties 同步）；
/// v0.13.9 修：之前 v0.13.5/7/8 bump version 时忘记同步这里——导致 status.log
/// 一直显示 "1.2.9"。后续 bump version 时记得改这里（+ gradle.properties）。
fn get_status_report<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    java_thread: JString<'local>,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "getStatusReport")?;
    let thread_name: String = java_thread
        .try_to_string(env)
        .map_err(|e| BridgeError::JniError(e))?;
    // ⑥ mod_version 不再硬编码（历史上三次 bump 忘同步导致 status.log 谎报
    // "1.2.13"）；由 Java 启动时 setModVersionNative 注入真实版本。
    let report = crate::status::StatusReport::gather(&instance, thread_name);
    Ok(env.new_string(report.to_json())?)
}

/// setImeLogFile(path) —— 让 Rust 侧 [system_ime] 行同时写入文件（默认只进 stderr）。
/// Java 在 bridge 初始化后调用，路径通常是 .minecraft/waylandcraft-ime.log。
fn set_ime_log_file<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> Result<(), BridgeError> {
    let path_str: String = env.get_string(&path)?.into();
    let _ = std::fs::rename(&path_str, &format!("{path_str}.prev"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path_str);
    match file {
        Ok(mut f) => {
            // 换文件前先把旧 writer 缓冲的尾部落盘/摘要闭合（不丢行）。
            ime_log_flush();
            // 每次进程启动写一条运行边界（带 native 版本标识）。
            // ime.log 是追加模式（不清空），多实例/多版本日志混在一起时，
            // 用这条分隔行区分"本次运行"的起点 —— 诊断时从最新边界行
            // 往后读，避免把历史残留（如 log-v3 旧版）误判为当前版本。
            {
                use std::io::Write;
                let _ = writeln!(
                    f,
                    "===== WaylandCraft native session start | native {} (git {}) | pid {} =====",
                    env!("CARGO_PKG_VERSION"),
                    env!("WAYLANDCRAFT_GIT_HASH"),
                    std::process::id()
                );
                let _ = f.flush();
            }
            let mut guard = match IME_LOG_FILE.lock() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("[system_ime] setImeLogFile: mutex poisoned: {e}");
                    return Ok(());
                }
            };
            *guard = Some(ImeLogWriter::new(f));
            eprintln!(
                "[system_ime] setImeLogFile: 输入法穿透全链路日志 -> {path_str}"
            );
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "[system_ime] setImeLogFile: 打开 {path_str} 失败: {e}（继续只写 stderr）"
            );
            Ok(())
        }
    }
}

/// runImeDiagnostic(instance) —— 跑完整输入法故障链诊断（v0.11.0+）。
///
/// 返回 JSON 字符串（含 [PASS]/[FAIL] + 根因 + 修复建议）。
/// Java 端可在控制台 `wl ime diagnostic` 调用，结果在 Minecraft log 显示。
fn run_ime_diagnostic<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "runImeDiagnostic")?;
    let report = instance.state.run_diagnostic();
    env.new_string(report).map_err(BridgeError::JniError)
}

/// setImeSource(instance, "auto"|"fcitx5"|"builtin") —— settings 键
/// `imeSource` 下发（WaylandCraft.java responder）。非法值如实记日志、
/// 不改内部状态（与 setImeNested 同纪律）。
fn set_ime_source<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    mode: JString<'local>,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "setImeSource")?;
    let mode: String = env.get_string(&mode)?.into();
    if let Err(e) = instance.state.ime.set_ime_source(&mode) {
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime][source] 忽略非法 imeSource={mode:?}（合法 auto|fcitx5|builtin）: {e}"
        ));
    }
    Ok(())
}

/// imeSourceCommand(instance, "auto"|"fcitx5"|"builtin") —— `/wl ime source`
/// CLI 即时切档（Java 侧已做合法性预检；这里复用同一 setter 保证语义单一）。
fn ime_source_command<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    sub: JString<'local>,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "imeSourceCommand")?;
    let sub: String = env.get_string(&sub)?.into();
    let reply = match instance.state.ime.set_ime_source(&sub) {
        Ok(s) => format!("{s}
{}", instance.state.ime.builtin_status_brief()),
        Err(e) => e,
    };
    env.new_string(reply).map_err(BridgeError::JniError)
}

/// imeBuiltinCandidates(instance) —— 内置候选面板轮询（结构化文本）。
/// 返回空串 = 无变化（轻量合同，与 imePopupFrame 的 null 语义同族）；
/// 否则为快照 JSON（合同见 builtin::BuiltinEngine::snapshot_json）：
/// `{"visible","preedit","cursor","page","pages","page_size",
///    "candidates":[…],"labels":["1"…]}`。仅内置档参与渲染；fcitx5 像素
/// 帧路径（imePopupFrame）与本通道互斥、互不影响。
fn ime_builtin_candidates<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "imeBuiltinCandidates")?;
    let json = instance.state.ime.take_builtin_snapshot().unwrap_or_default();
    env.new_string(json).map_err(BridgeError::JniError)
}

/// imeBuiltinSelect(instance, page_index) —— 候选面板点选（页内 0..8）。
/// 返回提交的文本（空 = 无效果），供 Java 打观测日志。
fn ime_builtin_select<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    page_index: jint,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "imeBuiltinSelect")?;
    let idx = if page_index < 0 { 0usize } else { page_index as usize };
    let committed = instance.state.ime.builtin_select(idx);
    env.new_string(committed).map_err(BridgeError::JniError)
}

/// imeBuiltinPage(instance, delta) —— 候选面板翻页（±1）。返回是否翻动。
fn ime_builtin_page<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    delta: jint,
) -> Result<jboolean, BridgeError> {
    let instance = jptr_to_instance!(instance, "imeBuiltinPage")?;
    Ok(instance.state.ime.builtin_page(delta as i32))
}

/// imeEngList(instance) —— `/wl ime eng`：引擎清单（内置拼音 + fcitx5
/// 种子引擎如实列出）。纯读。
fn ime_eng_list<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "imeEngList")?;
    let text = instance.state.ime.ime_eng_list();
    env.new_string(text).map_err(BridgeError::JniError)
}

/// imeNestedCommand(instance, "status"|"start"|"stop") —— v0.14 嵌套 IME
/// 守护进程三级命令，转发到 Rust NestedIme（ime/nested_daemon.rs）。
/// 返回状态/决策文本；未知 sub 返回错误提示。绝不 panic——命令路径在
/// 游戏主线程（update 循环外），异常会崩游戏。
fn ime_nested_command<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    sub: JString<'local>,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "imeNestedCommand")?;
    let sub: String = env.get_string(&sub)?.into();
    let reply = match instance.state.ime.nested_command(&sub, std::time::Instant::now()) {
        Some(s) => s,
        None => format!(
            "未知子命令: {sub}（合法: status|start|stop）\n当前: {}",
            instance.state.ime.nested_status_line()
        ),
    };
    env.new_string(reply).map_err(BridgeError::JniError)
}

/// setImeNested(instance, mode, home) —— settings（imeNested auto|on|off）
/// 下发到 Rust NestedIme：设置隔离 HOME（不存在则建）+ 路由模式。
/// 非法 mode 字符串被 NestedIme::set_mode 拒绝（返回 false，这里如实记日志，
/// 不改内部状态）。home 空串跳过（服务端/未初始化路径）。
fn set_ime_nested<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    mode: JString<'local>,
    home: JString<'local>,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "setImeNested")?;
    let mode: String = env.get_string(&mode)?.into();
    let home: String = env.get_string(&home)?.into();
    if !home.is_empty() {
        instance.state.ime.nested_set_home(std::path::Path::new(&home));
    }
    if !instance.state.ime.nested_set_mode(&mode) {
        crate::bridge::ime_log_write(&format!(
            "[waylandcraft][ime][nested] 忽略非法 mode={mode:?}（合法 auto|on|off）"
        ));
    }
    Ok(())
}

/// notifyHostFocusGained(instance) —— Minecraft 窗口重新获得 OS 键盘焦点。
///
/// 供输入法穿透的事件驱动焦点重协商使用：若穿透 text_input 因创建晚于
/// 宿主焦点分配而收不到 enter（KWin 已知行为），在此一次性重建触发
/// 宿主重新评估。纯事件驱动，无定时器。
fn notify_host_focus_gained<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<(), BridgeError> {
    // C 方案：host_ime 穿透已删，notifyHostFocusGained 不再需要中转。
    // （嵌套应用通过自己的 GdkIMContext / im2 grab 处理焦点）
    Ok(())
}

/// candidateNav(instance, action, arg) —— Java 候选窗用户操作 → 宿主输入法。
///
/// action: `0`=选字（arg=当前页内下标） `1`=上一页 `2`=下一页。
/// fcitx5 后端走专用 `SelectCandidate`/`PrevPage`/`NextPage` 方法；
/// C 方案：候选窗操作通过 im2 grab 通路自动处理（im2 客户端接收 PreeditString
/// / CommitString 信号直接处理候选）。Java 端 candidateNav 不再需要中转。
/// 保留函数签名兼容 Java 端调用（仅打印日志）。
fn candidate_nav<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
    action: jint,
    _arg: jint,
) -> Result<(), BridgeError> {
    // 分级=细节（已删通路的每调用 stderr 行；Java 导航键可高频触达）→ verbose 档。
    if verbose_logging() {
        eprintln!(
            "[waylandcraft][bridge] candidateNav action={action}（C 方案已删此通路，候选由 im2 grab 处理）"
        );
    }
    Ok(())
}

/// takeLookupTable(instance) —— 取走候选窗快照（JSON，Java 每帧轮询）。
///
/// 返回空串表示无新快照（候选窗应维持上次状态/隐藏）。字段：
/// `visible` 候选窗可见性、`cursor` 高亮页内下标、`page_size` 页大小、
/// `orientation` 布局（0=NotSet 1=Vertical 2=Horizontal）、
/// `candidates`/`labels` 字符串数组。
fn take_lookup_table<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "takeLookupTable")?;
    let json = match instance.state.ime.take_lookup_table() {
        Some(lt) => lookup_table_to_json(&lt),
        None => String::new(),
    };
    env.new_string(json).map_err(BridgeError::JniError)
}

/// 候选窗快照 → JNI 字节流（纯函数，编码合同的唯一事实源——
/// `ime_popup_frame` 生产路径与 tests_popup 合同测试共用，杜绝
/// "测试复刻编码器漂移"）。格式：`[w i32][h i32][seq u32][visible u8][argb…]`
/// （小端；argb 仅在场时 w*h*4 字节；总长 = 13 + pixels）。
pub(crate) fn encode_popup_snapshot(snap: &crate::ime::PopupSnapshot) -> Vec<u8> {
    let pixels = snap.argb.as_ref().map(|a| a.len()).unwrap_or(0);
    let mut data = Vec::with_capacity(13 + pixels);
    data.extend_from_slice(&snap.width.to_le_bytes());
    data.extend_from_slice(&snap.height.to_le_bytes());
    data.extend_from_slice(&snap.seq.to_le_bytes());
    data.push(snap.visible as u8);
    if let Some(argb) = &snap.argb {
        data.extend_from_slice(argb);
    }
    data
}

/// imePopupFrame(instance) —— 轮询嵌套 fcitx5 候选窗帧（Phase 1）。
///
/// 编码见 [`encode_popup_snapshot`]（合同测试双语言锚定：
/// tests_popup::popup_java_polling_sequence_matches_wire_encoding_contract
/// + ImeCandidateLayoutTest.nativeGoldenFrameDecodesExactly）。
/// **返回 null = 自上次轮询以来无任何状态变化**（无新帧且可见性原料未变），
/// 零拷贝；无 popup 时同样落在这条极廉价路径。
///
/// Java 侧解码为 `ImeCandidateLayout.Frame`：null→保持现状；
/// visible=false→隐藏；visible=true+新 seq→上传纹理并显示。
fn ime_popup_frame<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    let instance = jptr_to_instance!(instance, "imePopupFrame")?;
    // 可见性门已不依赖 seat.kb_active（窗口捕获信号，聊天打字恒 false——
    // sync11 根因）；快照只跟 IME 会话激活 + 内容，输入框聚焦由 Java hasBox 把守。
    // sync15 仪表：有交付路径的全程（快照构造 + 编码 + JNI byte[]）。
    // popup_delivered_bytes 直接量化"候选帧全量复制"的流量（~64KB/次；
    // 同帧重复交付修复后：deliveries 增速与字节增速应同步骤降）。
    let t_delivery = std::time::Instant::now();
    let Some(snap) = instance.state.ime.take_popup_snapshot() else {
        return Ok(JPrimitiveArray::null());
    };
    let data = encode_popup_snapshot(&snap);
    let arr = env.byte_array_from_slice(&data)?;
    {
        let ime = &mut instance.state.ime;
        ime.metrics.popup_delivery.record(t_delivery.elapsed());
        ime.metrics.popup_delivered_bytes += data.len() as u64;
    }
    Ok(arr)
}

/// imePopupWorkState(instance) —— 中文引擎工作态（状态透明度角标用）。
/// 语义：im2 端点在位且会话激活 **且** 最近一次交付给应用的批次携带
/// 非空预编辑（= 拼音引擎正在组合）。协议限制如实标注：im2 不向合成器
/// 暴露引擎名，这里不是"引擎 == 拼音"，而是"有预编辑/候选流的工作态"。
/// 纯读（不消耗 epoch、零拷贝），每帧轮询安全。
fn ime_popup_work_state<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<jboolean, BridgeError> {
    let instance = jptr_to_instance!(instance, "imePopupWorkState")?;
    Ok(instance.state.ime.popup_work_state())
}

/// imePopupAnchor(instance) —— 世界形态候选窗锚定原料（sync14）。
///
/// 返回聚焦应用最近一次 ti3 `set_cursor_rectangle` 的矩形 `[x, y, w, h]`
/// （该 surface 的像素坐标，与候选窗帧同源自 nested IME 会话）；应用尚未
/// 上报时返回 **null**——Java 世界渲染据此回退锚窗口 quad 底部中央。
/// 独立只读通道：不消耗 popup epoch、不改 `ime_popup_frame` 的 13 字节头
/// 编码合同（该合同由 tests_popup 与 ImeCandidateLayoutTest 双侧 golden
/// 锁死，本函数与之正交）。每 tick 轮询安全（一次 Option 拷贝）。
fn ime_popup_anchor<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jint>, BridgeError> {
    let instance = jptr_to_instance!(instance, "imePopupAnchor")?;
    let Some((x, y, w, h)) = instance.state.ime.popup_anchor() else {
        return Ok(JPrimitiveArray::null());
    };
    let rect: [jint; 4] = [x, y, w, h];
    let array = JIntArray::new(env, 4)?;
    array.set_region(env, 0, &rect)?;
    Ok(array)
}

/// imeInfo(instance) —— /wl ime info 多行报告：nested 状态、已种引擎、
/// 候选窗统计、工作态、路由与 serial 诊断计数。纯读。
fn ime_info<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "imeInfo")?;
    let kb_active = instance.state.seat.kb_active;
    let text = instance.state.ime.ime_info(kb_active);
    env.new_string(text).map_err(BridgeError::JniError)
}

/// imePopupClick(instance, x, y, button, pressed) —— 候选窗 overlay 区域
/// 点击回传（Phase 3）。x/y = popup surface 局部像素；button = evdev 码
/// （BTN_LEFT=0x110）。native 走 seat popup pointer 通道发
/// enter（必要时 leave 旧面）→ motion → button，每事件后补 frame。
/// 候选命中与上屏由 fcitx5 InputPanel 自己完成（inputwindow.cpp 取证）。
/// 无存活 popup 时极廉价返回。
fn ime_popup_click<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    x: jint,
    y: jint,
    button: jint,
    pressed: jboolean,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "imePopupClick")?;
    let Some(surface) = instance.state.ime.popup_surface() else {
        return Ok(());
    };
    instance.state.seat.popup_pointer_click(
        &surface,
        x.max(0) as f64,
        y.max(0) as f64,
        button as u32,
        pressed, // jni 0.22 的 jboolean 即 Rust bool
    );
    Ok(())
}

/// imePopupAxis(instance, x, y, dy) —— 候选窗区域滚轮回传（Phase 3）。
/// dy = notch 数（已按游戏窗口通路约定取负：正=向下翻页方向同 sendScroll）。
fn ime_popup_axis<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    x: jint,
    y: jint,
    dy: jdouble,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "imePopupAxis")?;
    let Some(surface) = instance.state.ime.popup_surface() else {
        return Ok(());
    };
    instance.state.seat.popup_pointer_axis(
        &surface,
        x.max(0) as f64,
        y.max(0) as f64,
        dy,
    );
    Ok(())
}

/// C 方案：Java 端 CursorRectReporter 已不再需要（嵌套应用通过自己的 ti3
/// SetCursorRectangle 直接报光标给候选窗）。保留函数签名兼容 Java 端调用。
fn update_cursor_rect<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
    _x: jint,
    _y: jint,
    _w: jint,
    _h: jint,
) -> Result<(), BridgeError> {
    // no-op: ti3 已替代
    Ok(())
}

/// LookupTableSnapshot → 紧凑 JSON（手工序列化，避免引 serde_json）。
fn lookup_table_to_json(lt: &crate::ime::LookupTableSnapshot) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }
    let cands: Vec<String> = lt
        .candidates
        .iter()
        .map(|s| format!("\"{}\"", esc(s)))
        .collect();
    let labels: Vec<String> = lt
        .labels
        .iter()
        .map(|s| format!("\"{}\"", esc(s)))
        .collect();
    format!(
        "{{\"visible\":{},\"cursor\":{},\"page_size\":{},\"orientation\":{},\"candidates\":[{}],\"labels\":[{}]}}",
        lt.visible,
        lt.cursor_pos,
        lt.page_size,
        lt.orientation,
        cands.join(","),
        labels.join(",")
    )
}

/// audioCaptureStatus() —— 返回当前音频捕获链路状态（JSON），供 Java /wl audio status 查询。
/// 每段链路（PID → 拓扑 → 节点 → 连接 → 回调 → 数据）都有明确状态，哪里断一眼可见。
fn audio_capture_status<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    let status = crate::audio_capture::get_audio_capture_status();
    let status: String = status.into();
    env.new_string(status)
        .map_err(|e| BridgeError::JniError(e))
}

fn fullscreened<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jlong>, BridgeError> {
    let instance = jptr_to_instance!(instance, "fullscreened")?;

    let mut handles: Vec<jlong> = vec![];
    for toplevel in instance.state.xdg_state.toplevel_surfaces() {
        let fullscreen = toplevel.with_committed_state(|state| {
            state
                .map(|s| s.states.contains(xdg_toplevel::State::Fullscreen))
                .unwrap_or(false)
        });

        if !fullscreen {
            continue;
        }

        handles
            .push(insert_get_handle(&mut instance.bridge.toplevels, toplevel));
    }

    let array = JLongArray::new(env, handles.len())?;
    array.set_region(env, 0, &handles)?;
    Ok(array)
}

fn output_size<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jint>, BridgeError> {
    let instance = jptr_to_instance!(instance, "outputSize")?;

    let size = instance.state.output.size();
    let size: [jint; 2] = [size.w, size.h];

    let array = JIntArray::new(env, 2)?;
    array.set_region(env, 0, &size)?;
    Ok(array)
}

fn output_bounds<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jint>, BridgeError> {
    let instance = jptr_to_instance!(instance, "outputBounds")?;

    let bounds = instance.state.output.bounds();
    let bounds: [jint; 2] = [bounds.w, bounds.h];

    let array = JIntArray::new(env, 2)?;
    array.set_region(env, 0, &bounds)?;
    Ok(array)
}

fn output_resize<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    width: jint,
    height: jint,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "outputResize")?;
    let size = instance.state.output.size();
    let width_changed = size.w != width;
    let height_changed = size.h != height;

    if width < 1 {
        return Err(BridgeError::NonPositiveWidth);
    } else if height < 1 {
        return Err(BridgeError::NonPositiveHeight);
    }

    if !width_changed && !height_changed {
        return Ok(());
    }

    instance.state.output.resize(width, height);

    for toplevel in instance.state.xdg_state.toplevel_surfaces() {
        toplevel.with_pending_state(|state| {
            if state.states.contains(xdg_toplevel::State::Fullscreen) {
                state.size = Some(Size::new(width, height));
            }
        });

        toplevel.send_pending_configure();
    }

    Ok(())
}

fn output_set_bounds<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    width: jint,
    height: jint,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "outputSetBounds")?;
    let bounds = instance.state.output.bounds();
    let width_changed = bounds.w != width;
    let height_changed = bounds.h != height;

    if width < 1 {
        return Err(BridgeError::NonPositiveWidth);
    } else if height < 1 {
        return Err(BridgeError::NonPositiveHeight);
    }

    if !width_changed && !height_changed {
        return Ok(());
    }

    instance.state.output.set_bounds(width, height);

    for toplevel in instance.state.xdg_state.toplevel_surfaces() {
        toplevel.with_pending_state(|state| {
            if state.states.contains(xdg_toplevel::State::Maximized) {
                state.size = Some(Size::new(width, height));
            }
        });

        toplevel.send_pending_configure();
    }

    Ok(())
}

fn check_input_region<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    surface_handle: jlong,
    x: jdouble,
    y: jdouble,
) -> Result<jboolean, BridgeError> {
    let Some(surface) = jptr_to_ref(surface_handle) else {
        return Ok(false);
    };

    let point: Point<f64, Logical> = Point::new(x, y);

    Ok(with_states(surface, |data| {
        let mut attr_guard = data.cached_state.get::<SurfaceAttributes>();
        let attr = attr_guard.deref_mut().current();
        if let Some(r) = &attr.input_region {
            r.contains(point.to_i32_floor())
        } else {
            true
        }
    }))
}

fn surface_xdg_geometry<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    surface_handle: jlong,
) -> Result<JPrimitiveArray<'local, jint>, BridgeError> {
    let surface = match jptr_to_ref(surface_handle) {
        Some(s) => s,
        None => return Ok(JIntArray::null()),
    };

    let geometry: Option<[jint; 4]> = with_states(surface, |states| {
        let mut guard = states.cached_state.get::<SurfaceCachedState>();
        guard
            .current()
            .geometry
            .map(|r| [r.loc.x, r.loc.y, r.size.w, r.size.h])
    });

    if let Some(geometry) = geometry {
        let array = JIntArray::new(env, 4)?;
        array.set_region(env, 0, &geometry)?;
        Ok(array)
    } else {
        Ok(JIntArray::null())
    }
}

fn toplevel_title<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    toplevel_handle: jlong,
) -> Result<JString<'local>, BridgeError> {
    let toplevel = jptr_to_toplevel!(toplevel_handle, "toplevelTitle")?;

    let surface = toplevel.wl_surface();

    // P1-3: surface 上无 XdgToplevelSurfaceData（传错句柄类型/数据已回收）或锁中毒时
    // 返回 null 字符串（调用方已处理 None → JString::null），不再 unwrap panic。
    let title = with_states(surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|d| d.lock().ok().and_then(|g| g.title.clone()))
    });

    if let Some(title) = title {
        Ok(env.new_string(title)?)
    } else {
        Ok(JString::null())
    }
}

fn toplevel_app_id<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    toplevel_handle: jlong,
) -> Result<JString<'local>, BridgeError> {
    let toplevel = jptr_to_toplevel!(toplevel_handle, "toplevelAppId")?;

    let surface = toplevel.wl_surface();

    // P1-3: 同 toplevelTitle——缺数据/锁中毒返回 null，不再 unwrap panic。
    let app_id = with_states(surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|d| d.lock().ok().and_then(|g| g.app_id.clone()))
    });

    if let Some(app_id) = app_id {
        Ok(env.new_string(app_id)?)
    } else {
        Ok(JString::null())
    }
}

/// 原生 wayland 窗口所属客户端进程 PID。
///
/// wayland 的 xdg_toplevel 没有 X11 的 _NET_WM_PID 属性，但 compositor 可以通过
/// SO_PEERCRED（wl_client_get_credentials）拿到连 wayland socket 的客户端 PID ——
/// 对原生 wayland 应用（Firefox 等）这就是窗口进程的 PID，是音频按进程捕获的前提。
fn toplevel_pid<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    toplevel_handle: jlong,
) -> Result<jint, BridgeError> {
    let wlc = jptr_to_instance!(instance, "toplevelPid")?;
    let toplevel = jptr_to_toplevel!(toplevel_handle, "toplevelPid")?;

    let surface = toplevel.wl_surface();
    let Some(client) = surface.client() else {
        return Ok(0);
    };

    match client.get_credentials(&wlc.state.display_handle) {
        Ok(creds) if creds.pid > 0 => Ok(creds.pid as jint),
        _ => Ok(0),
    }
}

fn toplevel_resize<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    toplevel_handle: jlong,
    width: jint,
    height: jint,
    interactive: jboolean,
) -> Result<(), BridgeError> {
    let toplevel = jptr_to_toplevel!(toplevel_handle, "toplevelResize")?;

    toplevel.with_pending_state(|state| {
        state.size = Some(Size::new(width, height));
        state.states.unset(xdg_toplevel::State::Maximized);
        state.states.unset(xdg_toplevel::State::Fullscreen);
        if interactive {
            state.states.set(xdg_toplevel::State::Resizing);
        } else {
            state.states.unset(xdg_toplevel::State::Resizing);
        }
    });

    toplevel.send_pending_configure();

    Ok(())
}

fn toplevel_resize_ovr<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    toplevel_handle: jlong,
    width: jint,
    height: jint,
) -> Result<(), BridgeError> {
    let toplevel = jptr_to_toplevel!(toplevel_handle, "toplevelResizeOvr")?;

    toplevel.with_pending_state(|state| {
        state.size = Some(Size::new(width, height));
        state.states.unset(xdg_toplevel::State::Resizing);
    });

    toplevel.send_pending_configure();

    Ok(())
}

fn toplevel_maximize<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    toplevel_handle: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "toplevelMaximize")?;
    let toplevel = jptr_to_toplevel!(toplevel_handle, "toplevelMaximize")?;

    toplevel.with_pending_state(|state| {
        if state.states.contains(xdg_toplevel::State::Fullscreen) {
            return;
        }
        let output = &instance.state.output;
        state.size = Some(output.bounds());
        state.states.set(xdg_toplevel::State::Maximized);
    });

    toplevel.send_configure();
    Ok(())
}

fn toplevel_fullscreen<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    toplevel_handle: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "toplevelFullscreen")?;
    let toplevel = jptr_to_toplevel!(toplevel_handle, "toplevelFullscreen")?;

    toplevel.with_pending_state(|state| {
        let output = &instance.state.output;
        state.size = Some(output.size());
        state.states.set(xdg_toplevel::State::Fullscreen);
    });

    toplevel.send_configure();
    Ok(())
}

fn free_surface<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    surface_handle: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "freeSurface")?;
    remove_element(&mut instance.bridge.surfaces, surface_handle);

    Ok(())
}

fn free_toplevel<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    toplevel_handle: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "freeToplevel")?;
    remove_element(&mut instance.bridge.toplevels, toplevel_handle);

    Ok(())
}

fn free_popup<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    popup_handle: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "freePopup")?;
    remove_element(&mut instance.bridge.popups, popup_handle);

    Ok(())
}

fn raw_desktop_entry_to_java<'local>(
    env: &mut Env<'local>,
    entry: &RawDesktopEntry,
) -> Result<JRawDesktopEntry<'local>, BridgeError> {
    macro_rules! opt_to_jstring {
        ($env:expr, $string:expr) => {
            match $string {
                Some(string) => JString::new($env, string),
                None => Ok(JString::null()),
            }
        };
    }

    let app_id = JString::new(env, &entry.app_id)?;
    let name = opt_to_jstring!(env, &entry.name)?;
    let generic_name = opt_to_jstring!(env, &entry.generic_name)?;
    let exec = opt_to_jstring!(env, &entry.exec)?;
    let exec_terminal = entry.exec_terminal;
    let comment = opt_to_jstring!(env, &entry.comment)?;
    let visible = entry.visible;
    let icon_path = opt_to_jstring!(env, &entry.icon_path)?;

    let keywords = entry
        .keywords
        .iter()
        .map(|keyword| JString::new(env, keyword))
        .collect::<Result<Vec<_>, _>>()?;

    let kw_array =
        JObjectArray::<JString>::new(env, keywords.len(), &JString::null())?;

    for (index, keyword) in keywords.iter().enumerate() {
        kw_array.set_element(env, index, keyword)?;
    }

    let categories = entry
        .categories
        .iter()
        .map(|keyword| JString::new(env, keyword))
        .collect::<Result<Vec<_>, _>>()?;

    let cat_array =
        JObjectArray::<JString>::new(env, categories.len(), &JString::null())?;

    for (index, category) in categories.iter().enumerate() {
        cat_array.set_element(env, index, category)?;
    }

    Ok(JRawDesktopEntry::new(
        env,
        app_id,
        name,
        generic_name,
        exec,
        exec_terminal,
        comment,
        kw_array,
        cat_array,
        visible,
        icon_path,
    )?)
}

fn load_desktop_entry<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    path: JString<'local>,
) -> Result<JRawDesktopEntry<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "loadDesktopEntry")?;
    let path: PathBuf = path.try_to_string(env)?.into();
    let entry = match instance.xdg.load_entry(path) {
        Some(e) => e,
        None => return Ok(JRawDesktopEntry::null()),
    };

    raw_desktop_entry_to_java(env, &entry)
}

fn load_desktop_entries<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JObjectArray<'local, JRawDesktopEntry<'local>>, BridgeError> {
    let instance = jptr_to_instance!(instance, "loadDesktopEntries")?;
    let entries = instance.xdg.get_raw_entries();
    let entries = entries
        .iter()
        .map(|e| raw_desktop_entry_to_java(env, e))
        .collect::<Result<Vec<_>, _>>()?;

    let array = JObjectArray::<JRawDesktopEntry>::new(
        env,
        entries.len(),
        &JRawDesktopEntry::null(),
    )?;
    for (index, entry) in entries.iter().enumerate() {
        array.set_element(env, index, entry)?;
    }

    Ok(array)
}

fn render_svg<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
    width: jint,
    height: jint,
    buffer_ptr: jlong,
) -> Result<jboolean, BridgeError> {
    let path: PathBuf = path.try_to_string(env)?.into();
    let data = (buffer_ptr as usize) as *mut u8;
    let width = width as u32;
    let height = height as u32;

    Ok(crate::svg::render_svg(path, width, height, data).is_some())
}

fn set_satellite_binary<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> Result<(), BridgeError> {
    let path = path.try_to_string(env)?;
    crate::satellite::set_binary_path(path);
    Ok(())
}

fn exec_app<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    app_id: JString<'local>,
) -> Result<jboolean, BridgeError> {
    let instance = jptr_to_instance!(instance, "execApp")?;
    let app_id = app_id.try_to_string(env)?;

    let mut env_vars = vec![
        ("WAYLAND_DISPLAY".into(), instance.state.socket.clone()),
        ("QT_QPA_PLATFORM".into(), "wayland".into()),
        ("ELECTRON_OZONE_PLATFORM_HINT".into(), "auto".into()),
        ("GDK_BACKEND".into(), "wayland".into()),
    ];
    // X11-only apps need DISPLAY from xwayland-satellite
    if let Some(ref s) = instance.state.satellite {
        env_vars.push(("DISPLAY".into(), s.get_display().into()));
    }

    Ok(instance.xdg.exec_app(app_id, env_vars))
}

// upstream f23fdcb: Java 侧下发首选终端命令
fn set_preferred_terminal<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    cmd: JString<'local>,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "setPreferredTerminal")?;
    let cmd = cmd.try_to_string(env)?;

    instance.xdg.set_preferred_terminal(cmd);

    Ok(())
}

fn check_app<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    app_id: JString<'local>,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "checkApp")?;
    let app_id = app_id.try_to_string(env)?;
    let status = instance.xdg.check_app(app_id);
    Ok(JString::new(env, &status)?)
}

fn set_keymap_default<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "setKeymapDefault")?;
    instance.state.seat.change_keymap_to_default();

    Ok(())
}

fn export_keymap<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    let instance = jptr_to_instance!(instance, "exportKeymap")?;
    let keymap_str = instance.state.seat.export_keymap();
    Ok(JString::new(env, keymap_str)?)
}

fn set_keymap_from_str<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    keymap: JString<'local>,
) -> Result<jboolean, BridgeError> {
    let instance = jptr_to_instance!(instance, "setKeymapFromStr")?;
    let keymap_str = keymap.try_to_string(env)?;
    Ok(instance.state.seat.change_keymap_from_str(keymap_str))
}

fn check_dnd_request<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<JPrimitiveArray<'local, jint>, BridgeError> {
    let instance = jptr_to_instance!(instance, "checkDndRequest")?;
    let serial = match instance.state.data.check_dnd_request() {
        Some(r) => r as jint,
        None => return Ok(JIntArray::null()),
    };

    let array = JIntArray::new(env, 1)?;
    array.set_region(env, 0, &[serial])?;
    Ok(array)
}

fn check_dnd_active<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<jboolean, BridgeError> {
    let instance = jptr_to_instance!(instance, "checkDndActive")?;
    Ok(instance.state.data.dnd.is_some())
}

fn dnd_cancel<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "dndCancel")?;
    instance.state.data.dnd_cancel();

    Ok(())
}

fn dnd_drop<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "dndDrop")?;
    instance.state.data.dnd_drop();

    Ok(())
}

fn dnd_motion<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
    surface_handle: jlong,
    x: jdouble,
    y: jdouble,
) -> Result<(), BridgeError> {
    let instance = jptr_to_instance!(instance, "dndMotion")?;
    let surface = jptr_to_ref(surface_handle);
    instance.state.data.dnd_motion(surface, x, y);

    Ok(())
}

fn dnd_icon<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    instance: jlong,
) -> Result<jlong, BridgeError> {
    let instance = jptr_to_instance!(instance, "dndIcon")?;
    let Some(dnd) = &instance.state.data.dnd else {
        return Ok(0);
    };

    match dnd.icon.as_ref() {
        Some(icon) => {
            Ok(insert_get_handle(&mut instance.bridge.surfaces, icon))
        }
        None => Ok(0),
    }
}

// ═══════════════════════════════════════════════════════
// Portal ScreenCast 捕获 (XDG Desktop Portal, 跨桌面通用)
// ═══════════════════════════════════════════════════════

fn portal_capture_start<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    // 1. Portal: CreateSession → SelectSources → Start (用户确认)
    let node_id = crate::portal_capture::start_portal_capture()
        .map_err(|e| BridgeError::Null("portal_capture_failed"))?;

    // 2. 连接 PipeWire 读帧
    crate::portal_capture::start_pw_stream(node_id)
        .map_err(|e| BridgeError::Null("portal_capture_failed"))?;

    let result = format!("ok:{}", node_id);
    let byte_array = env.byte_array_from_slice(result.as_bytes())?;
    Ok(byte_array)
}

fn portal_capture_frame<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    if let Some(frame) = crate::portal_capture::get_frame() {
        let mut data = Vec::with_capacity(8 + frame.data.len());
        data.extend_from_slice(&frame.width.to_le_bytes());
        data.extend_from_slice(&frame.height.to_le_bytes());
        data.extend_from_slice(&frame.data);
        let byte_array = env.byte_array_from_slice(&data)?;
        return Ok(byte_array);
    }

    let empty = env.byte_array_from_slice(&[])?;
    Ok(empty)
}

fn portal_capture_stop<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<(), BridgeError> {
    // P1-4：旧实现是空函数（"进程退出时线程自灭"），每次 start/stop 泄漏
    // 一个永不退出、仍在收帧分配的 pw-stream 线程 + 一个不 Close 的 portal Session。
    // 现在走 portal_capture::stop_portal_capture()：quit → join → 清帧槽 → Session.Close。
    crate::portal_capture::stop_portal_capture();
    Ok(())
}

// ═══════════════════════════════════════════════════════
// macOS ScreenCaptureKit 捕获（窗口/显示器 → 游戏内贴图）
// 平台分派在 crate::capture_macos 内部完成：非 macOS 一律返回
// "macos_capture_unsupported_platform"，此处不做 cfg 特判。
// 线协议/错误码/接线见 docs/CAPTURE_MACOS.md。
// ═══════════════════════════════════════════════════════

/// 窗口列表；失败时返回单行 `["err:<code>"]`（权限/版本错误必须透出，
/// 不能伪装成空列表）。行格式同 get_desktop_windows：id|title|app|pid|hash|x|y|w|h
fn mac_capture_list<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JObjectArray<'local, JString<'local>>, BridgeError> {
    let lines = match crate::capture_macos::list() {
        Ok(windows) => crate::capture_macos::window_lines(&windows),
        Err(e) => {
            eprintln!("[mac-capture] list failed: {e}");
            vec![format!("err:{e}")]
        }
    };
    strings_to_jarray(env, &lines)
}

/// 显示器列表；行格式 `index|id|WxH|builtin|main|name`，失败同上 err 行。
fn mac_capture_displays<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JObjectArray<'local, JString<'local>>, BridgeError> {
    let lines = match crate::capture_macos::list_displays() {
        Ok(displays) => crate::capture_macos::display_lines(&displays),
        Err(e) => {
            eprintln!("[mac-capture] displays failed: {e}");
            vec![format!("err:{e}")]
        }
    };
    strings_to_jarray(env, &lines)
}

fn strings_to_jarray<'local>(
    env: &mut Env<'local>,
    lines: &[String],
) -> Result<JObjectArray<'local, JString<'local>>, BridgeError> {
    if lines.is_empty() {
        return Ok(JObjectArray::<JString>::new(env, 0, &JString::null())?);
    }
    let result_array = JObjectArray::<JString>::new(env, lines.len(), &JString::null())?;
    for (i, line) in lines.iter().enumerate() {
        let jstr = env.new_string(line)?;
        result_array.set_element(env, i, &jstr)?;
    }
    Ok(result_array)
}

/// 捕获窗口：返回 UTF-8 `"ok:{w}x{h}"`（点尺寸，实际帧见 mac_capture_frame）
/// 或 `"err:<code>"`（permission_denied / macos14_required / window_not_found:ID / …）。
/// 阻塞点：SCShareableContent 异步枚举最长 10s（系统权限弹窗期间）——与
/// portal start 阻塞等待用户确认同一模式，Java 勿在渲染关键线程调用。
fn mac_capture_start_window<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
    window_id: jint,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    let text = match crate::capture_macos::start_window(window_id as u32) {
        Ok((w, h)) => format!("ok:{w}x{h}"),
        Err(e) => {
            eprintln!("[mac-capture] start_window({window_id}) failed: {e}");
            format!("err:{e}")
        }
    };
    Ok(env.byte_array_from_slice(text.as_bytes())?)
}

/// 捕获显示器（index 来自 mac_capture_displays 首列）：返回同上 ok/err 串。
fn mac_capture_start_display<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
    index: jint,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    let text = match crate::capture_macos::start_display(index as u32) {
        Ok((w, h)) => format!("ok:{w}x{h}"),
        Err(e) => {
            eprintln!("[mac-capture] start_display({index}) failed: {e}");
            format!("err:{e}")
        }
    };
    Ok(env.byte_array_from_slice(text.as_bytes())?)
}

/// 拉帧：空 = 无新帧；`[1]` = 会话已终止（清理并 stop）；
/// 否则 `[w u32][h u32][seq u64][ARGB8888 像素]`（LE）。
/// ARGB 内存字节序是 B,G,R,A——与 portal 的 RGBA 不同，纹理上传需按此解释。
fn mac_capture_frame<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
    since_seq: jlong,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    let wire = crate::capture_macos::take_frame(if since_seq < 0 { 0 } else { since_seq as u64 });
    Ok(env.byte_array_from_slice(&wire)?)
}

/// 停止：确定性回收（置标志→唤醒→join→清帧槽）。幂等。
fn mac_capture_stop<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<(), BridgeError> {
    crate::capture_macos::stop().map_err(BridgeError::MacCapture)?;
    Ok(())
}

// Windows.Graphics.Capture 捕获（仅 Windows；见 capture_windows.rs）
//
// 与 portal_capture_* 的对称关系：
//   portal_capture_start → win_capture_start_window / win_capture_start_monitor
//     （同一 "ok:<id>" byte[] 回执；id：窗口=hwnd 十进制，显示器=负数 -1-index）
//   portal_capture_frame → win_capture_frame
//     （帧槽共享 + seq 去重：无新帧返回 null——portal 是每帧盲克隆，这里对齐
//      ime_popup_frame 的零拷贝语义，RemoteWindowRenderer 按 seq 决定是否上传）
//   portal_capture_stop  → win_capture_stop
//     （确定性关闭：session.Close → pool.Close → join "wc-win-capture" 线程
//      带 3s 预算 → 帧缓冲释放；返回关停报告字符串。portal 的空实现
//      [僵尸线程直到进程退出] 是有意不参照的缺陷）
//
// 实现体全部走 crate::capture_windows 的平台中立 API：非 Windows 上
// start/list 返回 Err、take/stop 天然空转——注册表条目跨平台保留（宏不支持
// 按条目 cfg），语义为"该平台上此功能不可用"。
// ═══════════════════════════════════════════════════════

fn win_capture_targets<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    match crate::capture_windows::list_targets() {
        Ok(json) => Ok(env.new_string(json)?),
        Err(e) => {
            eprintln!("[win-capture] targets failed: {e}");
            Err(BridgeError::Null("win_capture_unavailable"))
        }
    }
}

fn win_capture_start_window<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
    hwnd: jlong,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    if hwnd == 0 {
        return Err(BridgeError::Null("win_capture_invalid_hwnd"));
    }
    let report = crate::capture_windows::start_window_capture(hwnd as isize)
        .map_err(|e| {
            eprintln!("[win-capture] start_window failed: {e}");
            BridgeError::Null("win_capture_start_failed")
        })?;
    Ok(env.byte_array_from_slice(report.as_bytes())?)
}

fn win_capture_start_monitor<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
    index: jint,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    if index < 0 {
        return Err(BridgeError::Null("win_capture_invalid_monitor_index"));
    }
    let report = crate::capture_windows::start_monitor_capture(index)
        .map_err(|e| {
            eprintln!("[win-capture] start_monitor failed: {e}");
            BridgeError::Null("win_capture_start_failed")
        })?;
    Ok(env.byte_array_from_slice(report.as_bytes())?)
}

fn win_capture_frame<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    // seq 去重：无新帧 → null（Java 保持现纹理，零拷贝零上传）。
    match crate::capture_windows::take_frame_wire() {
        Some(data) => Ok(env.byte_array_from_slice(&data)?),
        None => Ok(JPrimitiveArray::null()),
    }
}

fn win_capture_stop<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JString<'local>, BridgeError> {
    // 幂等 + 确定性：返回报告（joined / thread-timeout-detached / no-session）。
    let report = crate::capture_windows::stop_capture();
    Ok(env.new_string(report.as_str())?)
}

// ═══════════════════════════════════════════════════════
// 按进程音频捕获 (PipeWire: 共享窗口的声音)
// ═══════════════════════════════════════════════════════

fn audio_capture_start<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
    pid: jint,
) -> Result<(), BridgeError> {
    // pid==0 = 窗口 PID 解析失败：仍然启动捕获（audio_capture.rs 会走
    // monitor-fallback 整机捕获），只有负数才是非法入参。
    if pid < 0 {
        return Err(BridgeError::Null("audio_capture_invalid_pid"));
    }
    crate::audio_capture::start_audio_capture(pid as u32)
        .map_err(|e| {
            eprintln!("[audio] start failed: {}", e);
            BridgeError::AudioCapture(e)
        })?;
    Ok(())
}

fn audio_capture_poll<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JPrimitiveArray<'local, i8>, BridgeError> {
    if let Some(data) = crate::audio_capture::poll_audio_capture() {
        let byte_array = env.byte_array_from_slice(&data)?;
        return Ok(byte_array);
    }
    let empty = env.byte_array_from_slice(&[])?;
    Ok(empty)
}

fn audio_capture_stop<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<(), BridgeError> {
    crate::audio_capture::stop_audio_capture();
    Ok(())
}

fn get_desktop_windows<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    _instance: jlong,
) -> Result<JObjectArray<'local, JString<'local>>, BridgeError> {
    let windows = crate::desktop_windows::get_desktop_windows();

    if windows.is_empty() {
        return Ok(JObjectArray::<JString>::new(env, 0, &JString::null())?);
    }

    let result_array = JObjectArray::<JString>::new(env, windows.len(), &JString::null())?;

    for (i, w) in windows.iter().enumerate() {
        let formatted = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}",
            w.window_id, w.title, w.app_id, w.pid, w.hash, w.x, w.y, w.width, w.height
        );
        let jstr = env.new_string(&formatted)?;
        result_array.set_element(env, i, &jstr)?;
    }

    Ok(result_array)
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use crate::diagnostic::{CheckStatus, DiagnosticCheck, DiagnosticReport};

    #[test]
    fn diagnostic_check_pass_render() {
        let check = DiagnosticCheck::pass("test_layer", "test detail");
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.layer, "test_layer");
        assert_eq!(check.detail, "test detail");
        assert!(check.root_cause.is_none());
        assert!(check.suggestion.is_none());
    }

    #[test]
    fn diagnostic_check_fail_has_root_cause() {
        let check = DiagnosticCheck::fail(
            "test_layer", "detail", "ROOT CAUSE", "SUGGESTION",
        );
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(check.root_cause.as_deref(), Some("ROOT CAUSE"));
        assert_eq!(check.suggestion.as_deref(), Some("SUGGESTION"));
    }

    #[test]
    fn diagnostic_check_warn_has_suggestion() {
        let check = DiagnosticCheck::warn("test", "detail", "fix it");
        assert_eq!(check.status, CheckStatus::Warn);
        assert_eq!(check.suggestion.as_deref(), Some("fix it"));
    }

    #[test]
    fn diagnostic_check_skip_no_cause() {
        let check = DiagnosticCheck::skip("test", "detail");
        assert_eq!(check.status, CheckStatus::Skip);
    }

    #[test]
    fn diagnostic_render_includes_all_layers() {
        let report = DiagnosticReport {
            checks: vec![
                DiagnosticCheck::pass("layer_a", "detail_a"),
                DiagnosticCheck::fail("layer_b", "detail_b", "rc", "sug"),
            ],
        };
        let rendered = report.render();
        assert!(rendered.contains("[PASS] layer_a: detail_a"));
        assert!(rendered.contains("[FAIL] layer_b: detail_b"));
        assert!(rendered.contains("Root cause: rc"));
        assert!(rendered.contains("Suggestion: sug"));
        assert!(rendered.contains("RESULT: INPUT METHOD BROKEN"));
    }

    #[test]
    fn diagnostic_render_pass_healthy() {
        let report = DiagnosticReport {
            checks: vec![DiagnosticCheck::pass("l1", "ok")],
        };
        let rendered = report.render();
        assert!(rendered.contains("[PASS] l1: ok"));
        assert!(rendered.contains("RESULT: INPUT METHOD HEALTHY"));
    }
}


#[cfg(test)]
mod handle_tests {
    use super::*;

    // P1-3 句柄 helper 语义测试。用 u32 作 T（Clone+PartialEq 齐备，
    // 地址语义与被测函数对 smithay 资源的用法一致）来验证：
    // - 不再对 stale/0 handle 解引用（旧 remove_element 是 UB）
    // - get/insert 未命中返回 0 而不是 panic

    fn addr<T>(e: &Box<T>) -> jlong {
        (e.as_ref() as *const T) as usize as jlong
    }

    #[test]
    fn insert_get_handle_roundtrip_and_idempotent() {
        let mut v: Vec<Box<u32>> = vec![];
        let h = insert_get_handle(&mut v, &7u32);
        assert_ne!(h, 0);
        assert_eq!(v.len(), 1);
        // 等值再插入：不新增，返回同一句柄
        let h2 = insert_get_handle(&mut v, &7u32);
        assert_eq!(h, h2);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn get_handle_miss_returns_zero_not_panic() {
        let v: Vec<Box<u32>> = vec![Box::new(1u32)];
        assert_eq!(get_handle(&v, &1u32), addr(&v[0]));
        // 旧实现这里 .unwrap() panic（JNI 边界依赖包装层兜底）；现在是 0 句柄
        assert_eq!(get_handle(&v, &999u32), 0);
    }

    #[test]
    fn remove_element_by_address_only() {
        let mut v: Vec<Box<u32>> = vec![Box::new(1u32), Box::new(2u32)];
        let h1 = addr(&v[0]);
        remove_element(&mut v, h1);
        assert_eq!(v.len(), 1);
        assert_eq!(*v[0], 2u32);

        // stale handle（已从表中移除、指向可能已释放的内存）：
        // 旧实现会解引用它 → use-after-free UB；现在纯地址比较，无操作。
        remove_element(&mut v, h1);
        assert_eq!(v.len(), 1);
        remove_element(&mut v, 0);
        assert_eq!(v.len(), 1);
        remove_element(&mut v, -1);
        assert_eq!(v.len(), 1);

        let last = addr(&v[0]);
        remove_element(&mut v, last);
        assert!(v.is_empty());
    }

    #[test]
    fn remove_element_stale_ptr_does_not_remove_live_element() {
        // 模拟"句柄指向的内存已被复用"场景：一个与现存元素**等值但不同地址**的堆对象。
        // 旧实现按值比较会把仍存活的元素误删；新实现只认地址。
        let mut v: Vec<Box<u32>> = vec![Box::new(5u32)];
        let alias = Box::new(5u32); // 与 v[0] 等值、不同地址
        remove_element(&mut v, (alias.as_ref() as *const u32) as usize as jlong);
        assert_eq!(v.len(), 1, "等值但不同地址的句柄不得删除存活元素");
        let _ = alias;
    }
}
