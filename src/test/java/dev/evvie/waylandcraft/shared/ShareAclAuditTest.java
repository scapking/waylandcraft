package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import java.util.UUID;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * ACL-5 审计与可见性测试 + ACL-1 过期扫描并发安全冒烟。
 */
class ShareAclAuditTest {

	private static final UUID OWNER = UUID.fromString("dddddddd-0000-0000-0000-000000000001");

	@Test
	@DisplayName("计数与 clear：会话停止归零，报告块含全部键")
	void countersAndClear() {
		ShareAclAudit.clear();
		long before = ShareAclAudit.grantsManual.get();
		ShareAclAudit.countGrant(WindowGrant.GrantSource.MANUAL);
		assertEquals(before + 1, ShareAclAudit.grantsManual.get());

		ShareAclAudit.log("GRANT", 0x1L, OWNER, OWNER, WindowPermission.VIEW, WindowGrant.GrantSource.MANUAL, 123L);
		ShareAclAudit.logDeny("interact", 0x1L, OWNER); // 不应抛

		String block = ShareAclAudit.reportBlock();
		for (String key : List.of("grants:", "lifecycle:", "invites:", "denies:", "reregister:",
			"manual=", "expired=", "revoked=", "sent=", "not_owner=", "cleared=")) {
			assertTrue(block.contains(key), "报告缺键: " + key);
		}

		ShareAclAudit.clear();
		assertEquals(0, ShareAclAudit.grantsManual.get());
		assertEquals(0, ShareAclAudit.grantsInvite.get());
		assertEquals(0, ShareAclAudit.expired.get());
	}

	@Test
	@DisplayName("expireGrants 与并发 grant/revoke 竞争：无异常、无半更新可见")
	void sweepRacesWithWriters() throws Exception {
		SharedWindowManager m = new SharedWindowManager(new PermissionManager());
		long h = 0x88L;
		m.registerWindow(h, OWNER, "race", null);
		SharedWindowEntry e = m.getWindow(h);

		int threads = 3;
		int ops = 5_000;
		ExecutorService pool = Executors.newFixedThreadPool(threads + 1);
		CountDownLatch start = new CountDownLatch(1);
		AtomicInteger errors = new AtomicInteger();

		for (int w = 0; w < threads; w++) {
			final int wid = w;
			pool.execute(() -> {
				try {
					start.await();
					for (int i = 0; i < ops; i++) {
						UUID p = new UUID(wid, i);
						if ((i & 1) == 0) {
							e.grant(p, WindowPermission.VIEW, OWNER, System.currentTimeMillis() + 10,
								System.currentTimeMillis(), WindowGrant.GrantSource.MANUAL);
						} else {
							e.revoke(p, OWNER, System.currentTimeMillis());
						}
					}
				} catch (Throwable t) {
					errors.incrementAndGet();
				}
			});
		}
		// 扫描线程模拟 tick（生产在 server thread；并发语义一致——只依赖 CHM 原子操作）
		pool.execute(() -> {
			try {
				start.await();
				for (int i = 0; i < ops / 10; i++) {
					List<SharedWindowManager.GrantExpired> evicted = m.expireGrants(System.currentTimeMillis());
					// 回收后玩家权限只能是 NONE（无记录）或后续被写线程重新授予——不得抛
					for (var x : evicted) {
						m.effectivePermission(h, x.playerUUID(), System.currentTimeMillis());
					}
				}
			} catch (Throwable t) {
				errors.incrementAndGet();
			}
		});

		start.countDown();
		pool.shutdown();
		assertTrue(pool.awaitTermination(30, TimeUnit.SECONDS), "并发冒烟必须限时完成");
		assertEquals(0, errors.get());
		// owner 记录在风暴中不丢
		assertEquals(WindowPermission.CONTROL, m.effectivePermission(h, OWNER, System.currentTimeMillis()));
		assertDoesNotThrow(() -> m.getAllWindows().size());
	}
}
