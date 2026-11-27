package dev.evvie.waylandcraft.immerse;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.immerse.ImmerseState.Mode;

/**
 * ⑩ v1.5.0 沉浸模式状态机测试（时钟注入，无 MC 依赖）。
 * 语义锚点：
 * - 视觉本地立即生效（PENDING 即 isVisualActive=true），隐私以服务端确认为准；
 * - vanilla 服 3s 超时 → local-only（视觉保留、confirmed=false），绝不无声回落 OFF；
 * - 服务端强制退出（受伤/超时/vanish 恢复失败）→ 无条件 OFF；
 * - 重复 on/off 幂等；异 mode 换档回到 PENDING（重新等待确认）。
 */
class ImmerseStateTest {
	
	@Test
	void enableGoesPendingThenVisualActive() {
		ImmerseState s = new ImmerseState();
		assertTrue(s.isOff());
		s.requestOn(Mode.STILL, 1000);
		assertTrue(s.isVisualActive(), "PENDING 即视觉生效（本地即时反馈）");
		assertFalse(s.serverConfirmed(), "确认前不得宣称隐私生效");
		assertEquals(Mode.STILL, s.mode());
	}
	
	@Test
	void serverConfirmPromotesToActive() {
		ImmerseState s = new ImmerseState();
		s.requestOn(Mode.GHOST, 1000);
		s.onServerConfirm(true, Mode.GHOST, 1500);
		assertTrue(s.isVisualActive());
		assertTrue(s.serverConfirmed());
		assertEquals(Mode.GHOST, s.mode());
	}
	
	@Test
	void pendingTimeoutFallsBackToLocalOnly() {
		ImmerseState s = new ImmerseState();
		s.requestOn(Mode.STILL, 1000);
		assertFalse(s.tickPendingTimeout(1000 + 2999), "未到 3s 不迁移");
		assertTrue(s.tickPendingTimeout(1000 + 3000), "到 3s 触发超时迁移");
		assertTrue(s.isVisualActive(), "local-only 视觉必须保留（vanilla 服纯特效）");
		assertFalse(s.serverConfirmed());
		assertTrue(s.tickPendingTimeout(9000) == false, "迁移后不再重复触发");
	}
	
	@Test
	void serverForcedExitLandsOff() {
		ImmerseState s = new ImmerseState();
		s.requestOn(Mode.GHOST, 1000);
		s.onServerConfirm(true, Mode.GHOST, 1100);
		assertTrue(s.isVisualActive());
		s.onServerConfirm(false, Mode.GHOST, 2000);
		assertTrue(s.isOff(), "服务端强制退出（受伤/超时）必须无条件落 OFF");
		assertFalse(s.isVisualActive());
	}
	
	@Test
	void offIsIdempotent() {
		ImmerseState s = new ImmerseState();
		s.requestOff(1000);
		assertTrue(s.isOff());
		s.requestOn(Mode.STILL, 1100);
		s.requestOff(1200);
		s.requestOff(1300);
		assertTrue(s.isOff());
	}
	
	@Test
	void sameModeReenableIsIdempotentAndStaysPending() {
		ImmerseState s = new ImmerseState();
		s.requestOn(Mode.STILL, 1000);
		long entered = 1000;
		s.requestOn(Mode.STILL, 2500);
		assertTrue(s.isVisualActive());
		// 同 mode 重复 on：不重置计时（超时判定不被 CLI 连发延后）
		assertTrue(s.tickPendingTimeout(entered + 3000), "重复 on 不得重置 pending 计时");
	}
	
	@Test
	void modeSwitchRestartsPending() {
		ImmerseState s = new ImmerseState();
		s.requestOn(Mode.STILL, 1000);
		s.onServerConfirm(true, Mode.STILL, 1200);
		assertTrue(s.serverConfirmed());
		s.requestOn(Mode.GHOST, 3000);
		assertEquals(Mode.GHOST, s.mode());
		assertFalse(s.serverConfirmed(), "换档必须重新等待服务端确认");
	}
	
	@Test
	void toggleOffWhilePendingStopsImmediately() {
		ImmerseState s = new ImmerseState();
		s.requestOn(Mode.STILL, 1000);
		s.requestOff(1050);
		assertFalse(s.isVisualActive(), "PENDING 期 off 必须立即终止视觉（本地即时反馈）");
	}
}
