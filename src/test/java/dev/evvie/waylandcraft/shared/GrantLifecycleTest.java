package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import java.util.UUID;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * ACL-1/2/6 授权生命周期测试（entry/manager 层，无 MC 运行时）：
 * 记录写入/墓碑/惰性过期/到期扫描/handle 重注册清权/join 策略物化。
 */
class GrantLifecycleTest {

	private static final UUID OWNER = UUID.fromString("aaaaaaaa-0000-0000-0000-000000000001");
	private static final UUID VIEWER = UUID.fromString("bbbbbbbb-0000-0000-0000-000000000002");
	private static final UUID STRANGER = UUID.fromString("cccccccc-0000-0000-0000-000000000003");

	// ===== entry 层 =====

	@Test
	@DisplayName("定向共享构造：owner CONTROL + 目标 VIEW，陌生人无记录")
	void targetedConstructor() {
		SharedWindowEntry e = new SharedWindowEntry(0x10L, OWNER, "t", VIEWER);
		assertEquals(WindowPermission.CONTROL, e.getPermission(OWNER));
		assertEquals(WindowPermission.VIEW, e.getPermission(VIEWER));
		assertEquals(WindowPermission.NONE, e.getPermission(STRANGER));
		assertNotNull(e.getGrant(VIEWER));
		assertEquals(WindowGrant.GrantSource.MANUAL, e.getGrant(VIEWER).source());
	}

	@Test
	@DisplayName("公开共享构造不再自动放行任何观众（策略由 manager 物化）")
	void publicConstructorGrantsNoOne() {
		SharedWindowEntry e = new SharedWindowEntry(0x11L, OWNER, "t", null);
		assertEquals(WindowPermission.CONTROL, e.getPermission(OWNER));
		assertEquals(WindowPermission.NONE, e.getPermission(VIEWER));
		assertEquals(1, e.grantsSnapshot().size(), "只有 owner 一条记录");
	}

	@Test
	@DisplayName("owner 不可被自我改写：setPermission/grant/revoke 对 owner 一律 false 且记录不动")
	void ownerRecordsImmutable() {
		SharedWindowEntry e = new SharedWindowEntry(0x12L, OWNER, "t", null);
		assertFalse(e.setPermission(OWNER, WindowPermission.NONE));
		assertFalse(e.grant(OWNER, WindowPermission.VIEW, OWNER, WindowGrant.NEVER_EXPIRES,
			System.currentTimeMillis(), WindowGrant.GrantSource.MANUAL));
		assertFalse(e.revoke(OWNER, OWNER, System.currentTimeMillis()));
		assertEquals(WindowPermission.CONTROL, e.getPermission(OWNER));
	}

	@Test
	@DisplayName("revoke 写墓碑：getPermission=NONE 且记录仍在（区别于从未授权）")
	void revokeWritesTombstone() {
		SharedWindowEntry e = new SharedWindowEntry(0x13L, OWNER, "t", VIEWER);
		assertTrue(e.revoke(VIEWER, OWNER, System.currentTimeMillis()));
		assertEquals(WindowPermission.NONE, e.getPermission(VIEWER));
		WindowGrant tomb = e.getGrant(VIEWER);
		assertNotNull(tomb, "墓碑必须留档（审计 + 压过默认策略回放）");
		assertEquals(WindowPermission.NONE, tomb.level());
	}

	@Test
	@DisplayName("到期记录读取即失效并惰性摘除（sweep 未跑前的竞态防线）")
	void expiredRecordLazilyPurged() {
		SharedWindowEntry e = new SharedWindowEntry(0x14L, OWNER, "t", VIEWER);
		long past = System.currentTimeMillis() - 10_000;
		e.grant(VIEWER, WindowPermission.VIEW, OWNER, past, past, WindowGrant.GrantSource.MANUAL);
		assertEquals(WindowPermission.NONE, e.getPermission(VIEWER));
		assertNull(e.getGrant(VIEWER), "过期记录读取后应被惰性摘除");
	}

	// ===== manager 层 =====

	@Test
	@DisplayName("grantTimed：owner 校验——伪造（非 owner）请求拒绝且计数")
	void grantTimedRejectsNonOwner() {
		SharedWindowManager m = new SharedWindowManager(new PermissionManager());
		long h = 0x20L;
		m.registerWindow(h, OWNER, "w", null);
		long before = ShareAclAudit.deniedNotOwner.get();
		assertNull(m.grantTimed(h, VIEWER, WindowPermission.VIEW, STRANGER,
			WindowGrant.NEVER_EXPIRES, WindowGrant.GrantSource.MANUAL, System.currentTimeMillis()));
		assertEquals(before + 1, ShareAclAudit.deniedNotOwner.get());
		// owner 自己授 → 成功
		WindowGrant g = m.grantTimed(h, VIEWER, WindowPermission.INTERACT, OWNER,
			System.currentTimeMillis() + 60_000, WindowGrant.GrantSource.MANUAL, System.currentTimeMillis());
		assertNotNull(g);
		assertEquals(WindowPermission.INTERACT, g.level());
		assertFalse(g.isPermanent());
	}

	@Test
	@DisplayName("updatePermission：owner 权威 + NONE 走墓碑（压过后续策略回放）")
	void updatePermissionOwnerAuthority() {
		SharedWindowManager m = new SharedWindowManager(new PermissionManager());
		long h = 0x21L;
		m.registerWindow(h, OWNER, "w", null);
		assertFalse(m.updatePermission(h, VIEWER, WindowPermission.VIEW, STRANGER), "非 owner 一律拒");
		assertNotNull(m.updatePermissionAndGetEntry(h, VIEWER, WindowPermission.VIEW, OWNER));
		assertTrue(m.effectivePermission(h, VIEWER, System.currentTimeMillis()).hasPermission(WindowPermission.VIEW));
		assertNotNull(m.updatePermissionAndGetEntry(h, VIEWER, WindowPermission.NONE, OWNER));
		assertEquals(WindowPermission.NONE, m.effectivePermission(h, VIEWER, System.currentTimeMillis()));
		assertNotNull(m.getWindow(h).getGrant(VIEWER), "revoke 后留墓碑记录");
		// 墓碑不被 ALLOW 策略回放覆盖（applyDefaultPolicy 不触碰未过期显式记录）
		m.reapplyAllowPolicyAll();
		assertEquals(WindowPermission.NONE, m.effectivePermission(h, VIEWER, System.currentTimeMillis()));
	}

	@Test
	@DisplayName("黑白名单接入判定（审计 P2-6）：黑名单压过记录，白名单压过记录")
	void tablesConsumedByJudgmentChain() {
		PermissionManager pm = new PermissionManager();
		SharedWindowManager m = new SharedWindowManager(pm);
		long h = 0x22L;
		m.registerWindow(h, OWNER, "w", null);

		m.grantTimed(h, VIEWER, WindowPermission.CONTROL, OWNER, WindowGrant.NEVER_EXPIRES,
			WindowGrant.GrantSource.MANUAL, System.currentTimeMillis());
		assertEquals(WindowPermission.CONTROL, m.effectivePermission(h, VIEWER, System.currentTimeMillis()));

		pm.addToBlacklist(VIEWER);
		assertEquals(WindowPermission.NONE, m.effectivePermission(h, VIEWER, System.currentTimeMillis()),
			"黑名单必须一票否决 per-window CONTROL");

		pm.addToWhitelist(STRANGER, WindowPermission.INTERACT);
		assertEquals(WindowPermission.INTERACT, m.effectivePermission(h, STRANGER, System.currentTimeMillis()),
			"白名单对无记录玩家生效（表被真正消费）");
	}

	@Test
	@DisplayName("owner 恒全权：拉黑 owner 自己也不能剥夺其 CONTROL")
	void ownerImmuneToBlacklist() {
		PermissionManager pm = new PermissionManager();
		SharedWindowManager m = new SharedWindowManager(pm);
		m.registerWindow(0x23L, OWNER, "w", null);
		pm.addToBlacklist(OWNER);
		assertEquals(WindowPermission.CONTROL, m.effectivePermission(0x23L, OWNER, System.currentTimeMillis()));
	}

	@Test
	@DisplayName("expireGrants：到期摘除 + 公开窗口按 ALLOW 策略回落 DEFAULT；定向窗口不回落")
	void expireSweepAndDefaultRefall() {
		PermissionManager pm = new PermissionManager();
		SharedWindowManager m = new SharedWindowManager(pm);
		long now = System.currentTimeMillis();

		long hPublic = 0x30L;
		long hTargeted = 0x31L;
		SharedWindowEntry pe = m.registerWindow(hPublic, OWNER, "p", null);
		SharedWindowEntry te = m.registerWindow(hTargeted, OWNER, "t", VIEWER);
		// 限时授权覆盖原记录（模拟 grant 10m）
		pe.grant(VIEWER, WindowPermission.CONTROL, OWNER, now + 10, now, WindowGrant.GrantSource.MANUAL);
		te.grant(STRANGER, WindowPermission.VIEW, OWNER, now + 10, now, WindowGrant.GrantSource.INVITE);

		long expiredBefore = ShareAclAudit.expired.get();
		List<SharedWindowManager.GrantExpired> evicted = m.expireGrants(now + 100);
		assertEquals(2, evicted.size());
		assertEquals(2, ShareAclAudit.expired.get() - expiredBefore, "expired 计数增量必须等于回收数");

		// 公开窗口：VIEWER 回到 DEFAULT 派生（policy=ALLOW 默认值）
		WindowGrant refall = m.getWindow(hPublic).getGrant(VIEWER);
		assertNotNull(refall, "公开+ALLOW：到期后回落 DEFAULT 记录");
		assertEquals(WindowGrant.GrantSource.DEFAULT, refall.source());
		assertEquals(WindowPermission.VIEW, refall.level(), "默认级别 = defaultPermission=VIEW");

		// 定向窗口：STRANGER 到期后保持无记录
		assertNull(m.getWindow(hTargeted).getGrant(STRANGER), "定向窗口到期不回落");
		assertEquals(WindowPermission.NONE, m.effectivePermission(hTargeted, STRANGER, now + 200));
	}

	@Test
	@DisplayName("expireGrants：黑名单玩家到期后不回落 DEFAULT")
	void expiredBlacklistedStaysNone() {
		PermissionManager pm = new PermissionManager();
		pm.addToBlacklist(VIEWER);
		SharedWindowManager m = new SharedWindowManager(pm);
		SharedWindowEntry e = m.registerWindow(0x32L, OWNER, "w", null);
		e.grant(VIEWER, WindowPermission.VIEW, OWNER, 5_000, 0, WindowGrant.GrantSource.MANUAL);
		var evicted = m.expireGrants(6_000);
		assertEquals(1, evicted.size());
		assertNull(e.getGrant(VIEWER), "黑名单：不回落 DEFAULT 记录");
		assertEquals(WindowPermission.NONE, m.effectivePermission(0x32L, VIEWER, 6_000));
	}

	@Test
	@DisplayName("onPlayerJoin：ALLOW 物化 DEFAULT；ASK 返回邀请；DENY 无动作；定向窗口不参与")
	void joinPolicyBranches() {
		PermissionManager pm = new PermissionManager();
		SharedWindowManager m = new SharedWindowManager(pm);
		long hPublic = 0x40L;
		long hTargeted = 0x41L;
		// 注册时在线列表为空（测试环境 server=null），不影响 join 路径
		m.registerWindow(hPublic, OWNER, "p", null);
		m.registerWindow(hTargeted, OWNER, "t", STRANGER);

		pm.setNewViewerPolicy(ShareAcl.NewViewerPolicy.ALLOW);
		assertTrue(m.onPlayerJoin(VIEWER).isEmpty());
		assertEquals(WindowPermission.VIEW, m.effectivePermission(hPublic, VIEWER, System.currentTimeMillis()),
			"ALLOW：join 物化 DEFAULT 授权（等价历史 grantViewToNewPlayer）");
		assertEquals(WindowPermission.NONE, m.effectivePermission(hTargeted, VIEWER, System.currentTimeMillis()),
			"定向窗口任何策略都不自动授权");

		// ASK：新玩家不物化，返回邀请列表
		pm.setNewViewerPolicy(ShareAcl.NewViewerPolicy.ASK);
		var invites = m.onPlayerJoin(UUID.randomUUID());
		assertEquals(1, invites.size(), "ASK：每个无记录的公开窗口一条邀请");
		assertEquals(hPublic, invites.get(0).windowHandle());

		// DENY：无记录无邀请
		pm.setNewViewerPolicy(ShareAcl.NewViewerPolicy.DENY);
		assertTrue(m.onPlayerJoin(UUID.randomUUID()).isEmpty());
	}

	@Test
	@DisplayName("default=NONE 表值：ALLOW 策略不物化任何记录（表被消费）")
	void defaultLevelNoneNoMaterialization() {
		PermissionManager pm = new PermissionManager();
		pm.setDefaultPermission(WindowPermission.NONE);
		SharedWindowManager m = new SharedWindowManager(pm);
		m.registerWindow(0x42L, OWNER, "p", null);
		assertTrue(m.onPlayerJoin(VIEWER).isEmpty());
		assertEquals(WindowPermission.NONE, m.effectivePermission(0x42L, VIEWER, System.currentTimeMillis()));
	}

	// ===== ACL-6：handle 重注册 =====

	@Test
	@DisplayName("同 owner 重注册：旧授权/墓碑全部清空，陌生人旧权限不残留")
	void reRegisterClearsAcl() {
		PermissionManager pm = new PermissionManager();
		pm.setNewViewerPolicy(ShareAcl.NewViewerPolicy.DENY); // 排除策略物化干扰
		SharedWindowManager m = new SharedWindowManager(pm);
		long h = 0x50L;
		SharedWindowEntry e1 = m.registerWindow(h, OWNER, "first", null);
		e1.grant(VIEWER, WindowPermission.CONTROL, OWNER, WindowGrant.NEVER_EXPIRES,
			System.currentTimeMillis(), WindowGrant.GrantSource.MANUAL);
		e1.revoke(STRANGER, OWNER, System.currentTimeMillis());

		SharedWindowEntry old = m.replaceWindowForSameOwner(h, OWNER, "second", null);
		assertNotNull(old);
		org.junit.jupiter.api.Assertions.assertNotSame(e1, m.getWindow(h), "必须是全新 entry 对象（记录表重建）");
		assertEquals(WindowPermission.NONE, m.effectivePermission(h, VIEWER, System.currentTimeMillis()),
			"重建窗口：旧观众 CONTROL 不得残留（典型逻辑洞）");
		assertEquals(WindowPermission.NONE, m.effectivePermission(h, STRANGER, System.currentTimeMillis()));
		assertEquals(WindowPermission.CONTROL, m.effectivePermission(h, OWNER, System.currentTimeMillis()));
	}

	@Test
	@DisplayName("handlePlayerDisconnect：返回名下句柄供网络层清缓存；他人窗口不受影响")
	void disconnectReturnsHandles() {
		SharedWindowManager m = new SharedWindowManager(new PermissionManager());
		m.registerWindow(0x60L, OWNER, "a", null);
		m.registerWindow(0x61L, OWNER, "b", null);
		m.registerWindow(0x62L, VIEWER, "c", null);
		List<Long> removed = m.handlePlayerDisconnect(OWNER);
		assertEquals(2, removed.size());
		assertNull(m.getWindow(0x60L));
		assertNotNull(m.getWindow(0x62L), "他人窗口不受影响");
		assertTrue(m.getPlayerSubscriptions(OWNER).isEmpty());
	}

	@Test
	@DisplayName("canInteract：INTERACT 权限但窗口已注销 → false（竞态防线）")
	void interactAfterUnregister() {
		SharedWindowManager m = new SharedWindowManager(new PermissionManager());
		long h = 0x70L;
		m.registerWindow(h, OWNER, "w", VIEWER);
		assertFalse(m.canInteract(VIEWER, h), "定向 VIEW 不足以交互（VIEW<INTERACT）");
		assertNotNull(m.updatePermissionAndGetEntry(h, VIEWER, WindowPermission.INTERACT, OWNER));
		assertTrue(m.canInteract(VIEWER, h));
		m.unregisterWindow(h);
		assertFalse(m.canInteract(VIEWER, h), "窗口注销后交互必须被拒");
	}
}
