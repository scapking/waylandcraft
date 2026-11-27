package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * 任务A-3（overload-guard）：全局转发缓冲内存守卫的纯判定函数与来源绑定。
 * 无 MC 依赖；绑定测试后必须还原，避免污染同 JVM 其他用例。
 */
class RelayMemoryGuardTest {

	/** 守卫会从 relay 构造器绑定真实来源；测试统一改为注入值并在结束时还原 */
	private static long testVideoBytes = 0;
	private static long testAudioBytes = 0;

	@BeforeEach
	void bindTestSources() {
		RelayMemoryGuard.bindVideoSource(() -> testVideoBytes);
		RelayMemoryGuard.bindAudioSource(() -> testAudioBytes);
	}

	@AfterEach
	void unbindTestSources() {
		testVideoBytes = 0;
		testAudioBytes = 0;
		RelayMemoryGuard.bindVideoSource(() -> 0L);
		RelayMemoryGuard.bindAudioSource(() -> 0L);
	}

	private static final long LIMIT = RelayMemoryGuard.GLOBAL_BUFFER_BYTES_LIMIT;

	@Test
	@DisplayName("视频准入（替换语义）：现有总缓存 − 旧帧 + 新帧 ≤ 上限")
	void videoAcceptUsesReplaceSemantics() {
		long nearLimit = LIMIT - 2_000_000;
		// 同窗口替换 1.9MB → 1.9MB：总占用不变，放行
		assertTrue(RelayMemoryGuard.videoAcceptAllowed(nearLimit, 1_900_000, 1_900_000));
		// 同窗口替换 1.9MB → 2MB：净增 100KB，距上限不足 100KB → 拒
		assertFalse(RelayMemoryGuard.videoAcceptAllowed(LIMIT - 50_000, 1_900_000, 2_000_000));
		// 同窗口替换 1.9MB → 2MB：余量充足 → 放行
		assertTrue(RelayMemoryGuard.videoAcceptAllowed(LIMIT - 2_000_000, 1_900_000, 2_000_000));
		// 新窗口 0 → 1.9MB：贴着上限放行
		assertTrue(RelayMemoryGuard.videoAcceptAllowed(LIMIT - 1_900_000, 0, 1_900_000));
		// 新窗口越限 1 字节即拒
		assertFalse(RelayMemoryGuard.videoAcceptAllowed(LIMIT - 1_900_000, 0, 1_900_001));
	}

	@Test
	@DisplayName("音频准入：总和贴上限放行、越限拒绝")
	void audioAcceptBoundary() {
		assertTrue(RelayMemoryGuard.audioAcceptAllowed(LIMIT - 1_024, 1_024));
		assertFalse(RelayMemoryGuard.audioAcceptAllowed(LIMIT, 1));
		assertFalse(RelayMemoryGuard.audioAcceptAllowed(LIMIT - 1_024, 1_025));
	}

	@Test
	@DisplayName("isOverLimit(videos, audio)：恰好等于上限不算超限（严格大于）")
	void overLimitIsStrictlyGreater() {
		assertFalse(RelayMemoryGuard.isOverLimit(LIMIT, 0));
		assertFalse(RelayMemoryGuard.isOverLimit(LIMIT - 1, 1));
		assertTrue(RelayMemoryGuard.isOverLimit(LIMIT - 1, 2));
		assertTrue(RelayMemoryGuard.isOverLimit(0, LIMIT + 1));
	}

	@Test
	@DisplayName("运行时求和：绑定来源求和 + 超限判定；来源抛异常按 0 兜底")
	void runtimeSumFromBoundSources() {
		testVideoBytes = 1_000_000;
		testAudioBytes = 2_000_000;
		assertEquals(1_000_000, RelayMemoryGuard.videoBufferedBytes());
		assertEquals(2_000_000, RelayMemoryGuard.audioBufferedBytes());
		assertEquals(3_000_000, RelayMemoryGuard.totalBufferedBytes());
		assertFalse(RelayMemoryGuard.isOverLimit());

		testAudioBytes = LIMIT;
		assertTrue(RelayMemoryGuard.isOverLimit(), "视频 + 音频越过全局上限");
	}
}
