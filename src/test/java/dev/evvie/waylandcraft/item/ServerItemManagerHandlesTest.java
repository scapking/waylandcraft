package dev.evvie.waylandcraft.item;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P0-2 修复证明：give_items 句柄表去重/限长的纯函数（原实现
 * List.contains 是 O(n²)；无限长 payload 可放大 give 循环）。
 */
class ServerItemManagerHandlesTest {

	@Test
	@DisplayName("sanitizeHandles: 去重且保持首次出现顺序")
	void dedupKeepsFirstOccurrenceOrder() {
		List<Long> out = ServerItemManager.sanitizeHandles(new long[]{5L, 3L, 5L, 1L, 3L}, 1024);
		assertEquals(List.of(5L, 3L, 1L), out);
	}

	@Test
	@DisplayName("sanitizeHandles: 超过上限整包拒绝（返回 null）")
	void overCapRejected() {
		long[] handles = new long[ServerItemManager.MAX_HANDLES_PER_PAYLOAD + 1];
		for (int i = 0; i < handles.length; i++) handles[i] = i;
		assertNull(ServerItemManager.sanitizeHandles(handles, ServerItemManager.MAX_HANDLES_PER_PAYLOAD));
	}

	@Test
	@DisplayName("sanitizeHandles: 恰好等于上限合法；大量重复去重后仍为小集合")
	void boundaryAndHeavyDuplicates() {
		long[] atCap = new long[ServerItemManager.MAX_HANDLES_PER_PAYLOAD];
		for (int i = 0; i < atCap.length; i++) atCap[i] = i;
		List<Long> out = ServerItemManager.sanitizeHandles(atCap, ServerItemManager.MAX_HANDLES_PER_PAYLOAD);
		assertEquals(ServerItemManager.MAX_HANDLES_PER_PAYLOAD, out.size());

		long[] dup = new long[900];
		java.util.Arrays.fill(dup, 7L);
		List<Long> dedup = ServerItemManager.sanitizeHandles(dup, 1024);
		assertEquals(List.of(7L), dedup);
	}

	@Test
	@DisplayName("sanitizeHandles: null/空数组 → 空列表（不是 null，避免上层再判）")
	void nullAndEmptySafe() {
		assertTrue(ServerItemManager.sanitizeHandles(null, 1024).isEmpty());
		assertTrue(ServerItemManager.sanitizeHandles(new long[0], 1024).isEmpty());
	}
}
