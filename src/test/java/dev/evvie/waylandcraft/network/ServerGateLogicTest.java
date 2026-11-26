package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.HashMap;
import java.util.Map;
import java.util.UUID;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P0-1/P1-5/P1-7/P2-3 修复证明：服务端门控纯逻辑。
 * - P0-1 鉴权门：非 op 一律拒绝（任何 action 无旁路）。
 * - P1-5 alive_windows payload 限长。
 * - P1-7 注册速率门（4/s 窗口）+ title 截断。
 * - P2-3 窗口 state 坐标/尺寸 clamp。
 */
class ServerGateLogicTest {

	@Test
	@DisplayName("P0-1: 权限命令鉴权门——无 GAMEMASTER 权限一律拒")
	void permissionCommandGateBlocksNonOp() {
		assertFalse(SharedWindowServerHandler.isPermissionCommandAuthorized(false));
		assertTrue(SharedWindowServerHandler.isPermissionCommandAuthorized(true));
	}

	@Test
	@DisplayName("P1-5: alive_windows 1024 通过、1025 拒绝、null 放行给调用方跳过")
	void aliveWindowsLengthGate() {
		assertTrue(WaylandCraftNetworking.isAliveWindowsAcceptable(null));
		assertTrue(WaylandCraftNetworking.isAliveWindowsAcceptable(new long[0]));
		assertTrue(WaylandCraftNetworking.isAliveWindowsAcceptable(new long[1024]));
		assertFalse(WaylandCraftNetworking.isAliveWindowsAcceptable(new long[1025]));
	}

	@Test
	@DisplayName("P1-7: 注册速率门——同窗口内前 4 次放行，第 5 次拒绝；跨窗口重置")
	void registerRateGateAllowsFourPerSecond() {
		Map<UUID, long[]> rates = new HashMap<>();
		UUID p = UUID.randomUUID();
		long t0 = 1_000_000L;

		for (int i = 0; i < SharedWindowServerHandler.MAX_REGISTERS_PER_SECOND; i++) {
			assertTrue(SharedWindowServerHandler.allowRegister(rates, p, t0 + i, 1000,
				SharedWindowServerHandler.MAX_REGISTERS_PER_SECOND), "第 " + (i + 1) + " 次应放行");
		}
		assertFalse(SharedWindowServerHandler.allowRegister(rates, p, t0 + 5, 1000,
			SharedWindowServerHandler.MAX_REGISTERS_PER_SECOND), "窗口内第 5 次必须拒绝");

		// 其他玩家不受影响
		assertTrue(SharedWindowServerHandler.allowRegister(rates, UUID.randomUUID(), t0 + 5, 1000,
			SharedWindowServerHandler.MAX_REGISTERS_PER_SECOND));

		// 1s 后新窗口重新放行
		assertTrue(SharedWindowServerHandler.allowRegister(rates, p, t0 + 1001, 1000,
			SharedWindowServerHandler.MAX_REGISTERS_PER_SECOND));
	}

	@Test
	@DisplayName("P1-7: title 截断到 64 并去除换行/控制空白（防广播与日志放大）")
	void titleSanitized() {
		String longTitle = "x".repeat(200);
		assertEquals(64, SharedWindowServerHandler.sanitizeTitle(longTitle).length());

		assertEquals("", SharedWindowServerHandler.sanitizeTitle(null));
		assertEquals("", SharedWindowServerHandler.sanitizeTitle(""));
		assertEquals("a b c", SharedWindowServerHandler.sanitizeTitle("a\nb\rc"));
		assertEquals("short", SharedWindowServerHandler.sanitizeTitle("short"));
	}

	@Test
	@DisplayName("P2-3: state 坐标 clamp 到 ±1e7、尺寸 clamp 到 [0, 8192]")
	void stateClamps() {
		assertEquals(10_000_000, SharedWindowServerHandler.clampStateCoord(Integer.MAX_VALUE));
		assertEquals(-10_000_000, SharedWindowServerHandler.clampStateCoord(Integer.MIN_VALUE));
		assertEquals(123, SharedWindowServerHandler.clampStateCoord(123));
		assertEquals(-123, SharedWindowServerHandler.clampStateCoord(-123));

		assertEquals(8192, SharedWindowServerHandler.clampStateDimension(Integer.MAX_VALUE));
		assertEquals(0, SharedWindowServerHandler.clampStateDimension(-500));
		assertEquals(0, SharedWindowServerHandler.clampStateDimension(Integer.MIN_VALUE));
		assertEquals(1920, SharedWindowServerHandler.clampStateDimension(1920));
	}
}
