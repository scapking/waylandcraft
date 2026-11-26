package dev.evvie.waylandcraft.gui;

import java.util.ArrayList;
import java.util.List;

/**
 * 窗口管理屏（B 屏）焦点选择的纯逻辑（无 MC/bridge 依赖，可单测）。
 *
 * 缺陷根因（B 屏空白）：extractRenderState 无条件取 bridge.getMostRecentFocus()
 * 并直接渲染其表面树。该 API 在焦点序列表为空时会把**所有** toplevel 按创建顺序
 * 兜底种入（并非恒 null），因此本地直显模式下的真实空白路径有两条：
 *  1) bridge 尚无任何 toplevel（刚进服/事件未同步）→ null → 空白；
 *  2) 种入的"最近焦点"恰是未映射/零尺寸/无 framebuffer 的辅助窗口（如 QQ 启动器
 *     的隐藏 splash、X11 卫星的 0 尺寸壳）→ renderToplevel 非 null 但什么都画不出
 *     → 同样整屏空白，且键盘焦点被 focusSurface 抢给该废窗口。
 * 两种都表现为"按 B 什么都没"。修复：焦点不可用时不再直接画，走列表兜底——
 * 列出全部可渲染 toplevel 供选择（数字键/点击），选中即 focusSurface 进入既有
 * 表面树视图；无任何候选时给出明确指引文案。
 */
public final class WindowSelectionLogic {

	private WindowSelectionLogic() {}

	/** 一个 toplevel 在 B 屏视角下的候选描述（renderable=能画出内容）。 */
	public record Candidate(long handle, String title, int width, int height, boolean renderable) {}

	public enum Mode {
		/** 直接渲染选定 toplevel 的表面树（既有视图）。 */
		TREE,
		/** 渲染候选卡片列表，等待用户选择。 */
		LIST,
		/** 无任何候选：渲染指引文案（/wl launch 或 /wl capture）。 */
		GUIDE
	}

	/** mode=LIST/GUIDE 时 handle=0。 */
	public record Decision(Mode mode, long handle) {}

	/** 过滤出可渲染候选，保持 bridge 枚举顺序（创建顺序）。 */
	public static List<Candidate> usableCandidates(List<Candidate> all) {
		List<Candidate> out = new ArrayList<Candidate>(all.size());
		for(Candidate c : all) {
			if(c.renderable()) out.add(c);
		}
		return out;
	}

	/**
	 * 选择决策。优先级：
	 *  1. usable 为空 → GUIDE（给指引，绝不空白屏）。
	 *  2. forceListView（用户 TAB 手动唤出列表）→ LIST。
	 *  3. 上次手动选中仍可用 → TREE(manual)。
	 *  4. 焦点仍可用 → TREE(focused)（有 mod 服/已交互过的行为与旧实现一致）。
	 *  5. 焦点为空或不可用（根因 1/2 两条路径）→ LIST。
	 */
	public static Decision decide(boolean forceListView, long manualHandle, long focusedHandle, List<Candidate> usable) {
		if(usable.isEmpty()) return new Decision(Mode.GUIDE, 0);
		if(forceListView) return new Decision(Mode.LIST, 0);
		if(manualHandle != 0 && containsHandle(usable, manualHandle)) return new Decision(Mode.TREE, manualHandle);
		if(focusedHandle != 0 && containsHandle(usable, focusedHandle)) return new Decision(Mode.TREE, focusedHandle);
		return new Decision(Mode.LIST, 0);
	}

	private static boolean containsHandle(List<Candidate> usable, long handle) {
		for(Candidate c : usable) {
			if(c.handle() == handle) return true;
		}
		return false;
	}

	/** GLFW 数字键（GLFW_KEY_1=49..GLFW_KEY_9=57）→ 卡片下标；越界/超出卡片数返回 -1。 */
	public static int cardIndexForNumberKey(int glfwKey, int cardCount) {
		if(cardCount <= 0) return -1;
		int index = glfwKey - 49; // GLFW_KEY_1
		if(index < 0 || index >= 9 || index >= cardCount) return -1;
		return index;
	}

	/** 列表页脚提示（截断显示上限，超过部分提示用命令）。 */
	public static final int MAX_VISIBLE_CARDS = 9;

	public static List<Candidate> visibleCards(List<Candidate> usable) {
		if(usable.size() <= MAX_VISIBLE_CARDS) return usable;
		return usable.subList(0, MAX_VISIBLE_CARDS);
	}
}
