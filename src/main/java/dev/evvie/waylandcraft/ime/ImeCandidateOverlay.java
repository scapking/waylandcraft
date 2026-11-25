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
 *
 * 观测层（sync8 静默故障复盘：候选窗不显示但 latest.log 零痕迹）：
 * 本类任何降级/异常/状态翻转都必须留日志——排查口诀：
 * - grep {@code [ime][overlay]}：五元组 (frame,visible,hasBox,showing,texture)
 *   翻转一行 + 累计计数；
 * - 无"extract 首次命中"行 = ScreenRenderMixin 注入未生效（目标方法改名）；
 * - exceptions>0 且 frame 恒 false = JNI 轮询断链（旧 .so/大数组）；
 * - uploadsFails>0 且 texture=false = GL 纹理上传断链；
 * - native 半程对照：{@code /wl status} 的 ime_candidate_popup 行
 *   （frames/deliveries/epoch/delivered/五道可见性门）。
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
	/** 引擎工作态角标（状态透明度）："拼"字 + 深蓝衬底，无任何资源文件。 */
	static final int BADGE_BG = 0xE02B4661;
	static final int BADGE_FG = 0xFFEAF2FF;

	private static Frame current;   // 当前展示的帧（含像素）
	private static boolean showing;
	private static Rect lastRect;   // 最近一次绘制的矩形（Phase 3 命中检测，GUI 坐标）
	private static DynamicTexture texture;
	private static int texW, texH;
	private static boolean workState; // native「引擎工作态」（角标原料，纯读）

	// ═══════════════════════════ 观测层计数 ═══════════════════════════

	private static long polls;            // tick 轮询总次数
	private static long framesDelivered;  // native 交付过非 null byte[] 的次数
	private static long pollExceptions;   // imePopupFrame 抛异常次数（曾完全静默）
	private static long uploadFails;      // 纹理上传失败次数
	private static long reshowCount;      // 焦点恢复补显次数
	private static long incomingVisible;  // 解码后收到 f.visible()=true 的次数（入站可见帧，sync12 复盘盲区）
	private static long extractHits;      // ScreenRenderMixin 回调命中次数（注入证明）
	private static long lastWarnMs;       // WARN 节流 5s
	private static long lastTupleMs;      // 五元组 INFO 节流 1s
	private static long lastHealMs;       // 自愈重传节流 1s
	private static String lastLoggedTuple = "";
	private static boolean extractHitLogged;

	private ImeCandidateOverlay() {}

	// ══════════════════════════ 帧轮询与显隐状态机 ══════════════════════════

	/** 每客户端 tick 调用（MinecraftMixin.runTick HEAD，与 CursorRectReporter 同点）。 */
	public static void tick() {
		Minecraft mc = Minecraft.getInstance();
		if(mc == null) return;

		EditBox box = focusedEditBox(mc);
		byte[] raw = null;
		boolean pollFailed = false;
		if(WaylandCraft.instance != null && WaylandCraft.instance.bridge != null) {
			try {
				raw = WaylandCraft.instance.bridge.imePopupFrame();
			} catch(UnsatisfiedLinkError | RuntimeException e) {
				// 旧 .so 缺符号/bridge 瞬时异常：降级为不显示，绝不炸游戏
				// （仓库惯例：bridge 调用包 try/catch，见 WaylandCraft.update）。
				// 观测层：sync8 复盘确认"catch 静默"正是故障无法定位的形态——
				// 现在 5s 节流 WARN + 计数；若每帧都抛，latest.log 可见
				// exceptions 持续增长且五元组 frame 恒 false。
				pollFailed = true;
				pollExceptions++;
				warnThrottled("候选窗轮询异常（降级为不显示）", e);
			}
			if(!pollFailed) {
				try {
					// 引擎工作态（角标原料）：纯读、零拷贝；旧 .so 缺符号 → 无角标。
					workState = WaylandCraft.instance.bridge.imePopupWorkState();
				} catch(UnsatisfiedLinkError | RuntimeException e) {
					workState = false;
				}
			}
		}
		polls++;
		if(raw != null) framesDelivered++;
		Frame f = pollFailed ? null : ImeCandidateLayout.decode(raw);
		if(raw != null && f == null) {
			// 收到字节流但解码拒绝 = 跨语言编码合同断了（两侧测试理应已红；
			// 真到这里说明 jar/so 版本错配）——必须留痕。
			warnThrottled("native 字节流解码失败 len=" + raw.length, null);
		}
		// 观测网（sync12 复盘盲区）：旧五元组的 visible 读的是 current（显示后
		// 结果），无法证明"是否收到过可见帧"。incomingVisible 直接数解码后
		// f.visible()==true 的次数——>0 而 showing 恒 false = 断在 hasBox/显示
		// 判定；==0 而 native gate 快照显示 visible=true = 断在编码/JNI 传输。
		if(f != null && f.visible()) {
			incomingVisible++;
		}
		applyTransition(f, box != null, mc);
		logStateTransition(raw != null, box != null);
	}

	/** 状态转移 + 纹理更新（客户端 tick 线程）。 */
	private static void applyTransition(Frame f, boolean hasBox, Minecraft mc) {
		boolean next = ImeCandidateLayout.decideShowing(showing, f, hasBox);
		// 焦点恢复补显（ImeCandidateLayout.shouldReshowCurrent 合同）：native
		// 已消耗该 visible 帧的 epoch，失焦抖动 → 恢复后轮询恒 null，不补显
		// 候选窗就要等下一次按键才回来（sync8 复盘 bug B7）。
		if(!next && ImeCandidateLayout.shouldReshowCurrent(showing, f, hasBox, current)) {
			next = true;
			reshowCount++;
		}
		if(!next) {
			if(f != null && !f.visible()) current = null;
			showing = false;
			lastRect = null;
			return;
		}
		if(f == null) {
			// 无新状态：维持（首帧前 current==null 时 extract 自然跳过）。
			showing = true;
			// 自愈：上次上传失败留下 texture==null/尺寸不符时，current 会
			// 永远画不出来（decideShowing 已 true 而 extract 被 texture 门挡死）
			// ——用保留帧重试（1s 节流防分配风暴）。
			healTexture();
			return;
		}
		current = f;
		showing = true;
		uploadFrame(f);
	}

	// ═══════════════════════════════ 绘制 ═══════════════════════════════

	/** Screen render hook（ScreenRenderMixin → extractRenderStateWithTooltipAndSubtitles RETURN）。 */
	public static void extract(GuiGraphicsExtractor ctx) {
		extractHits++;
		if(!extractHitLogged) {
			// 注入证明（观测层）：本行出现 = ScreenRenderMixin 目标方法在运行时
			// 真实命中。若 latest.log 没有它而五元组正常翻转 → 渲染钩子断了
			// （方法改名/签名不符），与候选窗逻辑无关。
			extractHitLogged = true;
			WaylandCraftCommon.LOGGER.info(
					"[ime][overlay] ScreenRenderMixin 注入生效（extract 首次命中; showing={} textureReady={}）",
					showing, texture != null);
		}
		Minecraft mc = Minecraft.getInstance();
		if(mc == null) return;
		EditBox box = focusedEditBox(mc);
		Frame f = current;
		boolean popupInPlace = showing && f != null && f.visible() && f.argb() != null && texture != null;

		// 状态透明度角标：聚焦输入框 + 引擎工作态（预编辑/候选流在场）但
		// 候选窗帧不在场 → 输入框右上角"拼"角标——解决"分不清 en 还是拼音"。
		// 协议限制如实标注：im2 不向合成器暴露引擎名，角标语义 =「中文引擎
		// 工作态」而非引擎名；keyboard-us/未激活永不产生预编辑 → 无角标。
		if(box != null && !popupInPlace && workState) drawEngineBadge(ctx, box, mc);

		if(!popupInPlace) return;
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

	// ═══════════════════════════ Phase 3 输入回传 ═══════════════════════════

	/**
	 * 候选窗区域点击 → native wl_pointer 序列（enter/motion/button）。
	 * mx/my = GUI 逻辑坐标；button = GLFW 码（0=左），内部转 evdev（+0x110）。
	 * 返回 true = 事件已消费（overlay 在场且点在区域内——游戏/聊天不得处理）。
	 */
	public static boolean handleClick(double mx, double my, int button, boolean pressed) {
		Rect r = hitRect();
		if(r == null || !r.contains(mx, my)) return false;
		int[] local = popupLocalCoords(mx, my, r);
		try {
			if(WaylandCraft.instance != null && WaylandCraft.instance.bridge != null) {
				WaylandCraft.instance.bridge.imePopupClick(local[0], local[1], 0x110 + button, pressed);
			}
		} catch(UnsatisfiedLinkError | RuntimeException e) {
			// 消费但不回传（旧库）：避免误点穿透到游戏。
		}
		return true;
	}

	/** 候选窗区域滚轮 → native wl_pointer axis（fcitx5 翻页）。dy = GLFW 语义。 */
	public static boolean handleScroll(double mx, double my, double dy) {
		Rect r = hitRect();
		if(r == null || !r.contains(mx, my)) return false;
		int[] local = popupLocalCoords(mx, my, r);
		try {
			if(WaylandCraft.instance != null && WaylandCraft.instance.bridge != null) {
				// 取负对齐既有语义（WaylandCraft.onScroll 发窗口时同样 -scrollY：
				// GLFW 正=向上翻 → wl axis 负=向上，见 fcitx5 wheel(up) 判据）。
				WaylandCraft.instance.bridge.imePopupAxis(local[0], local[1], -dy);
			}
		} catch(UnsatisfiedLinkError | RuntimeException e) {
			// 同上：消费即可。
		}
		return true;
	}

	/** 显示矩形坐标 → popup 像素空间局部坐标（等比映射 + 钳进帧内）。
	 * 纯映射逻辑（不碰 bridge），单测直打。 */
	public static int[] popupLocalCoords(double mx, double my, Rect r) {
		Frame f = current;
		int lx, ly;
		if(f != null && f.width() > 0 && f.height() > 0) {
			// 显示矩形按帧等比映射到 popup 像素空间（收缩态安全）。
			lx = clampInt((int) Math.floor((mx - r.x()) * f.width() / (double) r.w()), 0, f.width() - 1);
			ly = clampInt((int) Math.floor((my - r.y()) * f.height() / (double) r.h()), 0, f.height() - 1);
		} else {
			lx = clampInt((int) Math.floor(mx - r.x()), 0, Math.max(0, r.w() - 1));
			ly = clampInt((int) Math.floor(my - r.y()), 0, Math.max(0, r.h() - 1));
		}
		return new int[] {lx, ly};
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
			// 观测层：计数 + 5s 节流 WARN（旧版每次即时 warn，GL 侧持续失败
			// 会刷屏；现在与轮询异常同一 WARN 通道，配五元组 texture 位判读）。
			uploadFails++;
			warnThrottled("候选窗纹理上传失败 w=" + f.width() + " h=" + f.height(), e);
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

	/** 自愈重传：showing 中但纹理缺失/尺寸不符（upload 曾失败）→ 重试，1s 节流。 */
	private static void healTexture() {
		Frame f = current;
		if(f == null || !f.visible()) return;
		if(texture != null && texW == f.width() && texH == f.height()) return;
		long now = System.currentTimeMillis();
		if(now - lastHealMs < 1000) return;
		lastHealMs = now;
		uploadFrame(f);
	}

	/** 引擎工作态角标：输入框右上角"拼"色块（纯 fill + 原版字体，零新资源）。 */
	private static void drawEngineBadge(GuiGraphicsExtractor ctx, EditBox box, Minecraft mc) {
		if(mc.font == null) return;
		int bx = box.getX() + box.getWidth() - 13;
		int by = box.getY() + 1;
		ctx.fill(bx - 2, by - 1, bx + 13, by + 12, BADGE_BG);
		ctx.text(mc.font, "拼", bx, by, BADGE_FG);
	}

	// ══════════════════════════════ 观测层 ══════════════════════════════

	/** WARN 统一通道：5s 节流一条，带累计计数——高频断链不刷屏但绝不静默。 */
	private static void warnThrottled(String what, Throwable t) {
		long now = System.currentTimeMillis();
		if(now - lastWarnMs < 5000) return;
		lastWarnMs = now;
		if(t == null) {
			WaylandCraftCommon.LOGGER.warn(
					"[ime][overlay] {}（累计 polls={} frames={} exceptions={} uploadFails={}）",
					what, polls, framesDelivered, pollExceptions, uploadFails);
		} else {
			WaylandCraftCommon.LOGGER.warn(
					"[ime][overlay] {}: {}（累计 polls={} frames={} exceptions={} uploadFails={}）",
					what, t.toString(), polls, framesDelivered, pollExceptions, uploadFails);
		}
	}

	/**
	 * 五元组 (frame, visible, hasBox, showing, texture) 任何翻转 → INFO 一条
	 * （1s 节流；对比"上次已打印元组"，节流期内的翻转由下一轮轮询补打，
	 * 不丢状态轨迹）。这是"候选窗为什么不显示"的一眼判读行：
	 * frame=false 恒 false → 看 exceptions（轮询断链）或 /wl status native 侧；
	 * visible=false → 可见性门（im2/focus/kb/frame）——对照 ime_candidate_popup；
	 * hasBox=false → 焦点组件不是 EditBox；showing=false 而前三真 → 状态机；
	 * texture=false → GL 上传断链（uploadFails>0 佐证）。
	 */
	private static void logStateTransition(boolean frameDelivered, boolean hasBox) {
		String tuple = "(frame=" + frameDelivered
				+ ", visible=" + (current != null && current.visible())
				+ ", hasBox=" + hasBox
				+ ", showing=" + showing
				+ ", texture=" + (texture != null) + ")";
		if(tuple.equals(lastLoggedTuple)) return;
		long now = System.currentTimeMillis();
		if(now - lastTupleMs < 1000) return;
		lastTupleMs = now;
		lastLoggedTuple = tuple;
		WaylandCraftCommon.LOGGER.info(
				"[ime][overlay] 状态五元组 {} | polls={} frames={} incomingVisible={} exceptions={} uploadFails={} reshow={} extracts={} workState={}",
				tuple, polls, framesDelivered, incomingVisible, pollExceptions, uploadFails, reshowCount, extractHits, workState);
	}

	private static int clampInt(int v, int lo, int hi) {
		return v < lo ? lo : Math.min(v, hi);
	}

	/** 诊断（日志/status）：当前是否展示候选窗。 */
	public static boolean isShowing() {
		return showing;
	}
}
