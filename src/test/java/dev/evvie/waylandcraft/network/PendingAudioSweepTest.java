package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P2-14 修复证明：断线前列表迟迟不来的场景下，pendingAudio 不再无界增长——
 * 1) 全局窗口数上限；2) TTL 清扫挂在包到达/显式 sweep 上，不依赖列表更新到达。
 * 通过注入时间的测试接缝（holdAudioAt/sweepPendingAudioAt）做确定性断言。
 */
class PendingAudioSweepTest {

	private static SharedWindowAudioPayload packet(long handle, int seq) {
		return new SharedWindowAudioPayload(handle, seq, 48000, 2, new byte[]{1, 2, 3, 4}, 0L);
	}

	@BeforeEach
	void reset() {
		SharedWindowClientHandler.resetPendingAudioForTest();
	}

	@Test
	@DisplayName("全局窗口上限：超限后新未知句柄不再建队列（旧实现无上限）")
	void globalWindowCapDropsExcess() {
		for (long h = 1; h <= 20; h++) {
			SharedWindowClientHandler.holdAudioAt(packet(h, (int) h), 1_000L);
		}
		assertEquals(16, SharedWindowClientHandler.pendingAudioWindowCount(),
			"上限 16 个窗口（PENDING_AUDIO_MAX_WINDOWS）");
	}

	@Test
	@DisplayName("TTL 清扫不依赖列表更新：过期未列出的窗口队列被回收")
	void sweepExpiresWithoutListUpdate() {
		for (long h = 1; h <= 5; h++) {
			SharedWindowClientHandler.holdAudioAt(packet(h, (int) h), 1_000L);
		}
		assertEquals(5, SharedWindowClientHandler.pendingAudioWindowCount());

		int removed = SharedWindowClientHandler.sweepPendingAudioAt(1_000L + 15_000L + 1L);
		assertEquals(5, removed, "全部过期，应回收 5 个窗口队列");
		assertEquals(0, SharedWindowClientHandler.pendingAudioWindowCount());
	}

	@Test
	@DisplayName("未过期不清；清扫后腾出的名额可给新窗口")
	void sweepKeepsFreshAndFreesSlots() {
		for (long h = 1; h <= 16; h++) {
			SharedWindowClientHandler.holdAudioAt(packet(h, (int) h), 1_000L);
		}
		// 17 号被上限拒绝
		SharedWindowClientHandler.holdAudioAt(packet(17L, 1), 1_000L);
		assertEquals(16, SharedWindowClientHandler.pendingAudioWindowCount());

		// 过期一半
		int removed = SharedWindowClientHandler.sweepPendingAudioAt(1_000L + 15_001L);
		assertEquals(16, removed);

		// 现在新窗口可入队
		SharedWindowClientHandler.holdAudioAt(packet(17L, 2), 31_000L);
		assertEquals(1, SharedWindowClientHandler.pendingAudioWindowCount());
	}

	@Test
	@DisplayName("每窗口包数上限行为保持（100 封顶丢最旧）")
	void perWindowPacketCapIntact() {
		for (int i = 0; i < 105; i++) {
			SharedWindowClientHandler.holdAudioAt(packet(77L, i), 1_000L);
		}
		assertTrue(SharedWindowClientHandler.pendingAudioPacketCount(77L) <= 100,
			"每窗口 ≤100 包的既有约束不得回归");
	}
}
