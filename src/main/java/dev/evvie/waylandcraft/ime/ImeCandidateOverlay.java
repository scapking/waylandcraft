package dev.evvie.waylandcraft.ime;

import org.lwjgl.system.MemoryUtil;

import com.mojang.blaze3d.platform.NativeImage;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.ime.ImeCandidateLayout.Frame;
import dev.evvie.waylandcraft.ime.ImeCandidateLayout.Rect;
import dev.evvie.waylandcraft.render.RenderUtils;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.gui.components.EditBox;
import net.minecraft.client.gui.components.events.GuiEventListener;
import net.minecraft.client.renderer.texture.DynamicTexture;
import net.minecraft.client.renderer.texture.TextureManager;
import net.minecraft.resources.Identifier;

/**
 * 游戏内输入法候选窗 overlay（Phase 2）。
 *
 * 数据流：嵌套 fcitx5 把候选窗画进 {@code zwp_input_popup_surface_v2}
 * （waylandinputwindow.cpp 取证），native 侧合成器采集其 shm 缓冲成
 * ARGB8888 帧（{@code ime/input_popup.rs}），Java 每 tick 经
 * {@code bridge.imePopupFrame()} 轮询——null=无新状态（零拷贝）；
 * visible=0→隐藏；visible=1→上传纹理并绘制在聚焦输入框（聊天框）光标
 * 下方，带背景衬底与边界钳制；popup 消失即隐藏。
 *
 * 锚点策略（规格"ti3 cursor rect 对应位置，拿不到就聊天输入框正下方"）：
 * 取聚焦 EditBox 的光标屏幕位置为候选窗左上角候选位（下方间距 GAP），
 * 屏幕放不下翻到框上方，再整体钳进安全边距——纯函数部分在
 * {@link ImeCandidateLayout}（单测直打）。
 *
 * 线程模型：tick() 在客户端 tick（=渲染线程）调用；DynamicTexture 上传
 * 与 extract 同线程，绝不跨线程碰 GL（同 RemoteWindowRenderer 纪律）。
 */
public final class ImeCandidateOverlay {

	private static final Identifier TEXTURE =
			Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "dynamic/ime-candidate");
	/** 候选窗与输入框的间距 / 屏幕安全边距（GUI px）。 */
	static final int GAP = 2;
	static final int MARGIN = 4;
	/** 背景衬底（半透深黑 ARGB）与 1px 边框。 */
	static final int BACKING = 0xCC101014;
	static final int BORDER = 0xFF454550;

	private static Frame current;   // 当前展示的帧（含像素）
	private static boolean showing;
	private static Rect lastRect;   // 最近一次绘制的矩形（Phase 3 命中检测，GUI 坐标）
	private static DynamicTexture texture;
	private static int texW, texH;

	private ImeCandidateOverlay() {}

	// ══════════════════════════ 帧轮询与显隐状态机 ══════════════════════════

	/** 每客户端 tick 调用（MinecraftMixin.runTick HEAD，与 CursorRectReporter 同点）。 */
	public static void tick() {
		Minecraft mc = Minecraft.getInstance();
		if(mc == null) return;

		EditBox box = focusedEditBox(mc);
		byte[] raw = null;
		if(WaylandCraft.instance != null && WaylandCraft.instance.bridge != null) {
			try {
				raw = WaylandCraft.instance.bridge.imePopupFrame();
			} catch(UnsatisfiedLinkError | RuntimeException e) {
				// 旧 .so 缺符号/bridge 瞬时异常：降级为不显示，绝不炸游戏
				// （仓库惯例：bridge 调用包 try/catch，见 WaylandCraft.update）。
				applyTransition(null, box != null, mc);
				return;
			}
		}
		applyTransition(ImeCandidateLayout.decode(raw), box != null, mc);
	}

	/** 状态转移 + 纹理更新（客户端 tick 线程）。 */
	private static void applyTransition(Frame f, boolean hasBox, Minecraft mc) {
		boolean next = ImeCandidateLayout.decideShowing(showing, f, hasBox);
		if(!next) {
			if(f != null && !f.visible()) current = null;
			showing = false;
			lastRect = null;
			return;
		}
		if(f == null) {
			// 无新状态：维持（首帧前 current==null 时 extract 自然跳过）。
			showing = true;
			return;
		}
		current = f;
		showing = true;
		uploadFrame(f);
	}

	// ═══════════════════════════════ 绘制 ═══════════════════════════════

	/** Screen render hook（ScreenRenderMixin → extractRenderStateWithTooltipAndSubtitles RETURN）。 */
	public static void extract(GuiGraphicsExtractor ctx) {
		Frame f = current;
		if(!showing || f == null || !f.visible() || f.argb() == null || texture == null) return;
		Minecraft mc = Minecraft.getInstance();
		EditBox box = focusedEditBox(mc);
		if(box == null) return; // 本帧焦点已丢，等 tick 收敛
		int screenW = mc.screen == null ? ctx.guiWidth() : mc.screen.width;
		int screenH = mc.screen == null ? ctx.guiHeight() : mc.screen.height;

		// 锚点：光标屏幕 x；下方 = 框底 + 间距；翻转上方 = 框顶 - 间距 - 窗高。
		int anchorX = clampInt(box.getScreenX(box.getCursorPosition()), 0, Math.max(0, screenW - 1));
		int yBelow = box.getY() + box.getHeight() + GAP;
		int yAbove = box.getY() - GAP - f.height();
		Rect r = ImeCandidateLayout.positionOverlay(anchorX, yBelow, yAbove, f.width(), f.height(),
				screenW, screenH, MARGIN);
		if(r == null) return;
		lastRect = r;

		// 背景衬底 + 1px 边框——候选窗自身可能半透明/圆角（cairo ARGB）。
		ctx.fill(r.x() - 2, r.y() - 2, r.x() + r.w() + 2, r.y() + r.h() + 2, BACKING);
		ctx.fill(r.x() - 2, r.y() - 2, r.x() + r.w() + 2, r.y() - 1, BORDER);
		ctx.fill(r.x() - 2, r.y() + r.h() + 1, r.x() + r.w() + 2, r.y() + r.h() + 2, BORDER);
		ctx.fill(r.x() - 2, r.y() - 2, r.x() - 1, r.y() + r.h() + 2, BORDER);
		ctx.fill(r.x() + r.w() + 1, r.y() - 2, r.x() + r.w() + 2, r.y() + r.h() + 2, BORDER);

		// flipV=false：shm 行序（左上原点）与 NativeImage 一致，同
		// renderFramebuffer2D 约定。
		RenderUtils.renderTexture2D(ctx, TEXTURE, r.x(), r.y(), r.w(), r.h(), false);
	}

	/** 隐藏并清命中矩形（纹理保留复用——下次显示直接 upload）。 */
	public static void hide() {
		showing = false;
		lastRect = null;
	}

	/** 命中矩形（Phase 3 拦截判定；null=不可交互）。 */
	public static Rect hitRect() {
		return showing ? lastRect : null;
	}

	// ══════════════════════════════ 内部工具 ══════════════════════════════

	/** 聚焦的 EditBox（聊天输入框等）。优先 MC 自身焦点跟踪，遍历兜底
	 * （与 CursorRectReporter 同款策略，防多框场景取错框）。 */
	private static EditBox focusedEditBox(Minecraft mc) {
		if(mc == null || mc.screen == null) return null;
		GuiEventListener focused = mc.screen.getFocused();
		if(focused instanceof EditBox eb) return eb;
		for(GuiEventListener child : mc.screen.children()) {
			if(child instanceof EditBox eb && eb.isFocused()) return eb;
		}
		return null;
	}

	/** ARGB8888 小端字节流 → NativeImage（RGBA 内存序 [R,G,B,A]）原地写入 + upload。 */
	private static void uploadFrame(Frame f) {
		try {
			if(f.width() <= 0 || f.height() <= 0) return;
			if(texture == null || texW != f.width() || texH != f.height()) {
				recreateTexture(f.width(), f.height());
			}
			NativeImage img = texture.getPixels();
			if(img == null || img.isClosed()) {
				recreateTexture(f.width(), f.height());
				img = texture.getPixels();
				if(img == null) return;
			}
			long ptr = img.getPointer();
			if(ptr == 0L) return;
			byte[] d = f.argb();
			int n = f.width() * f.height();
			// wire 像素字节 [B,G,R,A]（= u32 0xAARRGGBB 小端）；MC 26.x
			// NativeImage RGBA 内存 [R,G,B,A]（RemoteWindowRenderer 取证），
			// memPutInt 小端落盘 → 组装 (A<<24)|(B<<16)|(G<<8)|R。
			for(int i = 0; i < n; i++) {
				int b = i * 4;
				int b0 = d[b] & 0xFF;
				int b1 = d[b + 1] & 0xFF;
				int b2 = d[b + 2] & 0xFF;
				int b3 = d[b + 3] & 0xFF;
				MemoryUtil.memPutInt(ptr + i * 4L, (b3 << 24) | (b0 << 16) | (b1 << 8) | b2);
			}
			texture.upload();
		} catch(RuntimeException | UnsatisfiedLinkError e) {
			WaylandCraftCommon.LOGGER.warn("[ime][overlay] 候选窗纹理上传失败: {}", e.toString());
		}
	}

	private static void recreateTexture(int w, int h) {
		TextureManager tm = Minecraft.getInstance().getTextureManager();
		if(texture != null) {
			tm.release(TEXTURE);
			texture = null;
		}
		NativeImage image = new NativeImage(w, h, false);
		texture = new DynamicTexture(() -> "ime_candidate_" + w + "x" + h, image);
		tm.register(TEXTURE, texture);
		texW = w;
		texH = h;
	}

	private static int clampInt(int v, int lo, int hi) {
		return v < lo ? lo : Math.min(v, hi);
	}

	/** 诊断（日志/status）：当前是否展示候选窗。 */
	public static boolean isShowing() {
		return showing;
	}
}
