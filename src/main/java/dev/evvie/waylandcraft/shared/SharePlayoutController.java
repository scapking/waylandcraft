package dev.evvie.waylandcraft.shared;

import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.utils.WcLogging;

/**
 * ⑧ v1.4.8 —— 共享窗口 playout 统一时钟 + 负载自适应缓冲深度。
 *
 * 解决两个用户实测问题：
 *
 * 1. 音视频不同步（"视频已经出现，音频和视频不在同一个发送事件"）。
 *    旧模型：视频流与音频流各自维护独立的 minTransit 估计。两轨的
 *    "捕获→到达"下界天然不同（视频含 JPEG/PNG 编码+大小保护重编码，
 *    音频只有 poll+中继），dueAt = captureTs + 各自的估计 + D →
 *    同一内容时刻两轨播放时刻相差 (minTransit_video − minTransit_audio)，
 *    永久错拍。本类把估计合并为**每窗口一个共享时钟**：两轨的 transit
 *    样本喂同一个 min-tracking 估计，dueAt = captureTs + X(窗口) + D ——
 *    两轨只差内容时刻本身，编码耗时差异被缓冲深度 D 吸收而非转成错拍。
 *
 * 2. 固定 4–8s 预缓冲太短（弱网/中继批量到达时仍卡）。用户裁定：
 *    按负载自动延伸，下限 14s、上限 32s（延迟换流畅，尺寸/分辨率绝不动）。
 *    自适应只在"拥塞证据出现"时加深、连续 30s 干净才回落，带驻留防振荡。
 *
 * 设置语义（sharePlayoutDelayMs，单值同时门控两轨）：
 * - -1 = AUTO：按本类自适应深度（14000..32000ms，初始 14000）；
 * -  0 = 低延迟旧行为（单槽覆盖/直接排队），完全不变；
 * - >0 = 手动档（毫秒，CLI 校验到 32000），本类不参与。
 *
 * 线程模型：
 * - 共享时钟 observeTransit/transitMs：netty→主线程（音频）与主线程（视频）并发调用，
 *   ClockState 用 synchronized 保护（每包一次，竞争可忽略）；
 * - 自适应状态：仅渲染线程 RemoteWindowRenderer.tick() 1Hz 驱动 sample()，
 *   读取方（两轨到点判断）读 volatile 快照。
 */
public final class SharePlayoutController {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-playout");
	
	/** sharePlayoutDelayMs 的自动档哨兵值 */
	public static final int AUTO = -1;
	
	/** 自适应深度范围与初始值（用户裁定：4–8s 太短，至少 14s，可到 32s） */
	static final long AUTO_START_MS = 14_000;
	static final long AUTO_MIN_MS = 14_000;
	static final long AUTO_MAX_MS = 32_000;
	/** 手动档上限（CLI 校验与此一致） */
	public static final long MANUAL_MAX_MS = 32_000;
	
	/** 升档：每步 +2s，需连续 RAISE_STREAK 个拥塞采样；升后 HOLD 期内禁止再动 */
	static final long RAISE_STEP_MS = 2_000;
	static final long LOWER_STEP_MS = 1_000;
	static final int RAISE_STREAK = 3;
	static final long LOWER_QUIET_MS = 30_000;
	static final long HOLD_AFTER_RAISE_MS = 10_000;
	/** 拥塞判定阈值（score ≥ 此值算一个拥塞采样） */
	static final double CONGESTED_SCORE = 0.35;
	
	/** 共享时钟向上漂移速率（ms/观测）：容忍发送端变慢，抗突发尖峰拉高估计 */
	static final double TRANSIT_DRIFT_MS = 3.0;
	/** 时钟空闲过期（与 RemoteWindowRenderer 流回收同量级） */
	static final long CLOCK_EXPIRE_MS = 60_000;
	
	/** 缓冲容量：压缩字节地板/天花板（内存安全，绝不 OOM）与全局总额上限 */
	static final long VIDEO_CAP_FLOOR_BYTES = 48L * 1024 * 1024;
	static final long VIDEO_CAP_CEIL_BYTES = 192L * 1024 * 1024;
	/** 容量 = 目标深度 × 实测码率 × HEADROOM（欠载重同步需要余量） */
	static final double VIDEO_CAP_HEADROOM = 2.5;
	/** 所有窗口压缩缓冲总额上限（超限时各流丢最旧） */
	public static final long VIDEO_TOTAL_CAP_BYTES = 384L * 1024 * 1024;
	/** 音频 pending 队列深度：目标 + 4s 余量，100ms/包，钳制 [300, 400] */
	static final int AUDIO_PENDING_FLOOR = 300;
	static final int AUDIO_PENDING_CEIL = 400;
	static final long AUDIO_PENDING_MARGIN_MS = 4_000;
	
	// ===================== 每窗口共享 playout 时钟 =====================
	
	private static final class ClockState {
		long minTransitMs = -1;   // -1 = 尚无观测
		long lastObservedMs;
	}
	
	private static final Map<Long, ClockState> clocks = new ConcurrentHashMap<>();
	
	/**
	 * 观测一次 transit（本地到达 − 发送端 captureTs，调用方已钳 ≥0）。
	 * 视频与音频两轨都喂这里 —— 同一窗口共享一个估计，音画同锚。
	 */
	public static void observeTransit(long windowHandle, long transitMs, long now) {
		ClockState c = clocks.computeIfAbsent(windowHandle, k -> new ClockState());
		synchronized(c) {
			c.lastObservedMs = now;
			c.minTransitMs = c.minTransitMs < 0
				? transitMs
				: Math.min(transitMs, (long)(c.minTransitMs + TRANSIT_DRIFT_MS));
		}
	}
	
	/**
	 * 窗口当前传输下界估计（ms）；尚无观测返回 -1（调用方按 0 偏移处理，
	 * 首包立即入缓冲、由后续观测收敛——与旧逐轨行为一致）。
	 */
	public static long transitMs(long windowHandle) {
		ClockState c = clocks.get(windowHandle);
		if(c == null) return -1;
		synchronized(c) {
			return c.minTransitMs;
		}
	}
	
	/** 窗口注销/流回收：丢时钟 */
	public static void forgetWindow(long windowHandle) {
		clocks.remove(windowHandle);
	}
	
	/** 过期时钟清扫（空闲 > 60s；挂在 sample() 上顺带跑） */
	static int sweepExpiredClocks(long now) {
		int removed = 0;
		for(Map.Entry<Long, ClockState> e : clocks.entrySet()) {
			ClockState c = e.getValue();
			boolean expired;
			synchronized(c) {
				expired = now - c.lastObservedMs > CLOCK_EXPIRE_MS;
			}
			if(expired && clocks.remove(e.getKey(), c)) removed++;
		}
		return removed;
	}
	
	/** 断线重连/整体复位：丢弃所有窗口时钟（生产路径 + 测试接缝共用） */
	public static void forgetAllWindows() {
		clocks.clear();
	}
	
	// ===================== 深度解析（单一入口，两轨共用） =====================
	
	/**
	 * 解析当前生效的预缓冲深度。settings 值语义：-1=AUTO（自适应目标）、
	 * 0=低延迟、>0=手动。调用点即旧的 RemoteWindowRenderer.playoutDelayMs()。
	 */
	public static long resolveDelayMs(int settingMs) {
		if(settingMs == AUTO) return targetMs();
		return settingMs < 0 ? AUTO_START_MS : settingMs;
	}
	
	/** 当前自适应目标（volatile 快照读） */
	public static long targetMs() {
		return snapshot.targetMs;
	}
	
	// ===================== 自适应状态机 =====================
	
	/** 控制器状态快照（不可变；写者仅渲染线程 1Hz） */
	record ControllerState(long targetMs, int congestionStreak, long goodSinceMs, long holdUntilMs) {}
	
	private static volatile ControllerState snapshot =
		new ControllerState(AUTO_START_MS, 0, 0, 0);
	
	/** 一次 1s 采样的拥塞证据（增量为评估周期内计数差；fill 为缓冲占用比） */
	record PlayoutMetrics(long videoCapDroppedDelta, long videoDecodeStallDelta,
			long audioStaleDroppedDelta, double maxBufferFillRatio) {}
	
	/**
	 * 纯函数：拥塞分数 ∈ [0,1]。取各信号的强度最大值（不是求和——
	 * 任何一个硬证据都足以支持加深；分数只用于迟滞判断）。
	 * 强度序：容量截断丢帧（缓冲比目标浅，加深直接有效）= 0.9 >
	 * 解码积压卡死 / 音频迟到丢弃（播放跟不上）= 0.6 >
	 * 缓冲占用 ≥85%（快满了）= 0.4·ratio。
	 */
	static double congestionScore(PlayoutMetrics m) {
		double score = 0.0;
		if(m.videoCapDroppedDelta() > 0) score = Math.max(score, 0.9);
		if(m.videoDecodeStallDelta() > 0) score = Math.max(score, 0.6);
		if(m.audioStaleDroppedDelta() > 0) score = Math.max(score, 0.6);
		if(m.maxBufferFillRatio() >= 0.85) score = Math.max(score, 0.4 * m.maxBufferFillRatio());
		return score;
	}
	
	/**
	 * 纯函数：迟滞状态机一步。nowMs/goodSince/holdUntil 全注入，单测可打。
	 * 升档条件：连续 RAISE_STREAK 个拥塞采样且过了驻留期 → +RAISE_STEP（封顶 AUTO_MAX）。
	 * 降档条件：非拥塞且已连续 LOWER_QUIET_MS 干净且过了驻留期 → −LOWER_STEP（封底 AUTO_MIN）。
	 * 干净计数器 goodSinceMs 在每次拥塞时重置为当前时刻（"连续"语义）。
	 */
	static ControllerState nextState(ControllerState s, double score, long nowMs) {
		long target = s.targetMs();
		long holdUntil = s.holdUntilMs();
		long goodSince = s.goodSinceMs();
		int streak;
		if(score >= CONGESTED_SCORE) {
			streak = s.congestionStreak() + 1;
			goodSince = nowMs;
			if(streak >= RAISE_STREAK && nowMs >= holdUntil && target < AUTO_MAX_MS) {
				target = Math.min(AUTO_MAX_MS, target + RAISE_STEP_MS);
				holdUntil = nowMs + HOLD_AFTER_RAISE_MS;
				LOGGER.info("[playout] adaptive depth raised to {}ms (congestion score {})", target, String.format("%.2f", score));
			}
			streak = Math.min(streak, RAISE_STREAK);
		}
		else if(score <= 0.05) {
			streak = 0;
			if(goodSince == 0) goodSince = nowMs;
			if(target > AUTO_MIN_MS && nowMs - goodSince >= LOWER_QUIET_MS && nowMs >= holdUntil) {
				target = Math.max(AUTO_MIN_MS, target - LOWER_STEP_MS);
				holdUntil = nowMs + HOLD_AFTER_RAISE_MS / 2;
				LOGGER.info("[playout] adaptive depth relaxed to {}ms ({}/{}ms quiet)",
					target, LOWER_QUIET_MS, nowMs - goodSince);
			}
		}
		else {
			// 中间地带（0.05..0.35）：不升不降，但打断"连续干净"与升档连击
			streak = 0;
		}
		return new ControllerState(target, streak, goodSince, holdUntil);
	}
	
	/**
	 * 渲染线程 1Hz 驱动（RemoteWindowRenderer.tick 采样调用）。
	 * 顺带清扫过期时钟；verbose 档输出每次采样的分数明细。
	 */
	public static void sample(long nowMs, PlayoutMetrics m) {
		sweepExpiredClocks(nowMs);
		double score = congestionScore(m);
		snapshot = nextState(snapshot, score, nowMs);
		if(WcLogging.verbose()) {
			LOGGER.debug("[playout] sample score={} target={} capDrop={} stall={} staleDrop={} fill={}",
				String.format("%.2f", score), snapshot.targetMs(), m.videoCapDroppedDelta(),
				m.videoDecodeStallDelta(), m.audioStaleDroppedDelta(), String.format("%.2f", m.maxBufferFillRatio()));
		}
	}
	
	/** 测试/诊断接缝：读/写控制器状态 */
	static ControllerState stateForTest() {
		return snapshot;
	}
	
	static void resetForTest() {
		snapshot = new ControllerState(AUTO_START_MS, 0, 0, 0);
		clocks.clear();
	}
	
	// ===================== 容量公式（自适应深度的内存配套） =====================
	
	/**
	 * 纯函数：单窗口压缩缓冲字节上限 = 目标深度 × 实测码率 × headroom，
	 * 钳制 [48MB, 192MB]。码率 EMA ≤0（尚无观测）时取地板值。
	 * 旧实现固定 48MB：32s 深度 × 高码率会持续触发"超限丢最旧"，
	 * 缓冲被钳浅反而更卡——自适应深度必须有配套容量，否则自伤。
	 */
	static long videoBufferCapBytes(long targetMs, long rateEmaBytesPerSec) {
		if(rateEmaBytesPerSec <= 0) return VIDEO_CAP_FLOOR_BYTES;
		double need = (double)targetMs / 1000.0 * rateEmaBytesPerSec * VIDEO_CAP_HEADROOM;
		if(need <= VIDEO_CAP_FLOOR_BYTES) return VIDEO_CAP_FLOOR_BYTES;
		if(need >= VIDEO_CAP_CEIL_BYTES) return VIDEO_CAP_CEIL_BYTES;
		return (long)need;
	}
	
	/** 纯函数：音频 pending 队列 chunk 上限（100ms/包 + 4s 余量，钳 [300,400]） */
	static int audioPendingChunks(long targetMs) {
		long need = (targetMs + AUDIO_PENDING_MARGIN_MS) / 100;
		if(need <= AUDIO_PENDING_FLOOR) return AUDIO_PENDING_FLOOR;
		if(need >= AUDIO_PENDING_CEIL) return AUDIO_PENDING_CEIL;
		return (int)need;
	}
}
