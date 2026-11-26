package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P1-8 修复证明（纯函数层）：XTest 注入前把不可信的窗口内坐标钳进 [0, 窗口尺寸]。
 *
 * 越权场景：payload.x/y 未钳位时，远端 viewer 可以把根坐标算到
 * （x11RootX + 1e18 / NaN / 负数），拖走 owner 的全局 X 指针到窗口外
 * （终端/桌面/其他应用）再点击。钳位后指针永远落在被共享窗口矩形内。
 * （按键注入的 raise+setInputFocus 验证走 X11Interaction.raiseAndFocusWindow，
 * 依赖真实 X server，属真机 --ignored 范畴；此处覆盖可抽纯的坐标决策。）
 */
class ClientInteractionClampTest {

	@Test
	@DisplayName("正常窗口内坐标原样通过")
	void inRangePassthrough() {
		assertEquals(0.0, SharedWindowClientHandler.clampToWindow(0.0, 800));
		assertEquals(400.5, SharedWindowClientHandler.clampToWindow(400.5, 800));
		assertEquals(800.0, SharedWindowClientHandler.clampToWindow(800.0, 800));
	}

	@Test
	@DisplayName("负数/超限坐标钳回边界，绝不放行窗口外")
	void outOfRangeClamped() {
		assertEquals(0.0, SharedWindowClientHandler.clampToWindow(-1.0, 800));
		assertEquals(0.0, SharedWindowClientHandler.clampToWindow(-1e18, 800));
		assertEquals(800.0, SharedWindowClientHandler.clampToWindow(1e18, 800));
		assertEquals(800.0, SharedWindowClientHandler.clampToWindow(123456.0, 800));
	}

	@Test
	@DisplayName("NaN/±Infinity 归 0（恶意 double 不得变成根坐标垃圾值）")
	void nonFiniteFallsBackToZero() {
		assertEquals(0.0, SharedWindowClientHandler.clampToWindow(Double.NaN, 800));
		assertEquals(0.0, SharedWindowClientHandler.clampToWindow(Double.POSITIVE_INFINITY, 800));
		assertEquals(0.0, SharedWindowClientHandler.clampToWindow(Double.NEGATIVE_INFINITY, 800));
	}

	@Test
	@DisplayName("窗口尺寸未知（<=0）时坐标一律归 0，不注入未定义区域")
	void unknownWindowSizePinsToOrigin() {
		assertEquals(0.0, SharedWindowClientHandler.clampToWindow(42.0, 0));
		assertEquals(0.0, SharedWindowClientHandler.clampToWindow(42.0, -5));
	}
}
