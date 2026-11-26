package dev.evvie.waylandcraft.ime;

import org.jetbrains.annotations.Nullable;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.ime.ImeCandidateLayout.BuiltinSnapshot;
import dev.evvie.waylandcraft.ime.ImeCandidateLayout.Rect;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.Font;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.gui.components.EditBox;

/**
 * 内置拼音引擎候选面板（结构化文本渲染，与 fcitx5 像素帧模式互斥——
 * 数据来源由 native 的 ime_source 裁决，两通道天然不会同时产出）。
 *
 * 双形态显示（复用既有锚定/命中/翻页语义）：
 * - <b>GUI 形态</b>：{@code mc.screen != null} 且聚焦 EditBox → 锚输入框正下方
 *   （{@link ImeCandidateLayout#positionOverlay} 同款边界钳制），点击/滚轮走
 *   {@code imeBuiltinSelect/imeBuiltinPage}；
 * - <b>HUD 形态</b>：键盘捕获下 {@code mc.screen == null}（往游戏内应用窗口
 *   打字）→ 屏幕底部居中的原版字体文本面板（HudElementRegistry 通道，
 *   WaylandHudRenderer 注册）。点选以缩放鼠标坐标命中（CAPTURE 自由鼠标
 *   场景）；HARD_CAPTURE 锁鼠标时键盘选词（数字/空格/翻页）始终可用。
 *
 * 线程：tick 在客户端 tick（ImeCandidateOverlay.tick 顺路驱动）；extract 在
 * 渲染线程（ScreenRenderMixin / HUD 层）；点击在 GLFW 回调（同线程）。
 *
 * 观测：showing 翻转 INFO（{@code [ime][builtin]}），解码失败 5s 节流 WARN。
 */
public final class ImeBuiltinPanel {

	static final int PAD = 3;
	static final int CELL_GAP = 4;
	static final int MARGIN = 6;
	static final int BACKING = 0xE0101014;
	static final int BORDER = 0xFF454550;
	static final int TEXT = 0xFFE8E8E8;
	static final int LABEL = 0xFF9AA0B0;
	static final int PREEDIT = 0xFFB8E0FF;
	static final int HIGHLIGHT_BG = 0xE02B4661;

	private static @Nullable BuiltinSnapshot snap;   // 最近一次有效快照
	private static boolean showing;                  // 面板显示态（含形态选择）
	private static @Nullable int[] cellLefts;        // 候选单元左边界（面板局部 x）
	private static @Nullable int[] cellWidths;       // 候选单元宽度
	private static @Nullable Rect panelRect;         // 最近绘制矩形（GUI 或 HUD，屏幕坐标）
	private static boolean hudMode;                  // 最近一次绘制形态（gui/hud）
	private static long parseFails;
	private static long lastWarnMs;
	private static long lastTupleMs;
	private static String lastTuple = "";

	private ImeBuiltinPanel() {}

	// ══════════════════════════════ 轮询（客户端 tick）══════════════════════════════

	/** {@link ImeCandidateOverlay#tick()} 顺路驱动（同一轮询节拍，互不干扰）。 */
	public static void tick() {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) return;
		String json;
		try {
			json = wlc.bridge.imeBuiltinCandidates();
		} catch(Throwable t) {
			return; // 旧 .so 缺符号：整通道静默缺席（fcitx5 像素路径不受影响）
		}
		if(json != null && !json.isEmpty()) {
			BuiltinSnapshot decoded = ImeCandidateLayout.decodeBuiltin(json);
			if(decoded == null) {
				parseFails++;
				warnThrottled("内置候选快照解码失败 len=" + json.length());
			} else {
				snap = decoded;
			}
		}
		Minecraft mc = Minecraft.getInstance();
		boolean contextAllows = mc != null && (mc.screen != null ? focusedEditBox(mc) != null : mc.level != null);
		boolean next = ImeCandidateLayout.decideShowingBuiltin(showing, snap, contextAllows);
		if(next != showing) {
			showing = next;
			if(!showing) {
				panelRect = null;
				cellLefts = null;
			}
			logTuple();
		}
	}

	// ══════════════════════════════ 渲染（两形态共用）══════════════════════════════

	/** GUI 形态（ScreenRenderMixin 通道，聚焦 EditBox 锚定）。 */
	public static void extractGui(GuiGraphicsExtractor ctx, EditBox box, Minecraft mc) {
		if(!showing || snap == null || box == null) return;
		int screenW = mc.screen == null ? ctx.guiWidth() : mc.screen.width;
		int screenH = mc.screen == null ? ctx.guiHeight() : mc.screen.height;
		int[] dims = measure(mc.font);
		int w = dims[0], h = dims[1];
		int anchorX = box.getScreenX(box.getCursorPosition());
		Rect r = ImeCandidateLayout.positionOverlay(anchorX,
				box.getY() + box.getHeight() + ImeCandidateOverlay.GAP,
				box.getY() - ImeCandidateOverlay.GAP - h,
				w, h, screenW, screenH, ImeCandidateOverlay.MARGIN);
		if(r == null) return;
		draw(ctx, mc.font, r, true);
	}

	/** HUD 形态（screen==null 的键盘捕获场景；WaylandHudRenderer 注册驱动）。 */
	public static void extractHud(GuiGraphicsExtractor ctx) {
		Minecraft mc = Minecraft.getInstance();
		if(!showing || snap == null || mc == null || mc.screen != null) return; // 仅无 screen 时接管
		Font font = mc.font;
		if(font == null) return;
		int[] dims = measure(font);
		int w = dims[0], h = dims[1];
		int sw = ctx.guiWidth(), sh = ctx.guiHeight();
		int x = Math.max(MARGIN, (sw - w) / 2);
		int y = sh - h - 2 * MARGIN; // 底部中央（准星/输入语境的自然落点）
		Rect r = new Rect(x, y, w, h);
		draw(ctx, font, r, false);
	}

	private static void draw(GuiGraphicsExtractor ctx, Font font, Rect r, boolean gui) {
		BuiltinSnapshot s = snap;
		if(s == null) return;
		panelRect = r;
		hudMode = !gui;
		int lineH = font.lineHeight;
		// 背景 + 边框（与像素帧 overlay 同款观感）
		ctx.fill(r.x() - 2, r.y() - 2, r.x() + r.w() + 2, r.y() + r.h() + 2, BACKING);
		ctx.fill(r.x() - 2, r.y() - 2, r.x() + r.w() + 2, r.y() - 1, BORDER);
		ctx.fill(r.x() - 2, r.y() + r.h() + 1, r.x() + r.w() + 2, r.y() + r.h() + 2, BORDER);
		ctx.fill(r.x() - 2, r.y() - 2, r.x() - 1, r.y() + r.h() + 2, BORDER);
		ctx.fill(r.x() + r.w() + 1, r.y() - 2, r.x() + r.w() + 2, r.y() + r.h() + 2, BORDER);

		int x = r.x() + PAD;
		// 行 1：拼音串（缓冲）
		String pre = "拼音:" + (s.preedit() == null ? "" : s.preedit());
		ctx.text(font, pre, x, r.y() + PAD, PREEDIT, false);
		// 行 2：候选单元（label+词），高亮画底色
		int rowY = r.y() + PAD + lineH + 1;
		int[] lefts = new int[s.candidates().length];
		int[] widths = new int[s.candidates().length];
		int cx = x;
		for(int i = 0; i < s.candidates().length; i++) {
			String cell = s.labels()[i] + s.candidates()[i];
			int cw = font.width(cell);
			lefts[i] = cx - x;
			widths[i] = cw;
			if(i == s.cursor()) {
				ctx.fill(cx - 1, rowY - 1, cx + cw + 1, rowY + lineH + 1, HIGHLIGHT_BG);
			}
			ctx.text(font, s.labels()[i], cx, rowY, LABEL, false);
			ctx.text(font, s.candidates()[i], cx + font.width(s.labels()[i]), rowY, TEXT, false);
			cx += cw + CELL_GAP;
		}
		// 行尾页码指示（pages>1 时，"«p/P»" 文本标记，滚轮/方向键翻页）
		if(s.pages() > 1) {
			String tag = (s.page() + 1) + "/" + s.pages();
			ctx.text(font, tag, Math.min(cx, r.x() + r.w() - font.width(tag) - PAD), rowY, LABEL, false);
		}
		cellLefts = lefts;
		cellWidths = widths;
	}

	/** 面板尺寸测量（渲染与 tick 判定共用）。[w, h] */
	private static int[] measure(Font font) {
		BuiltinSnapshot s = snap;
		int lineH = font.lineHeight;
		if(s == null) return new int[] {1, 1};
		int preW = font.width("拼音:" + (s.preedit() == null ? "" : s.preedit()));
		int rowW = 0;
		for(int i = 0; i < s.candidates().length; i++) {
			rowW += font.width(s.labels()[i] + s.candidates()[i]) + CELL_GAP;
		}
		if(s.pages() > 1) rowW += 24;
		return new int[] {Math.max(preW, rowW) + PAD * 2, lineH * 2 + PAD * 2 + 1};
	}

	// ══════════════════════════════ 输入回传 ══════════════════════════════

	/**
	 * 点击拦截（ImeCandidateOverlay.handleClick 同点位顺路调用，先于像素帧
	 * overlay 判定）。mx/my = GUI 缩放坐标（与面板绘制同系）。
	 * 返回 true = 已消费（候选单元或面板本体命中）。
	 */
	public static boolean handleClick(double mx, double my, boolean pressed) {
		Rect r = panelRect;
		BuiltinSnapshot s = snap;
		if(!showing || r == null || s == null) return false;
		if(!r.contains(mx, my)) return false;
		if(!pressed) return true; // 配对消费（press 已拦截时 release 不漏）
		int lineH = lineH();
		int rowTop = r.y() + PAD + lineH + 1;
		int rowBottom = rowTop + lineH + 2;
		int idx = -1;
		if(my >= rowTop - 1 && my <= rowBottom && cellLefts != null) {
			idx = ImeCandidateLayout.hitBuiltinCell(cellLefts, cellWidths, r.x() + r.w(), mx - r.x());
		}
		WaylandCraft wlc = WaylandCraft.instance;
		if(idx >= 0 && wlc != null && wlc.bridge != null) {
			String committed = wlc.bridge.imeBuiltinSelect(idx);
			WaylandCraftCommon.LOGGER.info("[ime][builtin] 点选 cell={} 提交=\"{}\"", idx, committed);
		}
		return true;
	}

	/** 滚轮拦截（面板区域翻页；GUI/HUD 两形态同判据）。 */
	public static boolean handleScroll(double mx, double my, double dy) {
		Rect r = panelRect;
		if(!showing || r == null) return false;
		// HUD 形态下面板不在鼠标语义的"组件"里——放宽为面板矩形命中即可。
		if(!r.contains(mx, my)) return false;
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc != null && wlc.bridge != null) {
			wlc.bridge.imeBuiltinPage(dy > 0 ? -1 : 1);
		}
		return true;
	}

	// ══════════════════════════════ 诊断出口 ══════════════════════════════

	public static boolean isShowing() {
		return showing;
	}

	/** /wl ime info 内置档行（纯读）。 */
	public static String describe() {
		BuiltinSnapshot s = snap;
		return "builtin showing=" + showing + (hudMode ? "(hud)" : "(gui)")
				+ (s == null ? " snap=null"
						: " preedit=" + s.preedit() + " cands=" + s.candidates().length
								+ " page=" + (s.page() + 1) + "/" + Math.max(1, s.pages())
								+ " cursor=" + s.cursor())
				+ " parseFails=" + parseFails;
	}

	private static int lineH() {
		Minecraft mc = Minecraft.getInstance();
		return mc != null && mc.font != null ? mc.font.lineHeight : 9;
	}

	private static @Nullable EditBox focusedEditBox(Minecraft mc) {
		if(mc == null || mc.screen == null) return null;
		var focused = mc.screen.getFocused();
		if(focused instanceof EditBox eb) return eb;
		for(var child : mc.screen.children()) {
			if(child instanceof EditBox eb && eb.isFocused()) return eb;
		}
		return null;
	}

	private static void warnThrottled(String what) {
		long now = System.currentTimeMillis();
		if(now - lastWarnMs < 5000) return;
		lastWarnMs = now;
		WaylandCraftCommon.LOGGER.warn("[ime][builtin] {}（累计 parseFails={}）", what, parseFails);
	}

	private static void logTuple() {
		String tuple = "showing=" + showing + " mode=" + (hudMode ? "hud" : "gui");
		if(tuple.equals(lastTuple)) return;
		long now = System.currentTimeMillis();
		if(now - lastTupleMs < 1000) return;
		lastTupleMs = now;
		lastTuple = tuple;
		BuiltinSnapshot s = snap;
		WaylandCraftCommon.LOGGER.info("[ime][builtin] 面板状态 {} preedit={} cands={} page={}",
				tuple,
				s == null ? "-" : s.preedit(),
				s == null ? 0 : s.candidates().length,
				s == null ? 0 : s.page() + 1);
	}
}
