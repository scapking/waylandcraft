package dev.evvie.waylandcraft.network;

import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

/**
 * P1-2：慢观察者（slow viewer）追踪。
 *
 * 背压跳帧以"连续跳过时长"判定：某玩家 channel 从 firstSkipAt 起持续不可写
 * 超过 SLOW_THRESHOLD_MS（1s）记一次 slow_viewer 事件（每个连续episode只记一次，
 * 避免 10Hz 判定 × 多窗口重复计数）。channel 重新可写即视为恢复，episode 结束。
 *
 * 时钟显式传入（nowMs），纯逻辑可单测；多转发线程并发调用，方法级同步，
 * 参与者数量级为玩家数，锁竞争可忽略。
 */
public final class SlowViewerTracker {

	/** 连续跳过超过该时长即计一次 slow_viewer */
	static final long SLOW_THRESHOLD_MS = 1000;

	/** 已上报 episode 的哨兵值（等重新可写时清除） */
	private static final long REPORTED = Long.MIN_VALUE;

	private final Map<UUID, Long> episodeStart = new ConcurrentHashMap<>();

	/**
	 * 观察到该玩家本轮因不可写被跳帧。
	 * @return true 表示"连续跳过刚越过 1s 阈值"，调用方应把 slow_viewer 计数 +1
	 */
	public synchronized boolean observeUnwritable(UUID player, long nowMs) {
		Long start = episodeStart.get(player);
		if (start == null) {
			episodeStart.put(player, nowMs);
			return false;
		}
		if (start == REPORTED) {
			return false;
		}
		if (nowMs - start >= SLOW_THRESHOLD_MS) {
			episodeStart.put(player, REPORTED);
			return true;
		}
		return false;
	}

	/** 玩家 channel 重新可写：episode 结束。 */
	public synchronized void observeWritable(UUID player) {
		episodeStart.remove(player);
	}

	/** 下线/清理：删除不在 currentOnline 集合中的玩家 episode 状态，防泄漏。 */
	public synchronized void retainOnly(java.util.Set<UUID> currentOnline) {
		episodeStart.keySet().retainAll(currentOnline);
	}

	/** 当前处于跳过 episode 的玩家数（诊断用）。 */
	public synchronized int activeEpisodes() {
		return episodeStart.size();
	}

	public synchronized void clear() {
		episodeStart.clear();
	}
}
