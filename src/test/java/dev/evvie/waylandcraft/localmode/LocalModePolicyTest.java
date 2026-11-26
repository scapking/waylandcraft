package dev.evvie.waylandcraft.localmode;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.localmode.LocalModePolicy.WindowDisplayMode;

/**
 * 反作弊安全审计的防回归锁（缺陷 4 + 需求 B）：
 *  - 物品/存活窗口 payload 只跟随 serverHasMod——任何档位不得放行；
 *  - 客户端背包写入只在"无 mod 服 + 用户显式 items 档"时发生；
 *  - auto/display 档在任何服务端组合下都不写背包；
 *  - 依赖服务端 mod 的功能门（requireServerMod）在 serverHasMod=false 恒拒绝。
 */
class LocalModePolicyTest {

	@Test
	@DisplayName("红线：serverHasMod=false 时物品 payload 发送门恒拒绝（不看任何档位）")
	void payloadGateIsStrictlyServerMod() {
		assertFalse(LocalModePolicy.itemPayloadSendAllowed(false));
		assertTrue(LocalModePolicy.itemPayloadSendAllowed(true));
	}

	@Test
	@DisplayName("红线：auto 档在无 mod 服绝不放行客户端背包写入")
	void autoTierNeverWritesInventory() {
		assertFalse(LocalModePolicy.clientItemWriteAllowed(false, "auto"));
		assertFalse(LocalModePolicy.clientItemWriteAllowed(false, null));
		assertFalse(LocalModePolicy.clientItemWriteAllowed(false, ""));
	}

	@Test
	@DisplayName("红线：display 档在两种服务端下都不放行客户端背包写入")
	void displayTierNeverWritesInventory() {
		assertFalse(LocalModePolicy.clientItemWriteAllowed(false, "display"));
		assertFalse(LocalModePolicy.clientItemWriteAllowed(true, "display"));
	}

	@Test
	@DisplayName("红线：有 mod 服时客户端同样零背包写入（物品必须由服务端发放）")
	void modServerNeverAllowsClientWriteEvenWithItemsTier() {
		assertFalse(LocalModePolicy.clientItemWriteAllowed(true, "items"));
	}

	@Test
	@DisplayName("items 档是无 mod 服客户端背包写入的唯一放行条件（用户显式选择）")
	void itemsTierAllowsClientWriteOnlyWithoutServerMod() {
		assertTrue(LocalModePolicy.clientItemWriteAllowed(false, "items"));
	}

	@Test
	@DisplayName("requireServerMod 门：serverHasMod=false → 恒拒绝（依赖服务端的功能静默不可用）")
	void featureGateRejectsWithoutServerMod() {
		assertFalse(LocalModePolicy.serverModFeatureAllowed(false));
		assertTrue(LocalModePolicy.serverModFeatureAllowed(true));
	}

	@Test
	@DisplayName("auto 档解析：有 mod=ITEM，无 mod=LOCAL_DIRECT（维持现状行为）")
	void autoFollowsProbeResult() {
		assertEquals(WindowDisplayMode.ITEM, LocalModePolicy.resolveDisplayMode(true, "auto", false));
		assertEquals(WindowDisplayMode.LOCAL_DIRECT, LocalModePolicy.resolveDisplayMode(false, "auto", false));
	}

	@Test
	@DisplayName("items 档：无 mod 服也走物品模式（客户端自维护）")
	void itemsTierForcesItemMode() {
		assertEquals(WindowDisplayMode.ITEM, LocalModePolicy.resolveDisplayMode(false, "items", false));
		assertEquals(WindowDisplayMode.ITEM, LocalModePolicy.resolveDisplayMode(true, "items", false));
	}

	@Test
	@DisplayName("display 档：强制本地直显，即使服务端装了 mod")
	void displayTierForcesLocalDirect() {
		assertEquals(WindowDisplayMode.LOCAL_DIRECT, LocalModePolicy.resolveDisplayMode(true, "display", false));
		assertEquals(WindowDisplayMode.LOCAL_DIRECT, LocalModePolicy.resolveDisplayMode(false, "display", false));
	}

	@Test
	@DisplayName("运行期直显覆盖（give 静默失败自愈）优先级最高：压制一切档位与探测")
	void overrideBeatsEverything() {
		assertEquals(WindowDisplayMode.LOCAL_DIRECT, LocalModePolicy.resolveDisplayMode(true, "items", true));
		assertEquals(WindowDisplayMode.LOCAL_DIRECT, LocalModePolicy.resolveDisplayMode(true, "auto", true));
	}

	@Test
	@DisplayName("非法/未知档位值一律归一化为 auto（宁可保守，不默认放开客户端物品）")
	void unknownValuesNormalizeToAuto() {
		assertEquals("auto", LocalModePolicy.normalizeLocalItems(null));
		assertEquals("auto", LocalModePolicy.normalizeLocalItems(""));
		assertEquals("items", LocalModePolicy.normalizeLocalItems("Items  ")); // 大小写/空格宽容
		assertEquals("auto", LocalModePolicy.normalizeLocalItems("garbage"));
		assertEquals("items", LocalModePolicy.normalizeLocalItems("ITEMS"));
		assertEquals("display", LocalModePolicy.normalizeLocalItems(" display"));
		// garbage → auto 语义：有 mod 服走物品
		assertEquals(WindowDisplayMode.ITEM, LocalModePolicy.resolveDisplayMode(true, "garbage", false));
	}

	@Test
	@DisplayName("归一化不会把非法值放大为 items：无 mod + 垃圾值 → 直显而非客户端写背包")
	void garbageNeverEscalatesToItems() {
		assertEquals(WindowDisplayMode.LOCAL_DIRECT, LocalModePolicy.resolveDisplayMode(false, "items2", false));
		assertFalse(LocalModePolicy.clientItemWriteAllowed(false, "items2"));
	}
}
