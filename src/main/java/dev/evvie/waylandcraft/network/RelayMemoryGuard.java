package dev.evvie.waylandcraft.network;

import java.util.function.LongSupplier;

/**
 * 全局内存守卫（overload-guard：所有窗口转发缓冲字节总和上限）。
 *
 * 背景：per-window 限额已存在——视频为"每窗口 1 帧最新缓存"（≤MAX_FRAME_BYTES），
 * 音频为"每窗口 50 包 FIFO"。两者独立生效时，总缓冲随窗口数线性放大
 * （视频 ≈ 窗口数 × 1.9MB，音频 ≈ 窗口数 × 3.2MB 理论上界），窗口数 × 观众数
 * 叠加正是"观众过多内存打满"的路径。本守卫补一道全局总量上限：
 *
 * - 缓冲字节按需推导（视频 = relay 最新帧缓存求和，音频 = pending FIFO 求和），
 *   不维护增量计数器——消除"accept/evict/drain 路径配对遗漏导致计数永久漂移"
 *   的风险；求和开销 O(窗口数 × 队列深度)，量级 < 数千包，可忽略。
 * - 视频准入（替换语义）：现有缓存 − 该窗口旧帧 + 新帧 ≤ 上限，超限拒收新帧
 *   （旧帧保留 = 该窗口画面原地停留一拍），宁可不更新也不挤占其他窗口预算。
 * - 音频准入：超限先丢"当前最重队列的最旧包"（全局公平，而非单窗口挨饿），
 *   守不住再拒新包。取舍说明：音频丢旧听感无损（既有策略保实时不保完整），
 *   视频丢新帧只损失一拍更新（下一帧会重新准入），两者都比 OOM 崩服可接受。
 * - 出站联动：isOverLimit() 为真时，转发层对已降级观众（level ≥ 1）直接跳帧
 *   ——取舍：低优先级观众的 channel 大概率本来就不可写（背压同样会跳帧），
 *   危机时刻优先保全 L0 观众与 owner 链路；被跳过的观众在分级恢复 L0 后
 *   自动重新获得全量转发（见 ViewerLoadController）。
 *
 * 并发说明：accept 在 netty 线程、drain 在服务端线程，求和为弱一致快照，
 * 双线程同时准入可能短暂略超上限——守卫是背压上界而非精确配额，偏差可接受；
 * 拒绝/驱逐路径全部有界（单包 ≤64KB ≪ 上限），不会死循环。
 *
 * 纯判定函数（videoAcceptAllowed/audioAcceptAllowed/isOverLimit(long,long)）
 * 无状态可单测（见 RelayMemoryGuardTest）。
 */
public final class RelayMemoryGuard {

	/**
	 * 全局转发缓冲字节上限（视频最新帧缓存 + 音频 FIFO 总和）。
	 * 64MB ≈ 现有 per-window 限额理论峰值（24 窗 × 1.9MB + 24 窗 × 3.2MB ≈ 122MB）
	 * 的一半，同时远大于正常运行的瞬态占用（每 tick 排空）。
	 */
	public static final long GLOBAL_BUFFER_BYTES_LIMIT = 64L << 20; // 64MB

	/** 视频缓存字节来源（由 SharedWindowFrameRelay 构造时绑定） */
	private static volatile LongSupplier videoSource = () -> 0L;
	/** 音频 FIFO 字节来源（由 AudioFrameRelay 构造时绑定） */
	private static volatile LongSupplier audioSource = () -> 0L;

	private RelayMemoryGuard() {}

	static void bindVideoSource(LongSupplier supplier) {
		videoSource = supplier;
	}

	static void bindAudioSource(LongSupplier supplier) {
		audioSource = supplier;
	}

	/** 当前视频最新帧缓存总字节（来源异常时按 0 处理，不阻断转发） */
	public static long videoBufferedBytes() {
		try {
			return videoSource.getAsLong();
		} catch (Throwable t) {
			return 0L;
		}
	}

	/** 当前音频 FIFO 缓冲总字节 */
	public static long audioBufferedBytes() {
		try {
			return audioSource.getAsLong();
		} catch (Throwable t) {
			return 0L;
		}
	}

	/** 当前转发缓冲总字节（视频 + 音频） */
	public static long totalBufferedBytes() {
		return videoBufferedBytes() + audioBufferedBytes();
	}

	/** 纯函数：给定视频/音频占用是否越过全局上限 */
	public static boolean isOverLimit(long videoBytes, long audioBytes) {
		return videoBytes + audioBytes > GLOBAL_BUFFER_BYTES_LIMIT;
	}

	/** 运行时判定：当前总占用是否越过全局上限（转发层出站联动用） */
	public static boolean isOverLimit() {
		return isOverLimit(videoBufferedBytes(), audioBufferedBytes());
	}

	/**
	 * 纯函数（可单测）：视频帧替换语义下的准入判定。
	 * currentTotal 含该窗口旧帧（replacedLen），准入后旧帧被覆盖。
	 */
	public static boolean videoAcceptAllowed(long currentTotal, long replacedLen, long incomingLen) {
		return currentTotal - replacedLen + incomingLen <= GLOBAL_BUFFER_BYTES_LIMIT;
	}

	/** 纯函数（可单测）：音频包准入判定（驱逐后仍需满足） */
	public static boolean audioAcceptAllowed(long audioTotal, long incomingLen) {
		return audioTotal + incomingLen <= GLOBAL_BUFFER_BYTES_LIMIT;
	}
}
