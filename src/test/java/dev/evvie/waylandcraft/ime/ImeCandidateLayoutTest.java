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

	// ── 内置候选（结构化文本通道）纯函数 ──

	@Test
	void builtinDecodeGoldenMatchesNativeContract() {
		// 与 Rust BuiltinEngine::snapshot_json 的合同样例（golden，双侧锁形）。
		String json = "{\"visible\":true,\"preedit\":\"nihao\",\"cursor\":0,\"page\":0,\"pages\":2,"
				+ "\"page_size\":9,\"candidates\":[\"你好\",\"你好吗\"],\"labels\":[\"1\",\"2\"]}";
		var snap = ImeCandidateLayout.decodeBuiltin(json);
		assertNotNull(snap);
		assertTrue(snap.visible());
		assertEquals("nihao", snap.preedit());
		assertEquals(0, snap.cursor());
		assertEquals(2, snap.pages());
		assertEquals(9, snap.pageSize());
		assertArrayEquals(new String[] {"你好", "你好吗"}, snap.candidates());
		assertArrayEquals(new String[] {"1", "2"}, snap.labels());
	}

	@Test
	void builtinDecodeRejectsGarbage() {
		assertNull(ImeCandidateLayout.decodeBuiltin(null));
		assertNull(ImeCandidateLayout.decodeBuiltin(""));
		assertNull(ImeCandidateLayout.decodeBuiltin("{oops"));
		assertNull(ImeCandidateLayout.decodeBuiltin("{\"visible\":true}")); // 缺字段
		// labels/candidates 长度不符 = 合同破坏 → null
		assertNull(ImeCandidateLayout.decodeBuiltin(
				"{\"visible\":true,\"preedit\":\"a\",\"cursor\":0,\"page\":0,\"pages\":1,"
						+ "\"page_size\":9,\"candidates\":[\"啊\"],\"labels\":[]}"));
	}

	@Test
	void builtinCellsAndHitMap() {
		// 三单元宽 [10, 14, 8]，x0=5, gap=4 → 左边界 [5,19,37]，右限=45+… 定右限 60。
		int[] lefts = ImeCandidateLayout.builtinCellLefts(5, 4, new int[] {10, 14, 8});
		assertArrayEquals(new int[] {5, 19, 37}, lefts);
		assertEquals(0, ImeCandidateLayout.hitBuiltinCell(lefts, new int[] {10, 14, 8}, 60, 9.5));
		assertEquals(1, ImeCandidateLayout.hitBuiltinCell(lefts, new int[] {10, 14, 8}, 60, 20));
		assertEquals(2, ImeCandidateLayout.hitBuiltinCell(lefts, new int[] {10, 14, 8}, 60, 44));
		// gap 区按"下一格起点前"归当前格（末格含右限）
		assertEquals(-1, ImeCandidateLayout.hitBuiltinCell(lefts, new int[] {10, 14, 8}, 60, 70));
		assertEquals(-1, ImeCandidateLayout.hitBuiltinCell(lefts, new int[] {10, 14, 8}, 60, 2));
	}

	@Test
	void builtinShowingStateMachine() {
		var vis = ImeCandidateLayout.decodeBuiltin(
				"{\"visible\":true,\"preedit\":\"ni\",\"cursor\":0,\"page\":0,\"pages\":1,"
						+ "\"page_size\":9,\"candidates\":[\"你\"],\"labels\":[\"1\"]}");
		var hid = ImeCandidateLayout.decodeBuiltin(
				"{\"visible\":false,\"preedit\":\"\",\"cursor\":0,\"page\":0,\"pages\":0,"
						+ "\"page_size\":9,\"candidates\":[],\"labels\":[]}");
		// 无快照 → 维持；有快照 → 跟 visible；语境不允许（无框/无场景）→ 强制隐藏
		assertTrue(ImeCandidateLayout.decideShowingBuiltin(false, vis, true));
		assertTrue(ImeCandidateLayout.decideShowingBuiltin(true, null, true));
		assertFalse(ImeCandidateLayout.decideShowingBuiltin(true, hid, true));
		assertFalse(ImeCandidateLayout.decideShowingBuiltin(true, vis, false));
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

	// ── 跨语言编码合同：golden hexdump ──────────────────────────────
	//
	// 下面这段字节流是 native 侧 tests_popup::
	// popup_java_polling_sequence_matches_wire_encoding_contract 实测一次
	// 4x3 可见帧（argb_pattern seed=0）的 JNI byte[] 逐字节固化。两侧断言
	// 同一串：native 编码改了 → Rust 测试红；Java 解码改了 → 这里红。
	// 这是"Java 半程静默不显示"故障的回归锚点——Java 侧 decode/decideShowing
	// 必须与真实字节流逐字节对齐，而不是只测自造的 encode。
	private static final String GOLDEN_NATIVE_FRAME_HEX =
			"04000000030000000100000001112244ff102345ff132046ff122147ff"
			+ "013254ff003355ff033056ff023157ff310264ff300365ff330066ff320167ff";

	@Test
	void nativeGoldenFrameDecodesExactly() {
		byte[] raw = hexToBytes(GOLDEN_NATIVE_FRAME_HEX);
		assertEquals(13 + 4 * 3 * 4, raw.length, "golden 长度 = 13 + w*h*4");
		Frame f = ImeCandidateLayout.decode(raw);
		assertNotNull(f, "真实 native 字节流必须能解码");
		assertEquals(4, f.width());
		assertEquals(3, f.height());
		assertEquals(1, f.seq());
		assertTrue(f.visible());
		assertEquals(48, f.argb().length);
		// 首像素 [B,G,R,A] = [11,22,44,ff]（与 argb_pattern(x=0,y=0,seed=0) 一致）。
		assertArrayEquals(new byte[] {0x11, 0x22, 0x44, (byte) 0xff},
				java.util.Arrays.copyOf(f.argb(), 4));
		// 尾像素 [32,01,67,ff]（x=3,y=2）。
		assertArrayEquals(new byte[] {0x32, 0x01, 0x67, (byte) 0xff},
				java.util.Arrays.copyOfRange(f.argb(), 44, 48));
		// decideShowing：可见 golden + 聚焦框 → 显示。
		assertTrue(ImeCandidateLayout.decideShowing(false, f, true),
				"真实帧 + hasBox 必须判定显示——若这里 false 即根因在 Java 判定");
	}

	@Test
	void nativeGoldenInvisibleDeliveryDecodes() {
		// 角色登记/隐藏交付：13 字节头，visible=0（无像素）。
		byte[] raw = hexToBytes("040000000300000007000000" + "00");
		Frame f = ImeCandidateLayout.decode(raw);
		assertNotNull(f);
		assertFalse(f.visible());
		assertNull(f.argb());
		// 可见性翻 false → decideShowing 必须隐藏。
		assertFalse(ImeCandidateLayout.decideShowing(true, f, true));
	}

	private static byte[] hexToBytes(String hex) {
		int n = hex.length() / 2;
		byte[] out = new byte[n];
		for(int i = 0; i < n; i++) {
			out[i] = (byte) Integer.parseInt(hex.substring(i * 2, i * 2 + 2), 16);
		}
		return out;
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

	// ── shouldReshowCurrent（焦点恢复补显，sync8 复盘 bug 的回归锚点）──

	@Test
	void reshowCurrentAfterFocusBlipWhenNativeStillVisible() {
		Frame vis = new Frame(10, 10, 1, true, new byte[400]);
		// 失焦隐藏 → 恢复聚焦后 native 不再重投（epoch 已消耗），轮询恒 null。
		// 此时必须用保留的 visible 帧补显，否则候选窗"消失后直到下次按键才回来"。
		assertTrue(ImeCandidateLayout.shouldReshowCurrent(false, null, true, vis));
	}

	@Test
	void noReshowWhenInvisibleNoBoxOrFreshFrame() {
		Frame vis = new Frame(10, 10, 1, true, new byte[400]);
		Frame inv = new Frame(0, 0, 2, false, null);
		assertFalse(ImeCandidateLayout.shouldReshowCurrent(true, null, true, vis));  // 本已显示
		assertFalse(ImeCandidateLayout.shouldReshowCurrent(false, null, false, vis)); // 仍无聚焦框
		assertFalse(ImeCandidateLayout.shouldReshowCurrent(false, inv, true, vis));   // 有新帧走 decideShowing
		assertFalse(ImeCandidateLayout.shouldReshowCurrent(false, null, true, inv));  // retained 不可见
		assertFalse(ImeCandidateLayout.shouldReshowCurrent(false, null, true, null)); // retained 为空
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
