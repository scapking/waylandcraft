package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.Set;
import java.util.UUID;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P1-2 修复证明：慢观察者判定纯逻辑（跳帧 episode 计数）。
 * relay 线程的 isWritable 读取依赖 netty channel（难 mock，已在
 * RelaySinks.channelOf 注释说明），这里以注入时钟验证判定层：
 * 连续跳过 >1s 记且只记一次，恢复后可开启新 episode。
 */
class SlowViewerTrackerTest {

	private final UUID player = UUID.randomUUID();

	@Test
	@DisplayName("首次跳帧不记；持续不可写超过 1s 记一次；episode 内不重复记")
	void reportsOncePerEpisodeAfterOneSecond() {
		SlowViewerTracker tracker = new SlowViewerTracker();

		assertFalse(tracker.observeUnwritable(player, 0L), "t=0 首次跳过，不算慢");
		assertFalse(tracker.observeUnwritable(player, 500L), "t=0.5s 仍 <1s");
		assertTrue(tracker.observeUnwritable(player, 1000L), "t=1s 达阈值，记 slow_viewer");
		assertFalse(tracker.observeUnwritable(player, 1500L), "同 episode 不重复计数");
		assertFalse(tracker.observeUnwritable(player, 10_000L), "同 episode 不重复计数");
	}

	@Test
	@DisplayName("channel 恢复可写结束 episode；再次劣化可开新 episode 并重新计时")
	void writableResetStartsFreshEpisode() {
		SlowViewerTracker tracker = new SlowViewerTracker();
		tracker.observeUnwritable(player, 0L);
		assertTrue(tracker.observeUnwritable(player, 1200L));

		tracker.observeWritable(player);

		assertFalse(tracker.observeUnwritable(player, 2000L), "新 episode 从头计");
		assertFalse(tracker.observeUnwritable(player, 2999L), "差 1ms 未达阈值");
		assertTrue(tracker.observeUnwritable(player, 3200L), "新 episode 超 1s 再记一次");
	}

	@Test
	@DisplayName("玩家下线（retainOnly 剔除）后残留状态清零")
	void retainOnlyPrunesOfflineEpisodes() {
		SlowViewerTracker tracker = new SlowViewerTracker();
		UUID ghost = UUID.randomUUID();
		tracker.observeUnwritable(player, 0L);
		tracker.observeUnwritable(ghost, 0L);
		assertEquals(2, tracker.activeEpisodes());

		tracker.retainOnly(Set.of(player));
		assertEquals(1, tracker.activeEpisodes(), "不在场玩家的 episode 必须清掉，防泄漏");
	}

	@Test
	@DisplayName("多玩家独立判定互不干扰")
	void tracksPlayersIndependently() {
		SlowViewerTracker tracker = new SlowViewerTracker();
		UUID fast = UUID.randomUUID();
		tracker.observeUnwritable(player, 0L);
		assertFalse(tracker.observeUnwritable(fast, 900L));
		assertTrue(tracker.observeUnwritable(player, 1000L));
		assertFalse(tracker.observeUnwritable(fast, 1500L));
		assertTrue(tracker.observeUnwritable(fast, 1901L), "fast 从 900 起算 >1s");
	}
}
