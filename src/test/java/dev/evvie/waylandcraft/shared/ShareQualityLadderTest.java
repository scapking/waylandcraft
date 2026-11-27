package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

/**
 * ⑨ v1.4.9 自适应画质阶梯测试（默认最高档 + 负载自动升降，用户裁定）。
 * 语义锚点：
 * - 降档（既有 nextLadderValue）：严格更小的下一档，阶梯底返回 -1（丢帧保护，绝不越底）；
 * - 升档（新 nextLadderUpValue）：严格更大的最小档，阶梯顶返回 -1（封顶=默认最高）；
 * - current 不在阶梯中（如手动设了 0.9）时两个方向都要给出合理邻档。
 */
class ShareQualityLadderTest {
	
	private static final float[] LADDER = ImageCapture.CaptureConfig.DEFAULT_JPEG_QUALITY_LADDER; // {1.0, 0.85, 0.7, 0.55, 0.4}
	
	@Test
	void ladderDefaultsUnchanged() {
		assertEquals(1.0f, LADDER[0], 1e-6, "阶梯顶保持 1.0（降级阶梯是既有行为，不牵扯）");
		assertEquals(0.4f, LADDER[LADDER.length - 1], 1e-6);
	}
	
	@Test
	void downWalksToFloor() {
		assertEquals(0.85f, WindowShareManager.nextLadderValue(LADDER, 0.95f), 1e-6);
		assertEquals(0.85f, WindowShareManager.nextLadderValue(LADDER, 1.0f), 1e-6);
		assertEquals(0.7f, WindowShareManager.nextLadderValue(LADDER, 0.85f), 1e-6);
		assertEquals(-1f, WindowShareManager.nextLadderValue(LADDER, 0.4f), 1e-6, "阶梯底（既有丢帧保护语义）");
	}
	
	@Test
	void upWalksToCeiling() {
		assertEquals(0.85f, WindowShareManager.nextLadderUpValue(LADDER, 0.7f), 1e-6);
		assertEquals(1.0f, WindowShareManager.nextLadderUpValue(LADDER, 0.85f), 1e-6);
		assertEquals(-1f, WindowShareManager.nextLadderUpValue(LADDER, 1.0f), 1e-6, "阶梯顶封顶（默认最高档语义）");
	}
	
	@Test
	void offLadderCurrentResolvesReasonably() {
		// 手动档 0.9（不在阶梯）：向下降到 0.85、向上升到 1.0
		assertEquals(0.85f, WindowShareManager.nextLadderValue(LADDER, 0.9f), 1e-6);
		assertEquals(1.0f, WindowShareManager.nextLadderUpValue(LADDER, 0.9f), 1e-6);
	}
	
	@Test
	void nullAndEmptyLaddersAreSafe() {
		assertEquals(-1f, WindowShareManager.nextLadderUpValue(null, 0.5f), 1e-6);
		assertEquals(-1f, WindowShareManager.nextLadderUpValue(new float[0], 0.5f), 1e-6);
		assertEquals(-1f, WindowShareManager.nextLadderValue(null, 0.5f), 1e-6);
	}
}
