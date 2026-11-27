package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.UUID;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.network.ImmerseServer.ImmerseMode;
import dev.evvie.waylandcraft.network.ImmerseServer.ImmersePlayerState;
import dev.evvie.waylandcraft.network.ImmerseServer.ModeResolution;
import dev.evvie.waylandcraft.network.ImmerseServer.ToggleAction;
import dev.evvie.waylandcraft.network.ImmerseServer.ToggleDecision;

/**
 * v1.5.0 沉浸模式服务端权威层纯逻辑（无 MC 依赖）：
 * - mode 解析容错（null/空/非法/大小写 → STILL + 拒绝标记）。
 * - 超时保护边界（29:59 / 30:00 / 30:01；60s 预警窗口含起点不含到期）。
 * - 切换状态机幂等（重复 enable 同/异 mode、enable→disable→enable、无状态 disable）。
 */
class ImmerseServerLogicTest {

	private static final long MINUTE = 60L * 1000L;

	private static ImmersePlayerState state(ImmerseMode mode, long enterMs) {
		return new ImmersePlayerState(mode, enterMs, 1.0, 2.0, 3.0, 45.0f, -10.0f);
	}

	// ==================== mode 解析容错 ====================

	@Test
	@DisplayName("mode 解析：合法 still/ghost 精确放行，wire 值回环一致")
	void resolveModeAcceptsExactWireValues() {
		ModeResolution still = ImmerseServer.resolveMode("still");
		assertFalse(still.rejected());
		assertEquals(ImmerseMode.STILL, still.mode());
		assertEquals("still", still.mode().wire);

		ModeResolution ghost = ImmerseServer.resolveMode("ghost");
		assertFalse(ghost.rejected());
		assertEquals(ImmerseMode.GHOST, ghost.mode());
		assertEquals("ghost", ghost.mode().wire);
	}

	@Test
	@DisplayName("mode 解析容错：null/空/空白/非法/大小写不一致 → STILL + 拒绝标记")
	void resolveModeRejectsIllegalValuesAsStill() {
		String[] illegal = { null, "", " ", "fly", "STILL", "Ghost", "still ", "spectator" };
		for (String raw : illegal) {
			ModeResolution res = ImmerseServer.resolveMode(raw);
			assertTrue(res.rejected(), "raw=" + raw + " 应带拒绝标记");
			assertEquals(ImmerseMode.STILL, res.mode(), "raw=" + raw + " 应回落 STILL");
			assertEquals("still", res.mode().wire, "回执应为 still");
		}
	}

	// ==================== 超时保护（时钟注入） ====================

	@Test
	@DisplayName("超时边界：29:59 未到期、30:00 整到期、30:01 已到期")
	void shouldAutoExitBoundaries() {
		long enter = 1_000_000L;
		long at29m59s = enter + 29 * MINUTE + 59_000L;
		long at30m = enter + 30 * MINUTE;
		long at30m1s = enter + 30 * MINUTE + 1000L;

		assertFalse(ImmerseServer.shouldAutoExit(enter, at29m59s), "29:59 不应到期");
		assertTrue(ImmerseServer.shouldAutoExit(enter, at30m), "30:00 整应到期");
		assertTrue(ImmerseServer.shouldAutoExit(enter, at30m1s), "30:01 应到期");

		// 时钟倒退/同刻：不应误判到期
		assertFalse(ImmerseServer.shouldAutoExit(enter, enter), "刚进入不应到期");
	}

	@Test
	@DisplayName("60s 预警窗口：29:39 不提示、29:40 起提示、29:59 仍提示、30:00 起改走退出")
	void shouldWarnTimeoutWindow() {
		long enter = 0L;
		long warnStart = 30 * MINUTE - 60_000L; // 29:40

		assertFalse(ImmerseServer.shouldWarnTimeout(enter, warnStart - 1), "29:39.999 不应预警");
		assertTrue(ImmerseServer.shouldWarnTimeout(enter, warnStart), "29:40 整应预警");
		assertTrue(ImmerseServer.shouldWarnTimeout(enter, 29 * MINUTE + 59_000L), "29:59 应预警");
		assertFalse(ImmerseServer.shouldWarnTimeout(enter, 30 * MINUTE), "30:00 起应走到期而非预警");
	}

	// ==================== 切换状态机幂等 ====================

	@Test
	@DisplayName("重复 enable 同 mode = NOOP（幂等），activeAfter 恒 true")
	void repeatedEnableSameModeIsNoop() {
		for (ImmerseMode mode : ImmerseMode.values()) {
			ImmersePlayerState existing = state(mode, 0L);
			ToggleDecision d = ImmerseServer.decideToggle(existing, true, mode);
			assertEquals(ToggleAction.NOOP, d.action(), "重复 enable " + mode + " 应为 NOOP");
			assertEquals(mode, d.mode());
			assertTrue(d.activeAfter(), "NOOP 后仍应处于沉浸");
		}
	}

	@Test
	@DisplayName("mode 变更 = SWITCH（先清旧再入新），生效 mode 为新请求")
	void modeChangeIsSwitch() {
		ToggleDecision toGhost = ImmerseServer.decideToggle(state(ImmerseMode.STILL, 0L), true, ImmerseMode.GHOST);
		assertEquals(ToggleAction.SWITCH, toGhost.action());
		assertEquals(ImmerseMode.GHOST, toGhost.mode());
		assertTrue(toGhost.activeAfter());

		ToggleDecision backToStill = ImmerseServer.decideToggle(state(ImmerseMode.GHOST, 0L), true, ImmerseMode.STILL);
		assertEquals(ToggleAction.SWITCH, backToStill.action());
		assertEquals(ImmerseMode.STILL, backToStill.mode());
		assertTrue(backToStill.activeAfter());
	}

	@Test
	@DisplayName("enable→disable→enable 闭环：EXIT 回执旧 mode，再进为 ENTER")
	void enableDisableEnableCycle() {
		// 无状态 enable → ENTER
		ToggleDecision first = ImmerseServer.decideToggle(null, true, ImmerseMode.GHOST);
		assertEquals(ToggleAction.ENTER, first.action());
		assertEquals(ImmerseMode.GHOST, first.mode());
		assertTrue(first.activeAfter());

		// 有状态 disable → EXIT，mode 回执为实际生效过的旧 mode
		ImmersePlayerState existing = state(ImmerseMode.GHOST, 0L);
		ToggleDecision exit = ImmerseServer.decideToggle(existing, false, ImmerseMode.STILL);
		assertEquals(ToggleAction.EXIT, exit.action());
		assertEquals(ImmerseMode.GHOST, exit.mode(), "disable 回执应如实反映旧 mode");
		assertFalse(exit.activeAfter());

		// 退出后再 enable → ENTER（非 SWITCH）
		ToggleDecision reEnter = ImmerseServer.decideToggle(null, true, ImmerseMode.STILL);
		assertEquals(ToggleAction.ENTER, reEnter.action());
		assertTrue(reEnter.activeAfter());
	}

	@Test
	@DisplayName("无状态 disable = NOOP + 回执 false（幂等，不臆造状态）")
	void disableWithoutStateIsNoopWithInactiveReceipt() {
		ToggleDecision d = ImmerseServer.decideToggle(null, false, ImmerseMode.STILL);
		assertEquals(ToggleAction.NOOP, d.action());
		assertFalse(d.activeAfter(), "无状态 disable 回执应为未激活");
	}

	@Test
	@DisplayName("isImmersed/getActiveMode：空表查询安全，getActiveMode 未沉浸返回 null")
	void stateMapQueriesAreSafeOnEmptyTable() {
		UUID nobody = UUID.randomUUID();
		assertFalse(ImmerseServer.isImmersed(nobody), "空表不应误报沉浸");
		assertNull(ImmerseServer.getActiveMode(nobody), "未沉浸 mode 应为 null");
		assertTrue(ImmerseServer.immersedPlayers().isEmpty(), "空表快照应为空集");
	}
}
