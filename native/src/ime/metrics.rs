//! IME 热路径轻量计时/计数（sync15 性能取证，"先测量后修"的仪表半程）。
//!
//! ## 设计约束
//!
//! - **无锁**：所有热路径计数器都是 [`ImeMetrics`] 的普通字段——
//!   keyboard_input 路由分发、popup 交付、update 轮次全部发生在同一个
//!   事件循环线程（JNI 调用与 dispatch 同线程），读侧（`/wl ime info`、
//!   10s 聚合行）同线程，无需原子。唯一跨线程的是日志 IO 计数
//!   （host_bridge DBus worker 也经 `bridge::ime_log_write` 写）——那边
//!   用 relaxed [`std::sync::atomic::AtomicU64`]，单次几条指令，只进报告
//!   不进判定。
//! - **直方图桶**（12 个有界桶 + 溢出桶）：record 一次二分 + 一次自增；
//!   分位数报告的是**桶上界近似**（如实标注 `≈`），count/sum/max 精确。
//! - **只增不减**：累计语义保证 `ime_info` 两次调用结果一致（既有
//!   零副作用合同测试不破）；10s 聚合行用窗口差分，不用重置。
//!
//! ## 测量点（与修复项一一对应，报告在 `/wl ime info` 延迟段）
//!
//! - `key_input`：keyboard_input JNI 入口→返回全程——"按键链路排队/
//!   同步等待"的直接读数（用户实测 sync14 五键 8ms 爆发的判据）。
//! - `submit_sync_wait`：host_bridge submit_key_sync 的 150ms 等待实测
//!   分布（含桥不可用时的微秒级快速拒绝——用 count 区分两种形态）。
//! - `popup_delivery`：ime_popup_frame 有交付路径时长（快照构造 + 编码 +
//!   JNI byte[] 拷贝）；配 `popup_dup_skipped`/`popup_delivered_bytes`
//!   回答"同帧是否被反复全量交付"。
//! - 路由计数 + `keys_during_starting`：回答"fcitx5 spawn 后到 im2 grab
//!   前（libpinyin 词典加载 4-5s 窗口）按键走什么路径、是否阻塞"。
//! - `log_lines/log_bytes/log_write_ops/log_suppressed`：每行
//!   open/append+flush 修复前后的系统调用与字节量级对比。

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// 直方图桶上界（µs）。样本归入第一个 ≥ 它的桶；最后一个是 (>1s) 溢出桶。
const BUCKET_CAPS_US: [u64; 12] = [
    250, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000, 200_000, 500_000, 1_000_000,
];

/// 一个延迟分布的桶化计数（无锁、只增）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LatencyHistogram {
    buckets: [u32; BUCKET_CAPS_US.len() + 1],
    count: u64,
    sum_us: u64,
    max_us: u64,
}

impl LatencyHistogram {
    #[inline]
    pub fn record_us(&mut self, us: u64) {
        // binary_search 对 <12 元素是分支预测友好的短链；比线性扫描确定性更好。
        let idx = match BUCKET_CAPS_US.binary_search(&us) {
            Ok(i) => i,     // 恰在容量上 → 归该桶（上界含）
            Err(i) => i,    // 第一个 >us 的桶
        };
        self.buckets[idx] = self.buckets[idx].saturating_add(1);
        self.count += 1;
        self.sum_us += us;
        self.max_us = self.max_us.max(us);
    }

    #[inline]
    pub fn record(&mut self, d: Duration) {
        self.record_us(d.as_micros() as u64);
    }

    #[inline]
    pub fn count(&self) -> u64 {
        self.count
    }

    #[inline]
    pub fn max_us(&self) -> u64 {
        self.max_us
    }

    pub fn mean_us(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.sum_us / self.count
        }
    }

    /// 第 p 百分位（1..=100）的**桶上界近似**；溢出桶返回 1s+1µs 哨兵
    /// （报告端渲染为 ">1s"）。空分布 None。
    pub fn percentile_us(&self, p: u64) -> Option<u64> {
        if self.count == 0 || p == 0 || p > 100 {
            return None;
        }
        let rank = self.count.mul_ceil_100(p);
        let mut acc = 0u64;
        for (i, b) in self.buckets.iter().enumerate() {
            acc += *b as u64;
            if acc >= rank {
                return Some(match BUCKET_CAPS_US.get(i) {
                    Some(cap) => *cap,
                    None => u64::MAX, // 溢出桶哨兵（格式化为 >1s）
                });
            }
        }
        unreachable!("bucket sums invariant: Σbuckets == count")
    }

    /// 人类可读一行：`n=… mean=…µs p50≈… p95≈… max=…`；空分布 `n=0`。
    pub fn format(&self) -> String {
        if self.count == 0 {
            return "n=0".to_string();
        }
        let p50 = self
            .percentile_us(50)
            .map(format_us)
            .unwrap_or_else(|| "-".into());
        let p95 = self
            .percentile_us(95)
            .map(format_us)
            .unwrap_or_else(|| "-".into());
        format!(
            "n={} mean={}µs p50≈{} p95≈{} max={}",
            self.count,
            self.mean_us(),
            p50,
            p95,
            format_us(self.max_us)
        )
    }
}

/// `count * p / 100` 向上取整（u64 溢出安全：count 是进程级按键数，
/// 乘 100 后仍远小于 u64::MAX，但用 checked_mul 兜底饱和）。
trait MulCeil100 {
    fn mul_ceil_100(self, p: u64) -> u64;
}
impl MulCeil100 for u64 {
    fn mul_ceil_100(self, p: u64) -> u64 {
        let prod = self.saturating_mul(p);
        (prod + 99) / 100
    }
}

/// µs 数值 → 可读（≥10ms 用 ms，溢出哨兵显示 >1s）。
fn format_us(v: u64) -> String {
    if v == u64::MAX {
        ">1s".to_string()
    } else if v >= 10_000 {
        format!("{}ms", v as f64 / 1000.0)
    } else {
        format!("{}µs", v)
    }
}

/// 日志 IO 计数（桥级；DBus worker 与主循环都可能写，relaxed 原子足够）。
#[derive(Debug)]
pub struct LogIoCounters {
    /// 实际产生 stderr/文件写入的行数（被重复抑制的行不计）。
    pub lines: AtomicU64,
    /// 写入字节（含时间戳前缀）。
    pub bytes: AtomicU64,
    /// BufWriter 向 fd 实际 flush 的批次数（≈ write 系统调用次数）。
    pub write_ops: AtomicU64,
    /// 相邻重复行被合并抑制的次数。
    pub suppressed_repeats: AtomicU64,
}

impl LogIoCounters {
    pub const fn new() -> Self {
        Self {
            lines: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            write_ops: AtomicU64::new(0),
            suppressed_repeats: AtomicU64::new(0),
        }
    }
    #[inline]
    pub fn add_line(&self, bytes: u64) {
        self.lines.fetch_add(1, Relaxed);
        self.bytes.fetch_add(bytes, Relaxed);
    }
    #[inline]
    pub fn add_write_op(&self) {
        self.write_ops.fetch_add(1, Relaxed);
    }
    #[inline]
    pub fn add_suppressed(&self) {
        self.suppressed_repeats.fetch_add(1, Relaxed);
    }
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.lines.load(Relaxed),
            self.bytes.load(Relaxed),
            self.write_ops.load(Relaxed),
            self.suppressed_repeats.load(Relaxed),
        )
    }
}

impl Default for LogIoCounters {
    fn default() -> Self {
        Self::new()
    }
}

/// 相邻重复行合并器（纯逻辑，单测直打；`bridge::ime_log_write` 持锁调用）。
///
/// 语义：与上一条**内容相同**的行（时间戳由调用方在写出时添加，内容比对
/// 不含时间戳）不产生 stderr/文件写入，段计数 +1；下一条**不同**内容到来
/// （或 10s 心跳显式 [`Self::flush`]）时段闭合——重复数 >1 时调用方补写
/// 一行摘要 `"<内容> (xN)"`。效果：热路径刷屏行（vk modifiers、引擎直通
/// 心跳等）在日志里首尾仍可见且带总次数（诊断信息不丢），IO 从 N 次降到
/// 1 次；进程被 kill 时只可能丢"最后一段的重复计数"，最后一条行的本体
/// 已写过，无信息级损失。
#[derive(Debug, Default)]
pub struct RepeatCollapse {
    current: Option<(String, u32)>,
}

impl RepeatCollapse {
    /// 记一行。返回：
    /// - `Feed::Absorbed`：与上一条相同，本行被抑制；
    /// - `Feed::NewLine { closed: None }`：首行/上一条已闭合，写本体即可；
    /// - `Feed::NewLine { closed: Some((line, n)) }`：旧段闭合且 n>1，
    ///   调用方先补写旧段摘要再写本体；n==1 的旧段本体早已写过，不补。
    pub fn feed(&mut self, s: &str) -> Feed {
        if let Some((line, n)) = &mut self.current {
            if *line == s {
                *n = n.saturating_add(1);
                return Feed::Absorbed;
            }
            let closed = if *n > 1 { Some((line.clone(), *n)) } else { None };
            self.current = Some((s.to_string(), 1));
            return Feed::NewLine { closed };
        }
        self.current = Some((s.to_string(), 1));
        Feed::NewLine { closed: None }
    }

    /// 闭合当前段（心跳/会话边界用）。返回 Some((line, n>1)) = 需补写摘要。
    pub fn flush(&mut self) -> Option<(String, u32)> {
        match self.current.take() {
            Some((_, 1)) => None,
            Some(x) => Some(x),
            None => None,
        }
    }

    /// 当前段内容+次数（观测/测试用）。
    pub fn pending(&self) -> Option<(&str, u32)> {
        self.current.as_ref().map(|(l, n)| (l.as_str(), *n))
    }
}

pub enum Feed {
    /// 与上一条重复，本行未写。
    Absorbed,
    /// 新行（旧段若有多次重复，closed 给出摘要原料）。
    NewLine { closed: Option<(String, u32)> },
}

/// 10s 差分窗口的上一段快照。
#[derive(Clone, Copy, Debug, Default)]
struct WindowCounts {
    keys: u64,
    submit_waits: u64,
    deliveries: u64,
    dup_skipped: u64,
    delivered_bytes: u64,
    log_lines: u64,
    update_rounds: u64,
}

/// IME 侧热路径指标全集。挂在 `ImeState.metrics`；单事件循环线程读写。
#[derive(Debug, Default)]
pub struct ImeMetrics {
    /// keyboard_input JNI 入口→返回。
    pub key_input: LatencyHistogram,
    /// host_bridge submit_key_sync 阻塞时长（含快速拒绝）。
    pub submit_sync_wait: LatencyHistogram,
    /// ime_popup_frame 交付路径（快照+编码+JNI）时长。
    pub popup_delivery: LatencyHistogram,
    /// 按键路由分布。
    pub route_im2_grab: u64,
    pub route_raw_nested: u64,
    pub route_host_submitted: u64,
    pub route_raw_no_bridge: u64,
    /// daemon Starting（libpinyin 词典加载窗口）期间的按键数。
    pub keys_during_starting: u64,
    /// popup 内容指纹未变化而被去重跳过的轮询次数。
    pub popup_dup_skipped: u64,
    /// 经 JNI 交付给 Java 的候选窗字节累计（含 13B 头）。
    pub popup_delivered_bytes: u64,
    /// WaylandCraft::update() 轮次（每轮一次 event_loop.dispatch）。
    pub update_rounds: u64,

    // ── 10s 聚合行窗口簿记（不影响累计报告）──
    window_started: Option<Instant>,
    window_prev: WindowCounts,
}

/// 交付/帧计数在 tick 时由调用方现取（popup 状态是它们的唯一事实源，
/// metrics 只留窗口差分用的键值，不复制计数存储）。
#[derive(Clone, Copy, Debug)]
pub struct PopupDeliveryCounts {
    pub frames: u64,
    pub deliveries: u64,
}

impl ImeMetrics {
    /// `/wl ime info` 延迟段（累计值；**零副作用**——不碰窗口簿记，
    /// 两次调用输出必然一致）。
    ///
    /// 刻意**不**读桥级全局日志计数（`LogIoCounters` 跨测试线程变化会
    /// 让"两次调用输出一致"的既有零副作用合同变得不确定）；日志 IO
    /// 数字走 10s 聚合行（`tick_window_report`，只在生产 update 循环
    /// 触发）与 bench 测试直接打印。
    pub fn format_latency_section(&self, popup: PopupDeliveryCounts) -> String {
        format!(
            "延迟 keyboard_input(全程): {}\n\
             延迟 submit_key_sync(等待): {}\n\
             延迟 popup交付(encode+JNI): {}\n\
             按键路由: im2-grab={} raw直通(nested)={} host提交={} 无桥raw={} Starting期(词典窗口)={}\n\
             候选窗交付: frames={} deliveries={} 重复跳过={} 交付字节={}KB\n",
            self.key_input.format(),
            self.submit_sync_wait.format(),
            self.popup_delivery.format(),
            self.route_im2_grab,
            self.route_raw_nested,
            self.route_host_submitted,
            self.route_raw_no_bridge,
            self.keys_during_starting,
            popup.frames,
            popup.deliveries,
            self.popup_dup_skipped,
            self.popup_delivered_bytes / 1024,
        )
    }

    /// update() 每轮调用：推进 `update_rounds`，每满 10s（或首轮建窗）
    /// 产出一行聚合 debug 日志并差分推进窗口。返回 Some(line)。
    ///
    /// 纯读自身 + 参数；除窗口簿记外不改任何累计器。
    pub fn tick_window_report(
        &mut self,
        now: Instant,
        popup: PopupDeliveryCounts,
        log: &LogIoCounters,
    ) -> Option<String> {
        self.update_rounds += 1;
        let (lines, _, _, _) = log.snapshot();
        let cur = WindowCounts {
            keys: self.key_input.count(),
            submit_waits: self.submit_sync_wait.count(),
            deliveries: popup.deliveries,
            dup_skipped: self.popup_dup_skipped,
            delivered_bytes: self.popup_delivered_bytes,
            log_lines: lines,
            update_rounds: self.update_rounds,
        };
        let due = match self.window_started {
            None => true, // 首轮建窗也打一行：基线时刻在 ime.log 有锚点
            Some(t) => now.saturating_duration_since(t) >= Duration::from_secs(10),
        };
        if !due {
            return None;
        }
        let label = if self.window_started.is_none() { "开测窗口建立" } else { "10s" };
        let prev = std::mem::take(&mut self.window_prev);
        self.window_started = Some(now);
        self.window_prev = cur;
        let d = |a: u64, b: u64| a.saturating_sub(b);
        Some(format!(
            "[waylandcraft][ime][latency] {}: keys={} p50≈{} p95≈{} max={} submit_wait={} popup交付={} (+{}KB) 重复跳过={} 更新轮={} log行={}",
            label,
            d(cur.keys, prev.keys),
            self.key_input.percentile_us(50).map(format_us).unwrap_or_else(|| "-".into()),
            self.key_input.percentile_us(95).map(format_us).unwrap_or_else(|| "-".into()),
            format_us(self.key_input.max_us()),
            d(cur.submit_waits, prev.submit_waits),
            d(cur.deliveries, prev.deliveries),
            d(cur.delivered_bytes, prev.delivered_bytes) / 1024,
            d(cur.dup_skipped, prev.dup_skipped),
            d(cur.update_rounds, prev.update_rounds),
            d(cur.log_lines, prev.log_lines),
        ))
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_empty_formats_n0() {
        let h = LatencyHistogram::default();
        assert_eq!(h.format(), "n=0");
        assert_eq!(h.percentile_us(95), None);
        assert_eq!(h.mean_us(), 0);
    }

    #[test]
    fn histogram_bucket_boundaries_inclusive_to_upper_cap() {
        let mut h = LatencyHistogram::default();
        h.record_us(0); // 桶 0（≤250µs）
        h.record_us(250); // 桶 0 上界含
        h.record_us(251); // 桶 1（≤500µs）
        h.record_us(1_000_001); // 溢出桶
        assert_eq!(h.count(), 4);
        assert_eq!(h.max_us(), 1_000_001);
        assert_eq!(h.percentile_us(50), Some(250));
        assert_eq!(h.percentile_us(95), Some(u64::MAX)); // 排名 4 → 溢出桶
        assert_eq!(h.percentile_us(25), Some(250));
    }

    #[test]
    fn histogram_percentiles_are_bucket_upper_approximations() {
        let mut h = LatencyHistogram::default();
        for _ in 0..99 {
            h.record_us(100);
        }
        h.record_us(150_000);
        assert_eq!(h.percentile_us(50), Some(250));
        assert_eq!(h.percentile_us(95), Some(250)); // 95% 排名 = 95 → 首桶
        assert_eq!(h.percentile_us(100), Some(200_000));
        assert_eq!(h.mean_us(), (99 * 100 + 150_000) / 100);
        let s = h.format();
        assert!(s.contains("n=100"), "{s}");
        assert!(s.contains("p95≈250µs"), "{s}");
        assert!(s.contains("max=150ms"), "{s}");
    }

    #[test]
    fn repeat_collapse_absorbs_then_closes_with_summary() {
        let mut rc = RepeatCollapse::default();
        // 首行：NewLine closed=None（写本体）。
        assert!(matches!(rc.feed("mod"), Feed::NewLine { closed: None }));
        // 重复两行：Absorbed（不产生 IO）。
        assert!(matches!(rc.feed("mod"), Feed::Absorbed));
        assert!(matches!(rc.feed("mod"), Feed::Absorbed));
        assert_eq!(rc.pending(), Some(("mod", 3)));
        // 内容变化：旧段闭合且带摘要原料。
        match rc.feed("commit") {
            Feed::NewLine { closed: Some((line, n)) } => {
                assert_eq!((line.as_str(), n), ("mod", 3));
            }
            other => panic!("expect closed summary, got swallowed {:?}", matches!(other, Feed::Absorbed)),
        }
        // 单行段闭合不补摘要（本体早已写过）。
        assert!(rc.flush().is_none());
    }

    #[test]
    fn repeat_collapse_flush_returns_pending_summary_and_resumes() {
        let mut rc = RepeatCollapse::default();
        assert!(matches!(rc.feed("A"), Feed::NewLine { .. }));
        assert!(matches!(rc.feed("A"), Feed::Absorbed));
        let summary = rc.flush().expect("A 段重复 2 次必须给出摘要");
        assert_eq!((summary.0.as_str(), summary.1), ("A", 2));
        // flush 后同内容再来 = 新段本体（重计数），避免"摘要后静默吞行"。
        assert!(matches!(rc.feed("A"), Feed::NewLine { closed: None }));
    }

    #[test]
    fn window_report_first_round_anchors_and_ten_second_diff() {
        let m = &mut ImeMetrics::default();
        m.key_input.record_us(400); // 1 个按键样本进入窗口差分基准
        let log = LogIoCounters::new();
        let t0 = Instant::now();
        let line1 = m.tick_window_report(t0, PopupDeliveryCounts { frames: 0, deliveries: 0 }, &log)
            .expect("首轮建窗必须打基线行");
        assert!(line1.contains("开测窗口建立"), "{line1}");
        assert!(line1.contains("keys=1"), "{line1}");
        assert!(m.tick_window_report(t0 + Duration::from_millis(9_500),
            PopupDeliveryCounts { frames: 0, deliveries: 0 }, &log).is_none());
        m.key_input.record_us(90); // 第二个样本
        let line2 = m.tick_window_report(t0 + Duration::from_millis(10_500),
            PopupDeliveryCounts { frames: 0, deliveries: 0 }, &log)
            .expect("满 10s 必须打聚合行");
        assert!(line2.contains("[ime][latency] 10s"), "{line2}");
        assert!(line2.contains("keys=1"), "窗口差分：本窗只数到第二个键 → {line2}");
        assert!(line2.contains("更新轮=2"), "{line2}");
    }

    #[test]
    fn latency_section_is_pure_and_lists_all_instrumented_names() {
        let mut m = ImeMetrics::default();
        m.key_input.record_us(300);
        m.route_im2_grab = 7;
        m.popup_dup_skipped = 2;
        m.popup_delivered_bytes = 471 * 34 * 4;
        let p = PopupDeliveryCounts { frames: 5, deliveries: 9 };
        let a = m.format_latency_section(p);
        let b = m.format_latency_section(p);
        assert_eq!(a, b, "format_latency_section 必须零副作用");
        assert!(a.contains("keyboard_input"), "{a}");
        assert!(a.contains("submit_key_sync"), "{a}");
        assert!(a.contains("im2-grab=7"), "{a}");
        assert!(a.contains("Starting期(词典窗口)=0"), "{a}");
        assert!(a.contains("重复跳过=2"), "{a}");
        assert!(a.contains("交付字节=62KB"), "{a}"); // 471*34*4=64056B→62KB
        assert!(a.contains("deliveries=9"), "{a}");
        // 纯读：不得推进窗口簿记（tick_window_report 才是写侧）。
        assert_eq!(m.update_rounds, 0);
    }

    #[test]
    fn log_io_counters_snapshot_and_suppressed_paths() {
        let log = LogIoCounters::new();
        log.add_line(64);
        log.add_write_op();
        log.add_suppressed();
        log.add_suppressed();
        assert_eq!(log.snapshot(), (1, 64, 1, 2));
    }
}
