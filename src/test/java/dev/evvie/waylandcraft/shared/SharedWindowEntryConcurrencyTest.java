package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.Map;
import java.util.UUID;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P1-4 修复证明：SharedWindowEntry.permissions 从 HashMap 改为 ConcurrentHashMap。
 * 转发线程读（getPermission/hasPermission）、服务端线程写（setPermission），
 * HashMap 在该模式下可能丢条目/死循环；并发冒烟 + 快照语义必须无异常且自洽。
 *
 * 用定向共享（targetPlayerUUID != null）构造 entry：该路径不触碰
 * WaylandCraftCommon.instance（单测里为 null）。
 */
class SharedWindowEntryConcurrencyTest {

	@Test
	@DisplayName("并发读写权限表不抛异常，owner/已知条目不丢")
	void permissionsConcurrentReadWriteSmoke() throws Exception {
		UUID owner = UUID.randomUUID();
		UUID target = UUID.randomUUID();
		SharedWindowEntry entry = new SharedWindowEntry(0x1234L, owner, "title", target);

		int readers = 3;
		int writers = 2;
		int opsPerThread = 20_000;
		ExecutorService pool = Executors.newFixedThreadPool(readers + writers);
		CountDownLatch start = new CountDownLatch(1);
		AtomicInteger errors = new AtomicInteger();

		for (int w = 0; w < writers; w++) {
			final int wid = w;
			pool.execute(() -> {
				try {
					start.await();
					for (int i = 0; i < opsPerThread; i++) {
						UUID p = new UUID(wid, i % 997); // 复用有限键空间，保证可检查
						entry.setPermission(p, (i & 1) == 0 ? WindowPermission.VIEW : WindowPermission.NONE);
					}
				} catch (Throwable t) {
					errors.incrementAndGet();
				}
			});
		}
		for (int r = 0; r < readers; r++) {
			pool.execute(() -> {
				try {
					start.await();
					for (int i = 0; i < opsPerThread; i++) {
						UUID p = new UUID(i % 2, i % 997);
						entry.getPermission(p);
						entry.hasPermission(p, WindowPermission.VIEW);
						Map<UUID, WindowPermission> snap = entry.getAllPermissions();
						// 快照必须始终包含 owner（COW/并发读都不应看见半更新状态）
						if (!WindowPermission.CONTROL.equals(snap.get(owner))) {
							errors.incrementAndGet();
						}
					}
				} catch (Throwable t) {
					errors.incrementAndGet();
				}
			});
		}

		start.countDown();
		pool.shutdown();
		assertTrue(pool.awaitTermination(30, TimeUnit.SECONDS), "并发冒烟必须有限时完成（HashMap 死循环场景会超时）");
		assertEquals(0, errors.get(), "并发读写期间不允许出现异常/丢失 owner 条目");

		// 写线程结束后不变量：entry 自带的 owner/target 从未被写线程触碰（键空间不相交），
		// 在并发风暴中不允许丢失
		assertEquals(WindowPermission.CONTROL, entry.getPermission(owner));
		assertEquals(WindowPermission.VIEW, entry.getPermission(target));
	}

	@Test
	@DisplayName("getAllPermissions 返回快照：后续写入不影响已取副本")
	void getAllPermissionsIsDetachedSnapshot() {
		UUID owner = UUID.randomUUID();
		UUID extra = UUID.randomUUID();
		SharedWindowEntry entry = new SharedWindowEntry(1L, owner, "t", owner); // target=owner → 不新增

		Map<UUID, WindowPermission> snap = entry.getAllPermissions();
		assertDoesNotThrow(() -> entry.setPermission(extra, WindowPermission.INTERACT));
		assertEquals(1, snap.size(), "快照不反映后续写入");
		assertNull(snap.get(extra));
		assertEquals(WindowPermission.INTERACT, entry.getPermission(extra));
	}
}
