package dev.evvie.waylandcraft;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

/**
 * ⑨ v1.4.9 布局高度基准测试（用户裁定：居中对齐最高窗，不再逐窗参考底部方块）。
 * 语义锚点：
 * - layerLiftDelta 是"整层统一防插地兜底"：只有层内最低渲染底边低于 地面+净空 时
 *   补齐差额——层内所有窗被同一 delta 抬升，**居中对齐关系保持不变**；
 * - 已高于地面+净空的层不得被压低（只抬不压，与旧贴地钳制的"只抬升"语义一致）。
 */
class WindowLayoutLiftTest {
	
	@Test
	void noLiftWhenBottomAboveGround() {
		// 层中心 70、最高窗半高 3 → 最低底边 67；地面 60+净空 0.4 → 无需抬
		assertEquals(0.0, WindowLayoutManager.layerLiftDelta(67.0, 60, 0.4), 1e-9);
	}
	
	@Test
	void liftCoversGapExactly() {
		// 底边 58.5、地面 60、净空 0.4 → 整层抬 1.9（差额精确，不多不少）
		assertEquals(1.9, WindowLayoutManager.layerLiftDelta(58.5, 60, 0.4), 1e-9);
	}
	
	@Test
	void tallWindowDrivesLiftForWholeLayer() {
		// 场景还原：同层高低两窗（中心对齐同一水平线 64）
		// 高窗半高 6 → 底边 58（会插地）；矮窗半高 1.5 → 底边 62.5（不插地）
		// 整层兜底按**最低底边**（高窗的 58）计算 → 抬 2.9，两窗一起抬、对齐关系不变
		assertEquals(2.9, WindowLayoutManager.layerLiftDelta(58.0, 60, 0.9), 1e-9);
	}
	
	@Test
	void deepUndergroundLiftsFully() {
		// 极端：窗底深埋（-10），地面 0 净空 0.4 → 抬 10.4
		assertEquals(10.4, WindowLayoutManager.layerLiftDelta(-10.0, 0, 0.4), 1e-9);
	}
}
