package dev.evvie.waylandcraft.network;

import java.util.HashMap;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

/**
 * 观众负载分级控制器（overload-guard：观众过多/慢观众自动降载）。
 *
 * 在既有背压链（RelaySinks.isWritable 跳帧 + SlowViewerTracker 1s episode 计数）
 * 之上增量升级：把"持续跳帧的观众"分成 4 级并按级收缩推送，避免其连接长期处于
 * 跳帧状态还持续占用转发与出站内存：
 *
 * - L0（normal）全量：视频 20fps + 音频全量。
 * - L1（half）帧率减半：视频隔帧放行（约 10fps），音频全量。
 * - L2（keyframe）只发关键帧：视频每 KEYFRAME_INTERVAL_MS（1s）放一帧心跳保画面
 *   可见（帧缓存只留最新帧，心跳天然是"当前最新画面"而非过期帧）；音频隔包放行。
 * - L3（paused）暂停推送：视频/音频全部跳过。保留订阅与权限，画面冻结在最后
 *   一帧；恢复时因缓存只存最新帧，观众自然追上最新画面（无需额外追赶逻辑）。
 *
 * 迟滞（防抖）：
 * - 升级（降载）：目标级别需在 DEMOTE_HOLD_MS 内持续成立，且逐级 +1（0→1→2→3），
 *   让每级减载先发挥作用再升级，不跳级。
 * - 降级（恢复）：需 PROMOTE_HOLD_MS 内持续成立，允许直接跳回目标级，尽快恢复体验。
 * - 目标级别抖动会重置计时（pendingSinceMs），抖动永远凑不满 hold 时长。
 *
 * 压力观测：10s 滚动统计窗（tumbling window）内的 unwritable 采样时长与
 * SlowViewerTracker 的 1s episode 事件数（见 targetLevel）。
 *
 * 线程模型：视频/音频两个 relay 的转发线程并发调用；unwritable 是连接级属性，
 * 两个 relay 的观测合并进同一份 per-viewer 状态（避免同一观众被双重分级）。
 * 方法级同步，参与者数量级 = 在线观众数，锁竞争可忽略。
 *
 * 纯策略函数（targetLevel/advance/shouldSendVideo/shouldSendAudio）无状态、时钟
 * 显式注入，可脱离 MC 单测（见 ViewerLoadPolicyTest）。
 */
public final class ViewerLoadController {

	public static final int LEVEL_NORMAL = 0;
	public static final int LEVEL_HALF_RATE = 1;
	public static final int LEVEL_KEYFRAME = 2;
	public static final int LEVEL_PAUSED = 3;

	/** 压力观测窗（tumbling）：窗内累计 unwritable 时长/事件数，窗口推进即清零 */
	static final long OBS_WINDOW_MS = 10_000;
	/** 升级迟滞：目标级别需持续成立该时长才开始逐级 +1 */
	static final long DEMOTE_HOLD_MS = 3_000;
	/** 降级迟滞：压力需持续消退该时长才允许跳回目标级 */
	static final long PROMOTE_HOLD_MS = 5_000;
	/** L2 关键帧心跳间隔（约 1fps） */
	static final long KEYFRAME_INTERVAL_MS = 1_000;
	/**
	 * 单次 unwritable 采样计入的最大时长。同一观众同一时刻可能在多个窗口的
	 * 转发循环里被并发观测（channel 不可写是全局事实），不设上限会把并发采样
	 * 的间隙重复计入；真实观测间隔（relay 50ms 批次）远小于该值。
	 */
	static final long MAX_SAMPLE_GAP_MS = 250;

	/** per-viewer 运行时状态（全部字段只在 synchronized(states) 内读写） */
	private static final class ViewerState {
		int level = LEVEL_NORMAL;
		int pendingLevel = LEVEL_NORMAL;
		long pendingSinceMs = 0;
		long windowStartMs = 0;
		long unwritableMs = 0;
		int unwritableEvents = 0;
		long lastUnwritableMs = -1;
		boolean videoToggle = false;
		boolean audioToggle = false;
		long lastVideoSentMs = 0;
	}

	private static final Map<UUID, ViewerState> states = new ConcurrentHashMap<>();

	private ViewerLoadController() {}

	// ===== 纯策略（无状态，时钟显式注入，可单测） =====

	/**
	 * 观测窗内压力 → 目标级别（不含迟滞）。
	 * 两条输入通道：unwritable 时长占比（连续压力）与 1s episode 事件数（反复抖动）。
	 * 占比口径：10s 窗内 ≥20% 跳帧即减半、≥50% 只发心跳、≥80% 暂停；
	 * 事件口径：窗内 ≥2 次独立 >1s episode 减半、≥4 次只发心跳。
	 */
	static int targetLevel(long windowMs, long unwritableMs, int events) {
		if (windowMs <= 0) return LEVEL_NORMAL;
		double fraction = (double) unwritableMs / windowMs;
		if (fraction >= 0.8) return LEVEL_PAUSED;
		if (fraction >= 0.5 || events >= 4) return LEVEL_KEYFRAME;
		if (fraction >= 0.2 || events >= 2) return LEVEL_HALF_RATE;
		return LEVEL_NORMAL;
	}

	/** 迟滞状态机一步的不可变结果（level + pending 快照） */
	record LevelStep(int level, int pending, long pendingSinceMs) {}

	/**
	 * 迟滞状态机一步（纯函数）：target 变化重置 pending 计时；
	 * 升级逐级 +1、每次迁移后重置计时（每级都要计满 DEMOTE_HOLD_MS），
	 * 降级直达目标（需计满 PROMOTE_HOLD_MS）。
	 */
	static LevelStep advance(int current, int pending, long pendingSinceMs, int target, long nowMs) {
		int nextPending = pending;
		long nextSince = pendingSinceMs;
		if (target != pending) {
			nextPending = target;
			nextSince = nowMs;
		}
		int next = current;
		if (nextPending > next) {
			if (nowMs - nextSince >= DEMOTE_HOLD_MS) {
				next = Math.min(next + 1, nextPending);
				nextSince = nowMs;
			}
		} else if (nextPending < next) {
			if (nowMs - nextSince >= PROMOTE_HOLD_MS) {
				next = nextPending;
				nextSince = nowMs;
			}
		}
		return new LevelStep(next, nextPending, nextSince);
	}

	/**
	 * level → 本帧是否放行（视频）。toggle/lastVideoSentMs 的翻转由
	 * {@link #trySendVideo} 在实际放行时提交，纯函数只做判定。
	 */
	static boolean shouldSendVideo(int level, boolean lastToggle, long nowMs, long lastVideoSentMs) {
		return switch (level) {
			case LEVEL_NORMAL -> true;
			case LEVEL_HALF_RATE -> !lastToggle;
			case LEVEL_KEYFRAME -> nowMs - lastVideoSentMs >= KEYFRAME_INTERVAL_MS;
			default -> false;
		};
	}

	/** level → 本音频包是否放行（L0/L1 全量，L2 隔包，L3 暂停）。 */
	static boolean shouldSendAudio(int level, boolean lastToggle) {
		return switch (level) {
			case LEVEL_NORMAL, LEVEL_HALF_RATE -> true;
			case LEVEL_KEYFRAME -> !lastToggle;
			default -> false;
		};
	}

	// ===== 运行时观测（视频/音频转发线程并发调用） =====

	/**
	 * 观测到该观众因 channel 不可写被跳帧。thresholdEvent 传
	 * SlowViewerTracker.observeUnwritable 的返回值（连续 >1s 的 episode 计数）。
	 */
	public static void observeUnwritable(UUID viewer, long nowMs, boolean thresholdEvent) {
		if (viewer == null) return;
		synchronized (states) {
			ViewerState st = states.computeIfAbsent(viewer, k -> new ViewerState());
			if (nowMs - st.windowStartMs >= OBS_WINDOW_MS) {
				st.windowStartMs = nowMs;
				st.unwritableMs = 0;
				st.unwritableEvents = 0;
			}
			if (st.lastUnwritableMs >= 0) {
				long gap = Math.min(nowMs - st.lastUnwritableMs, MAX_SAMPLE_GAP_MS);
				if (gap > 0) st.unwritableMs += gap;
			}
			if (thresholdEvent) st.unwritableEvents++;
			st.lastUnwritableMs = nowMs;
			applyTarget(st, nowMs);
		}
	}

	/**
	 * 观测到该观众 channel 可写（每个发送机会都观测——分级暂停中的观众也要持续
	 * 探测可写性，否则永远拿不到恢复证据）。可写不累积压力；episode 由时间窗与
	 * 采样上限自然闭合（两次 unwritable 间隔超过 MAX_SAMPLE_GAP_MS 不再连算）。
	 */
	public static void observeWritable(UUID viewer, long nowMs) {
		if (viewer == null) return;
		synchronized (states) {
			ViewerState st = states.get(viewer);
			if (st == null) return;
			if (nowMs - st.windowStartMs >= OBS_WINDOW_MS) {
				st.windowStartMs = nowMs;
				st.unwritableMs = 0;
				st.unwritableEvents = 0;
			}
			applyTarget(st, nowMs);
		}
	}

	private static void applyTarget(ViewerState st, long nowMs) {
		long windowMs = Math.max(1, nowMs - st.windowStartMs);
		int target = targetLevel(windowMs, st.unwritableMs, st.unwritableEvents);
		LevelStep step = advance(st.level, st.pendingLevel, st.pendingSinceMs, target, nowMs);
		st.level = step.level();
		st.pendingLevel = step.pending();
		st.pendingSinceMs = step.pendingSinceMs();
	}

	/**
	 * 本帧是否允许发给该观众（视频）。无观测记录的观众 = 全量服务
	 * （fail-open，与 RelaySinks 未知即放行同哲学）。
	 * 注意：L1 的隔帧 toggle 必须在每个发送机会上都翻转——只在放行时翻转会让
	 * toggle 卡在 true 永远跳帧。
	 */
	public static boolean trySendVideo(UUID viewer, long nowMs) {
		if (viewer == null) return true;
		synchronized (states) {
			ViewerState st = states.get(viewer);
			if (st == null) return true;
			boolean allow = shouldSendVideo(st.level, st.videoToggle, nowMs, st.lastVideoSentMs);
			if (st.level == LEVEL_HALF_RATE) st.videoToggle = !st.videoToggle;
			if (allow && st.level == LEVEL_KEYFRAME) st.lastVideoSentMs = nowMs;
			return allow;
		}
	}

	/** 本音频包是否允许发给该观众（L2 隔包 toggle 同样每次机会翻转）。 */
	public static boolean trySendAudio(UUID viewer) {
		if (viewer == null) return true;
		synchronized (states) {
			ViewerState st = states.get(viewer);
			if (st == null) return true;
			boolean allow = shouldSendAudio(st.level, st.audioToggle);
			if (st.level == LEVEL_KEYFRAME) st.audioToggle = !st.audioToggle;
			return allow;
		}
	}

	/** 当前分级（无记录 = L0）。 */
	public static int levelOf(UUID viewer) {
		ViewerState st = states.get(viewer);
		return st == null ? LEVEL_NORMAL : st.level;
	}

	/** 级别名（诊断报告用） */
	public static String levelName(int level) {
		return switch (level) {
			case LEVEL_NORMAL -> "normal";
			case LEVEL_HALF_RATE -> "half";
			case LEVEL_KEYFRAME -> "keyframe";
			case LEVEL_PAUSED -> "paused";
			default -> "unknown";
		};
	}

	/** 逐观众分级快照（诊断报告用；副本，调用方可安全遍历） */
	public static Map<UUID, Integer> snapshot() {
		Map<UUID, Integer> out = new HashMap<>();
		for (Map.Entry<UUID, ViewerState> e : states.entrySet()) {
			out.put(e.getKey(), e.getValue().level);
		}
		return out;
	}

	/** 下线观众状态清理（防 map 随玩家进出泄漏），relay tick 调用。 */
	public static void retainOnly(Set<UUID> currentOnline) {
		states.keySet().retainAll(currentOnline);
	}

	public static void clear() {
		states.clear();
	}
}
