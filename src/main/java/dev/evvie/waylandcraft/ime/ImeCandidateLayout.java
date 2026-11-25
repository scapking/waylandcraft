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
}
