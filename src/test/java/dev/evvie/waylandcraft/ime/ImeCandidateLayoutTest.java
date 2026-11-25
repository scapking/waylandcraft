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
 * 候选窗纯函数层单测（照 WindowLayoutTest 风格，无 MC 运行时依赖）：
 * JNI 帧编解码、边界钳制、显隐状态机。
 */
class ImeCandidateLayoutTest {

	// ── decode / encode ──

	@Test
	void decodeRejectsNullOrTooShort() {
		assertNull(ImeCandidateLayout.decode(null));
		assertNull(ImeCandidateLayout.decode(new byte[0]));
		assertNull(ImeCandidateLayout.decode(new byte[12])); // 缺 visible 字节
	}

	@Test
	void decodeRoundTripsVisibleFrame() {
		byte[] px = {1, 2, 3, 4, 5, 6, 7, 8}; // 2x1 ARGB8888
		byte[] raw = ImeCandidateLayout.encode(2, 1, 7, true, px);
		Frame f = ImeCandidateLayout.decode(raw);
		assertNotNull(f);
		assertEquals(2, f.width());
		assertEquals(1, f.height());
		assertEquals(7, f.seq());
		assertTrue(f.visible());
		assertArrayEquals(px, f.argb());
	}

	@Test
	void decodeInvisibleFrameHasNoPixels() {
		Frame f = ImeCandidateLayout.decode(ImeCandidateLayout.encode(0, 0, 3, false, null));
		assertNotNull(f);
		assertFalse(f.visible());
		assertNull(f.argb());
	}

	@Test
	void decodeRejectsVisibleWithTruncatedPixels() {
		// visible=true 声称 4x4 但像素只给了 8 字节 → 无效帧（防御）。
		byte[] raw = ImeCandidateLayout.encode(4, 4, 1, true, new byte[8]);
		assertNull(ImeCandidateLayout.decode(raw));
	}

	@Test
	void decodeRejectsNegativeDims() {
		byte[] raw = ImeCandidateLayout.encode(1, 1, 1, false, null);
		raw[0] = (byte) 0xFF;
		raw[1] = (byte) 0xFF;
		raw[2] = (byte) 0xFF;
		raw[3] = (byte) 0xFF; // w = -1（全 F 才是负 i32）
		assertNull(ImeCandidateLayout.decode(raw));
	}

	// ── positionOverlay ──

	@Test
	void prefersBelowAnchorWhenFits() {
		Rect r = ImeCandidateLayout.positionOverlay(50, 100, 20, 200, 60, 400, 300, 4);
		assertNotNull(r);
		assertEquals(100, r.y());   // 下方优先
		assertEquals(50, r.x());
		assertEquals(200, r.w());
		assertEquals(60, r.h());
	}

	@Test
	void flipsAboveWhenNoRoomBelow() {
		// 输入框贴屏幕底：下方放不下 → 翻上方 yAbove=150（窗高60 → 210 ≤ 296 ✓）
		Rect r = ImeCandidateLayout.positionOverlay(50, 280, 150, 200, 60, 400, 300, 4);
		assertNotNull(r);
		assertEquals(150, r.y());
	}

	@Test
	void clampsRightEdgeOverflow() {
		Rect r = ImeCandidateLayout.positionOverlay(350, 100, 20, 200, 60, 400, 300, 4);
		assertNotNull(r);
		// 350+200 > 400-4 → x = 400-4-200 = 196
		assertEquals(196, r.x());
		assertTrue(r.x() + r.w() <= 400 - 4);
	}

	@Test
	void clampsLeftAndWidthWhenFrameWiderThanScreen() {
		Rect r = ImeCandidateLayout.positionOverlay(-30, 100, 20, 900, 60, 400, 300, 4);
		assertNotNull(r);
		assertEquals(4, r.x());
		assertEquals(400 - 8, r.w()); // 按可用区收缩，绝不越界
		assertTrue(r.x() + r.w() <= 400 - 4);
	}

	@Test
	void clampsTopWhenNeitherSideFits() {
		// 候选窗比屏幕还高（收缩后仍占满）→ 贴顶部安全边距
		Rect r = ImeCandidateLayout.positionOverlay(10, 250, -100, 200, 400, 400, 300, 4);
		assertNotNull(r);
		assertTrue(r.y() >= 4);
		assertTrue(r.y() + r.h() <= 300 - 4 + r.h()); // h 被钳到 maxH=292，贴边
		assertEquals(292, r.h());
	}

	@Test
	void rejectsNonPositiveSize() {
		assertNull(ImeCandidateLayout.positionOverlay(0, 0, 0, 0, 10, 400, 300, 4));
		assertNull(ImeCandidateLayout.positionOverlay(0, 0, 0, 10, -1, 400, 300, 4));
	}

	// ── decideShowing ──

	@Test
	void hidesWithoutFocusedBoxEvenWhenVisible() {
		Frame vis = new Frame(10, 10, 1, true, new byte[400]);
		assertFalse(ImeCandidateLayout.decideShowing(true, vis, false));
	}

	@Test
	void keepsCurrentStateWhenNoNewFrame() {
		assertTrue(ImeCandidateLayout.decideShowing(true, null, true));
		assertFalse(ImeCandidateLayout.decideShowing(false, null, true));
	}

	@Test
	void followsNativeVisibility() {
		Frame vis = new Frame(10, 10, 1, true, new byte[400]);
		Frame inv = new Frame(0, 0, 2, false, null);
		assertTrue(ImeCandidateLayout.decideShowing(false, vis, true));
		assertFalse(ImeCandidateLayout.decideShowing(true, inv, true));
	}

	// ── Rect.contains ──

	@Test
	void rectContainsBounds() {
		Rect r = new Rect(10, 20, 100, 40);
		assertTrue(r.contains(10, 20));
		assertTrue(r.contains(109.9, 59.9));
		assertFalse(r.contains(110, 20));
		assertFalse(r.contains(9.9, 20));
	}
}
