package dev.evvie.waylandcraft.ime;

/**
 * 候选窗 overlay 的纯函数层（零 Minecraft/GL 依赖，JUnit 直测——
 * 照 WindowLayout 的仓库测试风格拆分）。
 *
 * 职责：
 * - {@link #decode(byte[])}：JNI 帧协议解码（native {@code ime_popup_frame}
 *   的小端编码 [w i32][h i32][seq u32][visible u8][argb…]）；
 * - {@link #positionOverlay}：锚点候选位 → 边界钳制后的显示矩形
 *   （下方优先、放不下翻上方、整体钳进安全边距）；
 * - {@link #decideShowing}：显隐状态机（无新状态维持 / visible 翻转 /
 *   失焦即隐藏）。
 */
public final class ImeCandidateLayout {

	/** native 编码帧解码结果。argb 仅 visible=true 时非 null。 */
	public record Frame(int width, int height, int seq, boolean visible, byte[] argb) {}

	/** GUI 逻辑坐标矩形。 */
	public record Rect(int x, int y, int w, int h) {
		public boolean contains(double mx, double my) {
			return mx >= x && mx < x + w && my >= y && my < y + h;
		}
	}

	/** JNI 头长度：w+h+seq+visible = 4+4+4+1。 */
	static final int HEADER = 13;

	private ImeCandidateLayout() {}

	/** null/过短 → null（= 无新状态，overlay 维持现状）。 */
	public static Frame decode(byte[] raw) {
		if(raw == null || raw.length < HEADER) return null;
		int w = i32(raw, 0);
		int h = i32(raw, 4);
		int seq = i32(raw, 8);
		boolean visible = raw[12] != 0;
		if(w < 0 || h < 0) return null;
		byte[] argb = null;
		if(visible) {
			long need = (long) w * (long) h * 4L;
			if(need > Integer.MAX_VALUE - HEADER || raw.length < HEADER + (int) need) {
				// 声称可见但像素不足：native 编码错误防御，视为无效帧。
				return null;
			}
			argb = new byte[(int) need];
			System.arraycopy(raw, HEADER, argb, 0, (int) need);
		}
		return new Frame(w, h, seq, visible, argb);
	}

	static int i32(byte[] b, int at) {
		return (b[at] & 0xFF) | ((b[at + 1] & 0xFF) << 8)
				| ((b[at + 2] & 0xFF) << 16) | ((b[at + 3] & 0xFF) << 24);
	}

	/** 小端编码（与 native 侧一致，供测试构造样本帧）。 */
	public static byte[] encode(int w, int h, int seq, boolean visible, byte[] argb) {
		int px = argb == null ? 0 : argb.length;
		byte[] out = new byte[HEADER + px];
		out[0] = (byte) w; out[1] = (byte) (w >>> 8); out[2] = (byte) (w >>> 16); out[3] = (byte) (w >>> 24);
		out[4] = (byte) h; out[5] = (byte) (h >>> 8); out[6] = (byte) (h >>> 16); out[7] = (byte) (h >>> 24);
		out[8] = (byte) seq; out[9] = (byte) (seq >>> 8); out[10] = (byte) (seq >>> 16); out[11] = (byte) (seq >>> 24);
		out[12] = (byte) (visible ? 1 : 0);
		if(argb != null) System.arraycopy(argb, 0, out, HEADER, px);
		return out;
	}

	/**
	 * 边界钳制定位：
	 * 垂直：yBelow（输入框正下方）在安全区内 → 用；否则 yAbove（翻转挂框上方）
	 * 在安全区内 → 用；都不行 → 取 yBelow 钳进安全区（宁可贴边不越界）。
	 * 水平：anchorX 起排，越界向左回收，最左不早于 margin；过宽按可用区收缩。
	 */
	public static Rect positionOverlay(int anchorX, int yBelow, int yAbove, int w, int h,
			int screenW, int screenH, int margin) {
		if(w <= 0 || h <= 0) return null;
		int maxW = Math.max(1, screenW - 2 * margin);
		int maxH = Math.max(1, screenH - 2 * margin);
		int dw = Math.min(w, maxW);
		int dh = Math.min(h, maxH);

		int y;
		if(yBelow >= margin && yBelow + dh <= screenH - margin) {
			y = yBelow;
		} else if(yAbove >= margin && yAbove + dh <= screenH - margin) {
			y = yAbove;
		} else {
			y = Math.min(Math.max(margin, yBelow), Math.max(margin, screenH - dh - margin));
		}
		int x = anchorX;
		if(x + dw > screenW - margin) x = screenW - margin - dw;
		if(x < margin) x = margin;
		return new Rect(x, y, dw, dh);
	}

	/**
	 * 显隐状态机纯转移：
	 * - 无聚焦输入框 → 隐藏（候选窗是给"正在打字的框"配的语境件）；
	 * - 帧解码为 null（native 无新状态）→ 维持现状；
	 * - 否则跟 native visible（popup 销毁/attach(NULL)/会话结束都会给 false）。
	 */
	public static boolean decideShowing(boolean wasShowing, Frame f, boolean hasFocusedBox) {
		if(!hasFocusedBox) return false;
		if(f == null) return wasShowing;
		return f.visible();
	}

	/**
	 * 焦点恢复补显判据（decideShowing 的补充，不改变其合同）。
	 *
	 * 背景（sync8 复盘 bug）：因失焦抖动隐藏时，native 侧那份 visible 帧的
	 * epoch 已被交付消耗（delivered==epoch），恢复聚焦后的轮询只会拿到
	 * null——decideShowing(false, null, true) 恒 false，候选窗必须等到
	 * 下一次按键新帧才可能回来。本判据允许 overlay 用保留的 current 帧
	 * 立即补显：语义与 decideShowing "跟 native visible"一致（native 并未
	 * 宣告隐藏，只是不再重复投递），不违反既有状态机合同。
	 */
	public static boolean shouldReshowCurrent(boolean wasShowing, Frame f, boolean hasFocusedBox, Frame retained) {
		return !wasShowing && f == null && hasFocusedBox
				&& retained != null && retained.visible();
	}

	// ═══════════════ 世界形态（sync14）：窗口像素空间纯函数 ═══════════════
	//
	// 键盘捕获（CAPTURE/HARD_CAPTURE）下往游戏内应用窗口打字时 mc.screen==null，
	// GUI 形态（focusedEditBox 锚定）无处显示——世界形态把候选窗贴到目标窗口
	// quad 旁：所有坐标统一在**窗口平面局部像素空间**（origin=geometry 左上角，
	// x 右 y 下，1 单位 = 1 窗口像素；渲染/命中经 WindowDisplay.localX/Y 换算到
	// 世界单位）。纯函数零 MC 依赖，单测直打——与 GUI 形态的 positionOverlay/
	// decideShowing 完全平行，GUI 侧行为零改动。

	/**
	 * 世界候选窗定位：锚 ti3 光标矩形（下方优先，放不下翻上方），
	 * 无矩形（null 或 0x0 退化）回退锚窗口 quad 底部中央。
	 *
	 * 与 GUI 的 positionOverlay 不同点：世界空间没有屏幕边界，
	 * 候选窗越出窗口范围只是浮进世界（仍可见可点选），故不做整体钳制，
	 * 只在水平方向把起点收回窗口内（x ∈ [0, winW-popW]）保证贴窗观感。
	 *
	 * @param cursorRect ti3 光标矩形（平面局部像素），可 null
	 * @param winW       窗口 geometry 宽（像素）
	 * @param winH       窗口 geometry 高（像素）
	 * @param popW/popH  候选帧像素尺寸
	 * @param gap        光标矩形与候选窗的间距（像素）
	 * @return 候选窗矩形（平面局部像素，w=popW h=popH 与帧 1:1）；参数无效 → null
	 */
	public static Rect positionWorldPopup(Rect cursorRect, int winW, int winH, int popW, int popH, int gap) {
		if(popW <= 0 || popH <= 0 || winW <= 0 || winH <= 0) return null;
		int x = 0;
		int y;
		if(cursorRect == null || (cursorRect.w() <= 0 && cursorRect.h() <= 0)) {
			// 回退锚：窗口底部中央（应用没发 set_cursor_rectangle 时）。
			x = (winW - popW) / 2;
			y = winH + gap;
		} else {
			int ax = Math.max(0, Math.min(cursorRect.x(), Math.max(0, winW - 1)));
			int ay = Math.max(0, Math.min(cursorRect.y(), Math.max(0, winH - 1)));
			int below = ay + Math.max(cursorRect.h(), 1) + gap;
			// 下方优先；越出窗口底边翻到光标矩形上方。
			y = below + popH <= winH ? below : ay - gap - popH;
			x = Math.max(0, Math.min(ax, Math.max(0, winW - popW)));
		}
		return new Rect(x, y, popW, popH);
	}

	/**
	 * surface 局部像素 → 窗口平面局部像素。
	 * 逆 {@code WindowDisplay.intersect} 的正变换
	 * （localCoords = geometryLocal + (geometry.x, geometry.y)，再减 surface.xSubpos/ySubpos）。
	 * HARD_CAPTURE 的虚拟光标（surface 局部）用它换算到候选矩形同一空间。
	 */
	public static double[] surfaceLocalToPlaneLocal(double sx, double sy,
			double xSubpos, double ySubpos, int geometryX, int geometryY) {
		return new double[] { sx + xSubpos - geometryX, sy + ySubpos - geometryY };
	}

	/**
	 * 平面局部像素命中点 → popup 表面局部像素（ime_popup_click/axis 的回传坐标）。
	 * 世界形态显示矩形与帧像素 1:1（不像 GUI 可能被钳压缩），映射即平移 +
	 * 帧内防御钳制；命中点不在矩形内 → null。
	 */
	public static int[] worldHitToPopupLocal(Rect popup, double planeX, double planeY) {
		if(popup == null || !popup.contains(planeX, planeY)) return null;
		int lx = (int) Math.floor(planeX - popup.x());
		int ly = (int) Math.floor(planeY - popup.y());
		if(lx < 0) lx = 0; else if(lx >= popup.w()) lx = popup.w() - 1;
		if(ly < 0) ly = 0; else if(ly >= popup.h()) ly = popup.h() - 1;
		return new int[] {lx, ly};
	}

	/**
	 * 世界形态显隐（decideShowing 的语境参数化，不改变其合同）：
	 * GUI 的「聚焦输入框在场」↔ 世界的「目标窗口在场且聚焦」。
	 * f.visible() 在 native 侧已含「IME 会话激活」门（take_popup_snapshot），
	 * 窗口销毁/失焦（windowInPlace=false）即时隐藏。
	 */
	public static boolean decideShowingWorld(boolean wasShowing, Frame f, boolean windowInPlace) {
		return decideShowing(wasShowing, f, windowInPlace);
	}

	/** 世界形态焦点恢复补显（shouldReshowCurrent 的同款语境映射）。 */
	public static boolean shouldReshowCurrentWorld(boolean wasShowing, Frame f, boolean windowInPlace, Frame retained) {
		return shouldReshowCurrent(wasShowing, f, windowInPlace, retained);
	}

	// ═══════════════ 内置候选（结构化文本通道）：纯函数层 ═══════════════

	/**
	 * 内置引擎候选快照（JSON 合同 = Rust {@code BuiltinEngine::snapshot_json}，
	 * 仅状态变化时投递）。candidates/labels 为**当前页**内容。
	 */
	public record BuiltinSnapshot(boolean visible, String preedit, int cursor, int page, int pages,
			int pageSize, String[] candidates, String[] labels) {}

	/**
	 * 解码内置候选快照 JSON。null/空串 = 无新状态 → null；非法 JSON 防御式
	 * 返回 null（调用方维持现状并计一次 parseFail，绝不炸游戏）。
	 */
	public static BuiltinSnapshot decodeBuiltin(String json) {
		if(json == null || json.isEmpty()) return null;
		try {
			com.google.gson.JsonObject o = com.google.gson.JsonParser.parseString(json).getAsJsonObject();
			boolean visible = o.get("visible").getAsBoolean();
			String preedit = o.get("preedit").getAsString();
			int cursor = o.get("cursor").getAsInt();
			int page = o.get("page").getAsInt();
			int pages = o.get("pages").getAsInt();
			int pageSize = o.get("page_size").getAsInt();
			String[] cands = strArray(o, "candidates");
			String[] labels = strArray(o, "labels");
			if(pageSize <= 0 || labels.length != cands.length) return null;
			return new BuiltinSnapshot(visible, preedit, cursor, page, pages, pageSize, cands, labels);
		} catch(RuntimeException e) {
			return null;
		}
	}

	private static String[] strArray(com.google.gson.JsonObject o, String key) {
		var arr = o.getAsJsonArray(key);
		String[] out = new String[arr.size()];
		for(int i = 0; i < arr.size(); i++) out[i] = arr.get(i).getAsString();
		return out;
	}

	/**
	 * 候选单元水平排布（给定各单元文本宽度）：返回每个单元的左边界（绝对 x）。
	 * 纯函数——渲染与命中共用同一份边界数组，杜绝绘制/判定两套算术漂移。
	 */
	public static int[] builtinCellLefts(int x0, int gap, int[] cellWidths) {
		int[] lefts = new int[cellWidths.length];
		int x = x0;
		for(int i = 0; i < cellWidths.length; i++) {
			lefts[i] = x;
			x += cellWidths[i] + gap;
		}
		return lefts;
	}

	/** 命中检测：mx 落在哪个候选单元（cellLefts 升序 + 末单元右界）。-1=未命中。 */
	public static int hitBuiltinCell(int[] cellLefts, int[] cellWidths, int rightLimit, double mx) {
		for(int i = 0; i < cellLefts.length; i++) {
			int end = i + 1 < cellLefts.length ? cellLefts[i + 1] : rightLimit;
			if(mx >= cellLefts[i] && mx < end) return i;
		}
		return -1;
	}

	/**
	 * 内置候选显隐状态机（decideShowing 的文本通道同族，独立不干扰像素路径）：
	 * 无快照 → 维持；有快照 → 跟 visible；无聚焦框/无会话语境由调用方决定。
	 */
	public static boolean decideShowingBuiltin(boolean wasShowing, BuiltinSnapshot snap, boolean contextAllows) {
		if(!contextAllows) return false;
		if(snap == null) return wasShowing;
		return snap.visible();
	}
}
