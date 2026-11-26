package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.UUID;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * ACL-3 合成判定链纯函数测试：优先级 owner 恒全权 > 黑名单 > 白名单 > per-window
 * 记录（含过期/墓碑）> 无记录。发现点对应审计 P2-6（"无消费者权威表"）与
 * ACL 加固各项威胁。
 */
class ShareAclComposeTest {

	private static final UUID PLAYER = UUID.fromString("11111111-1111-1111-1111-111111111111");
	private static final UUID OWNER = UUID.fromString("22222222-2222-2222-2222-222222222222");

	private static WindowGrant grant(WindowPermission level, long expiresAt) {
		return new WindowGrant(PLAYER, level, OWNER, 1_000L, expiresAt, WindowGrant.GrantSource.MANUAL);
	}

	@Test
	@DisplayName("owner 恒全权：即使被拉黑/被墓碑/无记录也必须 CONTROL")
	void ownerAlwaysFullControl() {
		// 显式测试锁定（交付要求）：owner 身份压过一切
		assertEquals(WindowPermission.CONTROL, ShareAcl.compose(true, true, null,
			WindowGrant.tombstone(OWNER, OWNER, 1000, WindowGrant.GrantSource.MANUAL), false, false, 2000));
		assertEquals(WindowPermission.CONTROL, ShareAcl.compose(true, false, null, null, false, true, 2000));
		assertEquals(WindowPermission.CONTROL, ShareAcl.compose(true, true, WindowPermission.VIEW,
			grant(WindowPermission.NONE, 0), false, false, 2000));
	}

	@Test
	@DisplayName("黑名单压过白名单与 per-window 记录")
	void blacklistWinsEverythingExceptOwner() {
		WindowGrant record = grant(WindowPermission.CONTROL, 0);
		assertEquals(WindowPermission.NONE, ShareAcl.compose(false, true, WindowPermission.INTERACT,
			record, false, false, 2000));
	}

	@Test
	@DisplayName("白名单压过 per-window 记录（审计 P2-6：表必须被消费）")
	void whitelistBeatsPerWindow() {
		// 白名单 INTERACT、记录 NONE 墓碑 → INTERACT
		assertEquals(WindowPermission.INTERACT, ShareAcl.compose(false, false, WindowPermission.INTERACT,
			WindowGrant.tombstone(PLAYER, OWNER, 1000, WindowGrant.GrantSource.MANUAL), false, false, 2000));
		// 白名单命中优先于记录 VIEW
		assertEquals(WindowPermission.CONTROL, ShareAcl.compose(false, false, WindowPermission.CONTROL,
			grant(WindowPermission.VIEW, 0), false, false, 2000));
	}

	@Test
	@DisplayName("无白名单时 per-window 记录生效；无记录回退 NONE（不得有环境默认泄漏）")
	void perWindowThenNone() {
		assertEquals(WindowPermission.VIEW, ShareAcl.compose(false, false, null,
			grant(WindowPermission.VIEW, 0), false, false, 2000));
		assertEquals(WindowPermission.NONE, ShareAcl.compose(false, false, null, null, false, false, 2000));
		assertEquals(WindowPermission.NONE, ShareAcl.compose(false, false, null, null, false, true, 2000));
	}

	@Test
	@DisplayName("过期记录 = 无权限（墓碑 NONE 不算过期）")
	void expiredRecordIsNone() {
		WindowGrant timed = grant(WindowPermission.VIEW, 5_000L);
		assertTrue(timed.isExpired(5_000L));
		assertFalse(timed.isExpired(4_999L));
		assertEquals(WindowPermission.NONE, ShareAcl.compose(false, false, null, timed,
			timed.isExpired(6_000L), false, 6_000L));
		WindowGrant tomb = WindowGrant.tombstone(PLAYER, OWNER, 1000, WindowGrant.GrantSource.MANUAL);
		assertFalse(tomb.isExpired(1_000_000L), "NONE 墓碑永不算过期（防默认策略复活权限）");
	}

	@Test
	@DisplayName("NONE→VIEW→NONE→VIEW 时序：每次转换都由记录决定，无粘滞")
	void permissionFlipSequence() {
		long now = 10_000L;
		// NONE（无记录）
		assertEquals(WindowPermission.NONE, ShareAcl.compose(false, false, null, null, false, false, now));
		// VIEW（授权记录）
		WindowGrant view = grant(WindowPermission.VIEW, 0);
		assertEquals(WindowPermission.VIEW, ShareAcl.compose(false, false, null, view, false, false, now));
		// NONE（吊销墓碑）
		WindowGrant tomb = WindowGrant.tombstone(PLAYER, OWNER, now, WindowGrant.GrantSource.MANUAL);
		assertEquals(WindowPermission.NONE, ShareAcl.compose(false, false, null, tomb, false, false, now));
		// VIEW（重新授权覆盖墓碑）
		WindowGrant view2 = grant(WindowPermission.VIEW, 0);
		assertEquals(WindowPermission.VIEW, ShareAcl.compose(false, false, null, view2, false, false, now));
	}

	@Test
	@DisplayName("过期回落默认策略判定：仅公开+ALLOW+非 owner+非黑名单")
	void reapplyDefaultConditions() {
		assertFalse(ShareAcl.shouldReapplyDefault(true, ShareAcl.NewViewerPolicy.ALLOW, false, false), "定向窗口不回落");
		assertFalse(ShareAcl.shouldReapplyDefault(false, ShareAcl.NewViewerPolicy.ASK, false, false));
		assertFalse(ShareAcl.shouldReapplyDefault(false, ShareAcl.NewViewerPolicy.DENY, false, false));
		assertFalse(ShareAcl.shouldReapplyDefault(false, ShareAcl.NewViewerPolicy.ALLOW, true, false), "黑名单不回落");
		assertFalse(ShareAcl.shouldReapplyDefault(false, ShareAcl.NewViewerPolicy.ALLOW, false, true), "owner 不需要");
		assertTrue(ShareAcl.shouldReapplyDefault(false, ShareAcl.NewViewerPolicy.ALLOW, false, false));
	}

	@Test
	@DisplayName("grant 等级钳位：入站非法字节（fromLevel→NONE）落 VIEW，绝不钳位成 CONTROL")
	void grantedLevelClampIsConservative() {
		// 恶意/损坏 permissionLevel=9 → fromLevel=NONE → clampGrantedLevel=VIEW（不是 CONTROL！）
		assertEquals(WindowPermission.VIEW, ShareAcl.clampGrantedLevel(WindowPermission.fromLevel(9)));
		assertEquals(WindowPermission.VIEW, ShareAcl.clampGrantedLevel(null));
		assertEquals(WindowPermission.VIEW, ShareAcl.clampGrantedLevel(WindowPermission.NONE));
		assertEquals(WindowPermission.INTERACT, ShareAcl.clampGrantedLevel(WindowPermission.INTERACT));
		assertEquals(WindowPermission.CONTROL, ShareAcl.clampGrantedLevel(WindowPermission.CONTROL));
	}

	@Test
	@DisplayName("时长解析：10m/1h/30s/2d/裸秒合法；非法/超界/溢出拒绝；封顶 30d")
	void durationParsing() {
		assertEquals(600_000L, ShareAcl.parseDurationMs("10m"));
		assertEquals(3_600_000L, ShareAcl.parseDurationMs("1h"));
		assertEquals(30_000L, ShareAcl.parseDurationMs("30s"));
		assertEquals(172_800_000L, ShareAcl.parseDurationMs("2d"));
		assertEquals(90_000L, ShareAcl.parseDurationMs("90"), "裸数字=秒");
		assertEquals(-1, ShareAcl.parseDurationMs("abc"));
		assertEquals(-1, ShareAcl.parseDurationMs(""));
		assertEquals(-1, ShareAcl.parseDurationMs(null));
		assertEquals(-1, ShareAcl.parseDurationMs("-5"));
		assertEquals(-1, ShareAcl.parseDurationMs("0"));
		assertEquals(-1, ShareAcl.parseDurationMs("10x"));
		assertEquals(-1, ShareAcl.parseDurationMs("9223372036854775807h"), "乘法溢出必须拒绝");
		assertEquals(30L * 86_400_000L, ShareAcl.parseDurationMs("100d"), "超 30d 封顶而非变相永久");
		assertEquals(600L, ShareAcl.parseDurationSeconds("10m"));
	}

	@Test
	@DisplayName("expiryOf：duration>0 → now+duration；<=0 → 永久哨兵")
	void expiryOf() {
		assertEquals(0L, ShareAcl.expiryOf(5_000L, 0));
		assertEquals(6_000L, ShareAcl.expiryOf(5_000L, 1_000));
		assertEquals(WindowGrant.NEVER_EXPIRES, ShareAcl.expiryOf(1L, -3));
	}

	@Test
	@DisplayName("鉴权纯函数：canManageWindow / canRespondToInvite 无旁路")
	void authGates() {
		assertTrue(ShareAcl.canManageWindow(OWNER, OWNER));
		assertFalse(ShareAcl.canManageWindow(OWNER, PLAYER), "观众/管理员伪造包必须 false");
		assertFalse(ShareAcl.canManageWindow(null, OWNER));
		assertFalse(ShareAcl.canManageWindow(OWNER, null));
		assertFalse(ShareAcl.canRespondToInvite(false, true), "窗口已注销后旧应答作废");
		assertFalse(ShareAcl.canRespondToInvite(true, false), "非 owner 应答邀请被拒");
		assertTrue(ShareAcl.canRespondToInvite(true, true));
	}

	@Test
	@DisplayName("describeGrant：级别/来源/剩余时间可读，过期与永久可区分")
	void describeGrant() {
		long now = 100_000L;
		WindowGrant timed = new WindowGrant(PLAYER, WindowPermission.VIEW, OWNER, now,
			now + 50_000L, WindowGrant.GrantSource.INVITE);
		String s = ShareAcl.describeGrant(timed, now);
		assertTrue(s.contains("VIEW"), s);
		assertTrue(s.contains("invite"), s);
		assertTrue(s.contains("50s left"), s);
		assertTrue(ShareAcl.describeGrant(timed, now + 60_000L).contains("expired"));
		assertTrue(ShareAcl.describeGrant(
			WindowGrant.permanent(PLAYER, WindowPermission.CONTROL, OWNER, now, WindowGrant.GrantSource.MANUAL), now)
			.contains("forever"));
		assertTrue(ShareAcl.describeGrant(WindowGrant.tombstone(PLAYER, OWNER, now, WindowGrant.GrantSource.MANUAL), now)
			.contains("revoked"));
		assertEquals("none", ShareAcl.describeGrant(null, now));
	}
}
