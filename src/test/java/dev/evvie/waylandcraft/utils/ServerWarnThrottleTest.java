package dev.evvie.waylandcraft.utils;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P1-6/P1-7 修复证明：告警限频器（注入时钟）。被拒 warn 每玩家 5s 一条，
 * 恶意刷包不再造成日志洪泛。
 */
class ServerWarnThrottleTest {

	@Test
	@DisplayName("同一 key 在间隔内只放行第一条")
	void firstPassThenSuppressedWithinInterval() {
		String key = "test:player-A-" + System.nanoTime();
		assertTrue(ServerWarnThrottle.allowAt(key, 1000L, 5000));
		assertFalse(ServerWarnThrottle.allowAt(key, 1001L, 5000));
		assertFalse(ServerWarnThrottle.allowAt(key, 5999L, 5000));
		assertTrue(ServerWarnThrottle.allowAt(key, 6000L, 5000), "满 5s 后重新放行");
	}

	@Test
	@DisplayName("不同 key 互不影响")
	void differentKeysIndependent() {
		String k1 = "test:player-B-" + System.nanoTime();
		String k2 = "test:player-C-" + System.nanoTime();
		assertTrue(ServerWarnThrottle.allowAt(k1, 100L, 5000));
		assertTrue(ServerWarnThrottle.allowAt(k2, 101L, 5000), "k1 的窗口不应压制 k2");
		assertFalse(ServerWarnThrottle.allowAt(k1, 102L, 5000));
	}

	@Test
	@DisplayName("removePrefix 只清自己的命名空间（P2-5 分模块 reset 用）")
	void removePrefixScoped() {
		String mine = "interact-denied:" + System.nanoTime();
		String theirs = "frame-reject:" + System.nanoTime();
		ServerWarnThrottle.allowAt(mine, 1L, 5000);
		ServerWarnThrottle.allowAt(theirs, 1L, 5000);

		ServerWarnThrottle.removePrefix("interact-denied:");

		assertTrue(ServerWarnThrottle.allowAt(mine, 2L, 5000), "前缀内的 key 已清除，重新放行");
		assertFalse(ServerWarnThrottle.allowAt(theirs, 2L, 5000), "前缀外的 key 不受影响");
		ServerWarnThrottle.removePrefix("frame-reject:");
	}
}
