package dev.evvie.waylandcraft;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

/**
 * ⑨ v1.4.9/1.5.0 布局高度基准测试。
 * - layerLiftDelta（用户裁定：居中对齐最高窗，不再逐窗参考底部方块）：
 *   整层统一防插地兜底，只有层内最低渲染底边低于 地面+净空 时补齐差额——
 *   层内所有窗被同一 delta 抬升，居中对齐关系保持不变；只抬不压。
 * - cubeLayerOf（用户实测"向上堆叠时窗口重叠"）：cube 模板序号 → 层号。
 *   红线场景：窗口关闭后序号保留（空洞），新窗口复用低序号 → list（创建序）
 *   ≠ 序号序，applyLayerHeights 若按 list 顺序切层 = 层错乱 = 垂直重叠。
 *   修复 = 排序对齐后再切层，本测试钉死层号语义（与水平摆位分组同口径）。
 */
class WindowLayoutLiftTest {
	
	// ===== 整层防插地兜底 =====
	
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
	
	// ===== cube 层号（堆叠重叠修复） =====
	
	@Test
	void cubeLayerBoundaries() {
		int layerSize = 8; // perFace=2 的默认场景
		assertEquals(0, WindowLayoutManager.cubeLayerOf(0, layerSize));
		assertEquals(0, WindowLayoutManager.cubeLayerOf(layerSize - 1, layerSize), "首层最后一个序号");
		assertEquals(1, WindowLayoutManager.cubeLayerOf(layerSize, layerSize), "进入第二层");
		assertEquals(2, WindowLayoutManager.cubeLayerOf(layerSize * 2 + 3, layerSize));
	}
	
	@Test
	void negativeAltIndexClampsToFirstLayer() {
		// layoutAltIndex=-1（未参与 cube 排布的窗口）：钳 0，与分组循环同口径
		assertEquals(0, WindowLayoutManager.cubeLayerOf(-1, 8));
		assertEquals(0, WindowLayoutManager.cubeLayerOf(-7, 8));
	}
	
	@Test
	void holeReuseScenarioKeepsLayerOrder() {
		// 红线复现：4 窗 perFace×4=16/层。w0..w2 序号 0,1,2（层0）；关闭 w0 后
		// 新窗复用序号 0 → list 创建序 [w1(1), w2(2), w3(0)]。
		// 层号必须按**序号**而非 list 位置：w1/w2/w3 全是层 0（16/层时序号 0..2 都在首层）。
		// 用 layerSize=2 演示跨层错位修复语义：序号 1,2,0 → 层 0,1,0（排序后层0组在前）
		assertEquals(0, WindowLayoutManager.cubeLayerOf(1, 2));
		assertEquals(1, WindowLayoutManager.cubeLayerOf(2, 2));
		assertEquals(0, WindowLayoutManager.cubeLayerOf(0, 2));
	}
}
