package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.UUID;

import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * 任务A-1（overload-guard）：每观众同时观看窗口数上限。
 * 纯截断函数 + 登记表行为（fail-open / retainOnly / 暂缓计数），无 MC 依赖。
 */
class ViewerSubscriptionCapTest {

	private final UUID viewer = UUID.randomUUID();

	@BeforeEach
	void resetState() {
		ViewerSubscriptions.clear();
	}

	@Test
	@DisplayName("capDelivered：候选 20 个、上限 16 → 保留句柄最小的 16 个（确定性截断）")
	void capsToLowestHandles() {
		List<Long> candidates = new ArrayList<>();
		for (long h = 100; h < 120; h++) {
			candidates.add(h);
		}
		Set<Long> delivered = ViewerSubscriptions.capDelivered(candidates, ViewerSubscriptions.MAX_WINDOWS_PER_VIEWER);
		assertEquals(ViewerSubscriptions.MAX_WINDOWS_PER_VIEWER, delivered.size());
		for (long h = 100; h < 116; h++) {
			assertTrue(delivered.contains(h), "句柄 " + h + " 应在交付集内");
		}
		for (long h = 116; h < 120; h++) {
			assertFalse(delivered.contains(h), "句柄 " + h + " 超限应被暂缓");
		}
	}

	@Test
	@DisplayName("capDelivered：乱序输入结果一致（升序截断与输入顺序无关）")
	void deterministicRegardlessOfInputOrder() {
		List<Long> shuffled = new ArrayList<>(List.of(103L, 101L, 102L, 100L));
		Set<Long> delivered = ViewerSubscriptions.capDelivered(shuffled, 2);
		assertEquals(Set.of(100L, 101L), delivered, "应保留最小的两个句柄");
	}

	@Test
	@DisplayName("capDelivered：未超限全保留；上限 ≤0 → 空集；空候选 → 空集")
	void edgeCases() {
		assertEquals(Set.of(7L, 3L), ViewerSubscriptions.capDelivered(List.of(7L, 3L), 16));
		assertTrue(ViewerSubscriptions.capDelivered(List.of(1L, 2L), 0).isEmpty());
		assertTrue(ViewerSubscriptions.capDelivered(List.of(), 16).isEmpty());
	}

	@Test
	@DisplayName("isDelivered：无登记观众 fail-open 放行；登记后按交付集判定")
	void failOpenThenExactMatch() {
		UUID stranger = UUID.randomUUID();
		assertTrue(ViewerSubscriptions.isDelivered(stranger, 999L), "无登记 = 放行（广播未跑过不黑屏）");

		ViewerSubscriptions.refresh(viewer, Set.of(1L, 2L), 3);
		assertTrue(ViewerSubscriptions.isDelivered(viewer, 1L));
		assertTrue(ViewerSubscriptions.isDelivered(viewer, 2L));
		assertFalse(ViewerSubscriptions.isDelivered(viewer, 3L), "超限窗口不转发");
	}

	@Test
	@DisplayName("refresh 整体替换交付集；暂缓计数可查")
	void refreshReplacesAndCounts() {
		ViewerSubscriptions.refresh(viewer, Set.of(1L), 0);
		assertEquals(0, ViewerSubscriptions.lastDeniedCount(viewer));
		assertTrue(ViewerSubscriptions.isDelivered(viewer, 1L));

		ViewerSubscriptions.refresh(viewer, Set.of(2L), 5);
		assertEquals(5, ViewerSubscriptions.lastDeniedCount(viewer));
		assertFalse(ViewerSubscriptions.isDelivered(viewer, 1L), "旧交付集被替换");
		assertTrue(ViewerSubscriptions.isDelivered(viewer, 2L));
		assertTrue(ViewerSubscriptions.totalDenied() >= 5);
		assertEquals(1, ViewerSubscriptions.trackedViewers());
	}

	@Test
	@DisplayName("retainOnly 清理下线观众登记，防 map 随玩家进出泄漏")
	void retainOnlyPrunesOffline() {
		UUID other = UUID.randomUUID();
		ViewerSubscriptions.refresh(viewer, Set.of(1L), 0);
		ViewerSubscriptions.refresh(other, Set.of(2L), 0);

		ViewerSubscriptions.retainOnly(Set.of(viewer));
		assertEquals(1, ViewerSubscriptions.trackedViewers());
		assertTrue(ViewerSubscriptions.isDelivered(viewer, 1L));
		assertTrue(ViewerSubscriptions.isDelivered(other, 2L), "other 无登记 → fail-open 放行");
	}
}
