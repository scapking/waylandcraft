package dev.evvie.waylandcraft.gui;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.gui.WindowSelectionLogic.Candidate;
import dev.evvie.waylandcraft.gui.WindowSelectionLogic.Decision;
import dev.evvie.waylandcraft.gui.WindowSelectionLogic.Mode;

/**
 * 缺陷 1（B 屏本地直显空白）的选择/排序/回退纯逻辑。
 * 空白有两条静默路径：getMostRecentFocus()==null（bridge 无窗口），以及焦点序列表
 * 被"创建顺序兜底种入"后指向未映射/零尺寸辅助窗口（非 null 但画不出内容）。
 * 修复语义：焦点可用 → 既有树视图；焦点 null/不可用 → 列表兜底；无候选 → 指引页。
 */
class WindowSelectionLogicTest {

	private static Candidate c(long handle, String title, int w, int h, boolean renderable) {
		return new Candidate(handle, title, w, h, renderable);
	}

	private static final List<Candidate> EMPTY = List.of();

	@Test
	@DisplayName("焦点可用 → TREE(focused)：有 mod 服/正常路径行为与旧实现一致")
	void usableFocusRendersTree() {
		List<Candidate> usable = List.of(c(0x1, "QQ", 1920, 1080, true));
		Decision d = WindowSelectionLogic.decide(false, 0, 0x1, usable);
		assertEquals(Mode.TREE, d.mode());
		assertEquals(0x1, d.handle());
	}

	@Test
	@DisplayName("焦点为 null（根因路径 1）→ 不再空白：列出候选为 LIST")
	void nullFocusFallsBackToList() {
		List<Candidate> usable = List.of(c(0x1, "QQ", 1920, 1080, true), c(0x2, "xterm", 800, 600, true));
		Decision d = WindowSelectionLogic.decide(false, 0, 0, usable);
		assertEquals(Mode.LIST, d.mode());
	}

	@Test
	@DisplayName("焦点指向不可渲染辅助窗口（根因路径 2：创建顺序兜底种入的 0 尺寸壳）→ LIST，绝不画空白")
	void unusableFocusFallsBackToList() {
		List<Candidate> usable = List.of(c(0x1, "QQ", 1920, 1080, true));
		Decision d = WindowSelectionLogic.decide(false, 0, 0x9, usable);
		assertEquals(Mode.LIST, d.mode(), "废窗口焦点必须被回退取代");
	}

	@Test
	@DisplayName("无任何候选 → GUIDE 指引页（先 launch/capture），空白屏彻底消失")
	void noCandidatesShowsGuide() {
		assertEquals(Mode.GUIDE, WindowSelectionLogic.decide(false, 0, 0x1, EMPTY).mode());
		assertEquals(Mode.GUIDE, WindowSelectionLogic.decide(true, 0, 0, EMPTY).mode(), "forceList 也救不了空列表：仍走 GUIDE");
	}

	@Test
	@DisplayName("手动选中优先于焦点（选中后切走焦点也能回到选中窗口）；选中窗口消失后自动重走回退")
	void manualSelectionBeatsFocus() {
		List<Candidate> usable = List.of(c(0x1, "QQ", 1920, 1080, true), c(0x2, "xterm", 800, 600, true));
		assertEquals(0x2, WindowSelectionLogic.decide(false, 0x2, 0x1, usable).handle());

		// 手动选中的窗口没了：回退到焦点
		List<Candidate> withoutManual = List.of(c(0x1, "QQ", 1920, 1080, true));
		Decision d = WindowSelectionLogic.decide(false, 0x2, 0x1, withoutManual);
		assertEquals(Mode.TREE, d.mode());
		assertEquals(0x1, d.handle());
	}

	@Test
	@DisplayName("TAB 强制列表页：焦点可用也进列表（用于切换窗口）")
	void forceListViewOverridesUsableFocus() {
		List<Candidate> usable = List.of(c(0x1, "QQ", 1920, 1080, true));
		assertEquals(Mode.LIST, WindowSelectionLogic.decide(true, 0, 0x1, usable).mode());
	}

	@Test
	@DisplayName("usableCandidates 只留可渲染项且保持枚举顺序；不可渲染项（未映射/无 framebuffer）不得进入列表")
	void usableFilterKeepsOrder() {
		List<Candidate> all = List.of(
			c(0x1, "QQ", 1920, 1080, true),
			c(0x2, "壳", 0, 0, false),
			c(0x3, "firefox", 1280, 720, true));
		List<Candidate> usable = WindowSelectionLogic.usableCandidates(all);
		assertEquals(List.of(0x1L, 0x3L), usable.stream().map(Candidate::handle).toList());
	}

	@Test
	@DisplayName("数字键 1-9 → 卡片下标：越界/0 键/超出卡片数一律 -1")
	void numberKeyMapping() {
		assertEquals(0, WindowSelectionLogic.cardIndexForNumberKey(49, 3)); // GLFW_KEY_1
		assertEquals(2, WindowSelectionLogic.cardIndexForNumberKey(51, 3)); // GLFW_KEY_3
		assertEquals(-1, WindowSelectionLogic.cardIndexForNumberKey(52, 3), "键 4 超出 3 张卡片");
		assertEquals(-1, WindowSelectionLogic.cardIndexForNumberKey(48, 3), "KEY_0 不是卡片键");
		assertEquals(-1, WindowSelectionLogic.cardIndexForNumberKey(65, 3), "字母键不参与");
		assertEquals(-1, WindowSelectionLogic.cardIndexForNumberKey(49, 0), "空列表无卡片");
	}

	@Test
	@DisplayName("卡片显示截断：超过 9 张只画前 9，其余提示用命令")
	void visibleCardsCapped() {
		List<Candidate> many = new java.util.ArrayList<>();
		for(long i = 1; i <= 14; i++) many.add(c(i, "w" + i, 100, 100, true));
		assertEquals(9, WindowSelectionLogic.visibleCards(many).size());
		assertTrue(WindowSelectionLogic.visibleCards(List.of()).isEmpty());
	}
}
