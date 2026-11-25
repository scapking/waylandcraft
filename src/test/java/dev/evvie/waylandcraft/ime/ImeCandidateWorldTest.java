package dev.evvie.waylandcraft.ime;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.ime.ImeCandidateLayout.Frame;
import dev.evvie.waylandcraft.ime.ImeCandidateLayout.Rect;

/**
 * 世界形态候选窗纯函数单测（sync14）：锚定映射（ti3 光标 rect / surface 局部
 * → 窗口平面局部像素）、命中回传（平面局部 → popup 表面像素）、显隐状态机。
 *
 * 全部为 {@link ImeCandidateLayout} 的静态纯函数，零 MC/GL 依赖——锚定与命中
 * 的坐标链在这里逐条锁死，渲染侧（ImeCandidateWorld）只做数据搬运。与既有
 * ImeCandidateLayoutTest（GUI 形态）平行，不改动任何既有断言。
 */
class ImeCandidateWorldTest {

	// ── positionWorldPopup：锚定映射 ──

	@Test
	void worldAnchorsBelowCursorRectWhenFits() {
		// 窗口 400x300 像素；光标矩形在 (50,120) 尺寸 2x18；候选窗 120x30。
		Rect r = ImeCandidateLayout.positionWorldPopup(
				new Rect(50, 120, 2, 18), 400, 300, 120, 30, 4);
		assertNotNull(r);
		assertEquals(50, r.x());               // 水平左对齐光标 x
		assertEquals(120 + 18 + 4, r.y());     // 光标下方 + 间距
		assertEquals(120, r.w());
		assertEquals(30, r.h());
	}

	@Test
	void worldFlipsAboveCursorRectWhenNoRoomBelow() {
		// 光标贴窗口底：下方放不下 → 翻到光标上方（ay - gap - popH）。
		Rect r = ImeCandidateLayout.positionWorldPopup(
				new Rect(100, 290, 2, 8), 400, 300, 120, 30, 4);
		assertNotNull(r);
		assertEquals(290 - 4 - 30, r.y());  // 上方
		assertTrue(r.y() < 290);
	}

	@Test
	void worldFallsBackToWindowBottomWhenNoCursorRect() {
		// 无 ti3 光标矩形（null）：回退锚窗口 quad 底部中央。
		Rect r = ImeCandidateLayout.positionWorldPopup(null, 400, 300, 120, 30, 4);
		assertNotNull(r);
		assertEquals((400 - 120) / 2, r.x());  // 水平居中
		assertEquals(300 + 4, r.y());          // 窗口底 + 间距
	}

	@Test
	void worldTreatsZeroSizeCursorRectAsMissing() {
		// 退化 0x0 光标矩形（应用发了事件但没填尺寸）→ 与 null 同款回退。
		Rect a = ImeCandidateLayout.positionWorldPopup(new Rect(50, 60, 0, 0), 400, 300, 120, 30, 4);
		Rect fallback = ImeCandidateLayout.positionWorldPopup(null, 400, 300, 120, 30, 4);
		assertEquals(fallback, a);
	}

	@Test
	void worldRejectsNonPositiveFrameSize() {
		assertNull(ImeCandidateLayout.positionWorldPopup(new Rect(1, 1, 2, 2), 400, 300, 0, 30, 4));
		assertNull(ImeCandidateLayout.positionWorldPopup(new Rect(1, 1, 2, 2), 400, 300, 120, 0, 4));
		assertNull(ImeCandidateLayout.positionWorldPopup(new Rect(1, 1, 2, 2), 0, 300, 120, 30, 4));
	}

	@Test
	void worldClampsCursorXIntoWindowWhenOffscreen() {
		// 光标 x 越界（超出窗口宽）→ 收回窗口右缘内，保证候选窗贴在窗口上。
		Rect r = ImeCandidateLayout.positionWorldPopup(
				new Rect(10000, 50, 2, 10), 400, 300, 120, 30, 4);
		assertNotNull(r);
		assertTrue(r.x() >= 0);
		assertTrue(r.x() + r.w() <= 400);
	}

	@Test
	void worldAnchorsWiderThanWindowKeepsFrameSize() {
		// 候选窗比窗口还宽：世界空间不做宽度收缩（无屏幕边界），保持帧 1:1，
		// 只是水平起点收回 0（越界部分浮进世界，仍可点选）。
		Rect r = ImeCandidateLayout.positionWorldPopup(null, 100, 300, 200, 30, 4);
		assertNotNull(r);
		assertEquals(200, r.w());
		assertEquals((100 - 200) / 2, r.x()); // 可为负：浮进世界
	}

	// ── surfaceLocalToPlaneLocal：坐标空间换算（intersect 正变换的逆）──

	@Test
	void surfaceLocalToPlaneLocalMatchesIntersectForwardTransform() {
		// WindowDisplay.intersect 正变换：localCoords = geometryLocal + (geom.x, geom.y)。
		// 逆：plane = surfaceLocal + (xSubpos, ySubpos) - (geom.x, geom.y)。
		// 用一组非零偏移锁死符号方向（根因风险：geometry 偏移符号搞反→候选窗偏移窗口一截）。
		double[] p = ImeCandidateLayout.surfaceLocalToPlaneLocal(30, 40, 10, 20, 5, 8);
		assertArrayEquals(new double[] { 30 + 10 - 5, 40 + 20 - 8 }, p, 1e-9);
	}

	@Test
	void surfaceLocalIdentityWhenNoOffsets() {
		double[] p = ImeCandidateLayout.surfaceLocalToPlaneLocal(12.5, 7.25, 0, 0, 0, 0);
		assertArrayEquals(new double[] { 12.5, 7.25 }, p, 1e-9);
	}

	// ── worldHitToPopupLocal：命中 → popup 表面像素 ──

	@Test
	void worldHitMapsToPopupLocalPixel() {
		Rect popup = new Rect(100, 138, 120, 30);
		// 命中点平面局部 (110.7, 140.2) → popup 局部 (10, 2)。
		int[] local = ImeCandidateLayout.worldHitToPopupLocal(popup, 110.7, 140.2);
		assertNotNull(local);
		assertArrayEquals(new int[] { 10, 2 }, local);
	}

	@Test
	void worldHitClampsToFrameInteriorAtEdges() {
		Rect popup = new Rect(0, 0, 120, 30);
		// 右/下边界命中点 floor 后恰落在框内最后一像素（contains 是开区间，
		// 边界内 0.001 处命中应映射到 popW-1 / popH-1）。
		int[] local = ImeCandidateLayout.worldHitToPopupLocal(popup, 119.99, 29.99);
		assertNotNull(local);
		assertArrayEquals(new int[] { 119, 29 }, local);
	}

	@Test
	void worldHitOutsideReturnsNull() {
		Rect popup = new Rect(100, 100, 50, 20);
		assertNull(ImeCandidateLayout.worldHitToPopupLocal(popup, 99.9, 110));  // 左外
		assertNull(ImeCandidateLayout.worldHitToPopupLocal(popup, 150, 110));   // 右外（开区间）
		assertNull(ImeCandidateLayout.worldHitToPopupLocal(popup, 120, 99.5));  // 上外
		assertNull(ImeCandidateLayout.worldHitToPopupLocal(popup, 120, 120));   // 下外
		assertNull(ImeCandidateLayout.worldHitToPopupLocal(null, 120, 110));
	}

	// ── decideShowingWorld / shouldReshowCurrentWorld：显隐（GUI 语境参数化）──

	@Test
	void worldHidesWhenWindowGoneEvenIfVisible() {
		// 目标窗口销毁/失焦 → 立即隐藏（对齐 GUI 的"无聚焦框即隐藏"）。
		Frame vis = new Frame(10, 10, 1, true, new byte[400]);
		assertFalse(ImeCandidateLayout.decideShowingWorld(true, vis, false));
	}

	@Test
	void worldKeepsStateWhenNoNewFrameAndWindowInPlace() {
		assertTrue(ImeCandidateLayout.decideShowingWorld(true, null, true));
		assertFalse(ImeCandidateLayout.decideShowingWorld(false, null, true));
	}

	@Test
	void worldFollowsNativeVisibilityWhenWindowInPlace() {
		Frame vis = new Frame(10, 10, 1, true, new byte[400]);
		Frame inv = new Frame(0, 0, 2, false, null);
		assertTrue(ImeCandidateLayout.decideShowingWorld(false, vis, true));
		assertFalse(ImeCandidateLayout.decideShowingWorld(true, inv, true));
	}

	@Test
	void worldReshowCurrentMatchesGuiSemantics() {
		Frame vis = new Frame(10, 10, 1, true, new byte[400]);
		// 窗口在场、无新帧、保留帧可见 → 补显（与 GUI 焦点恢复补显同款）。
		assertTrue(ImeCandidateLayout.shouldReshowCurrentWorld(false, null, true, vis));
		// 窗口不在场 → 绝不补显。
		assertFalse(ImeCandidateLayout.shouldReshowCurrentWorld(false, null, false, vis));
	}
}
