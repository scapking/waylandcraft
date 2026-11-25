package dev.evvie.waylandcraft.ime;

import org.jetbrains.annotations.Nullable;

import com.mojang.blaze3d.vertex.PoseStack;

import dev.evvie.waylandcraft.WindowDisplay;
import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraft.KeyboardCaptureMode;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.bridge.WLCAbstractWindow;
import dev.evvie.waylandcraft.bridge.WLCAbstractWindow.SurfaceGeometry;
import dev.evvie.waylandcraft.bridge.WLCSurface;
import dev.evvie.waylandcraft.bridge.WLCToplevel;
import dev.evvie.waylandcraft.ime.ImeCandidateLayout.Frame;
import dev.evvie.waylandcraft.ime.ImeCandidateLayout.Rect;
import dev.evvie.waylandcraft.math.WorldPlane;
import dev.evvie.waylandcraft.render.RenderUtils;
import net.fabricmc.fabric.api.client.rendering.v1.level.LevelRenderContext;
import net.minecraft.client.Camera;
import net.minecraft.client.Minecraft;
import net.minecraft.resources.Identifier;
import net.minecraft.world.phys.Vec3;

/**
 * 候选窗世界形态渲染/交互（sync14 最后一块拼图）。
 *
 * 场景：键盘捕获（G=CAPTURE / J=HARD_CAPTURE）下往**游戏内应用窗口**打字——
 * mc.screen==null，GUI 形态（ImeCandidateOverlay 锚 focusedEditBox）无处显示，
 * 但 native 采集/交付链路正常（生产实锤：gate visible=true、Java
 * incomingVisible>0）。本类把同一帧纹理（共用 ImeCandidateOverlay 的
 * DynamicTexture）画到目标窗口 quad 旁边的**世界空间**，并接管点选/滚轮回传。
 *
 * 数据流（世界形态）：
 * <pre>
 * 嵌套 fcitx5 popup ──(shm 帧)──→ native input_popup 采集 ──ime_popup_frame──→ ImeCandidateOverlay.tick
 *                                                                                   │ worldFrame 保留
 * 嵌套应用 ti3 set_cursor_rectangle ──→ im2.last_cursor_rect ──imePopupAnchor──→ tick 轮询
 *                                                                                   │
 *                        ImeCandidateLayout.positionWorldPopup（纯函数，单测直打）
 *                                                                                   │
 *            窗口平面局部像素 rect ──→ render（localX/localY 换算世界单位 quad）
 *                                    └→ handleClick/handleScroll 命中 → ime_popup_click/axis
 * </pre>
 *
 * 坐标链（锚定）：cursor rect 是聚焦应用 **surface 像素**；平面上
 * geometryLocal = surfaceLocal − (geometry.x, geometry.y)（WindowDisplay.intersect
 * 正变换的逆）。无 rect（null 或 0x0）回退窗口 quad 底部中央。候选窗尺寸按
 * 帧像素 1:1 映射窗口像素，世界尺寸随窗口 pixelScale/viewScale 联动。
 *
 * 命中：候选 quad 与窗口 quad 共面（沿窗口法线向相机抬 LIFT_BLOCKS 防 z-fight），
 * 判定输入点（HARD_CAPTURE=窗口虚拟光标 surface 局部，CAPTURE=准星射线∩窗口平面）
 * 换算成平面局部像素后先测候选 rect——命中即消费，**优先于窗口本体**。
 *
 * 线程：tick()/render() 均在渲染线程（runTick HEAD / COLLECT_SUBMITS）；
 * handleClick/handleScroll 在 GLFW 回调（同线程）。与 GUI 形态显示门互斥
 * （GUI 要求 screen!=null 且 focusedEditBox，世界要求 screen==null），纹理
 * 复用不冲突。
 */
public final class ImeCandidateWorld {

	/** 光标矩形与候选窗的间距（窗口像素；500 ppb 下约 8mm，观感同 GUI GAP）。 */
	static final int GAP_PX = 4;
	/** 候选 quad 沿窗口法线向相机方向的抬升（方块）——保证与窗口内容共面时深度必胜。 */
	static final double LIFT_BLOCKS = 0.02;

	private static boolean worldShowing;
	private static @Nullable WindowDisplay target;   // 锚定的窗口（仅 worldShowing 时有值）
	private static @Nullable Rect rect;              // 候选窗矩形（target 平面局部像素）
	private static boolean anchoredByCursorRect;     // 锚定模式观测：true=ti3 光标矩形，false=窗口底部回退
	private static int[] lastAnchorRaw;              // 最近一次 imePopupAnchor 原始值（/wl ime info）

	private ImeCandidateWorld() {}

	// ══════════════════════════ tick：显隐 + 锚定解算 ══════════════════════════

	/** 每客户端 tick 调用（MinecraftMixin.runTick HEAD，紧随 ImeCandidateOverlay.tick）。 */
	public static void tick() {
		WaylandCraft wlc = WaylandCraft.instance;
		Minecraft mc = Minecraft.getInstance();
		// 场景门（与 GUI 形态互斥的判据全部在此）：
		// - screen==null：聊天框/搜索框在场时 GUI overlay 接管（回归零风险）；
		// - 捕获模式非 NONE：候选窗只在往窗口打字的语境有意义；
		// - 非共享窗口捕获：共享窗口（网络转发）不在本形态范围。
		boolean eligible = wlc != null && wlc.bridge != null && mc != null && mc.level != null
				&& mc.screen == null
				&& wlc.keyboardCaptureMode != KeyboardCaptureMode.NONE
				&& wlc.sharedKeyboardCapture == null;

		WindowDisplay t = eligible ? wlc.imeWorldTargetDisplay() : null;
		if(t != null && !t.isValid()) t = null;

		Frame f = ImeCandidateOverlay.peekWorldFrame();
		// 显隐状态机（ImeCandidateLayout 纯函数）：窗口在场且聚焦 && 帧 visible → 显示。
		// worldFrame 只在 invisible 交付时清空，无 GUI 那套"补显"空窗，f==null
		// 维持即可，无需 shouldReshowCurrentWorld（保留帧与 f 同源恒等）。
		boolean next = ImeCandidateLayout.decideShowingWorld(worldShowing, f, t != null);

		if(!next) {
			boolean was = worldShowing;
			worldShowing = false;
			target = null;
			rect = null;
			anchoredByCursorRect = false;
			if(was) {
				WaylandCraftCommon.LOGGER.info("[ime][world] worldVisible=false（窗口销毁/失焦/会话结束/候选帧消失）");
			}
			return;
		}

		// 不变式防御：worldShowing=true 蕴含 worldFrame 是可见帧（worldFrame 只在
		// invisible 交付时清空）；若未来状态机演化破坏该不变式，宁可隐藏也不 NPE。
		// t 同理非 null（decideShowingWorld 的 windowInPlace=false 分支已 return）。
		if(f == null || !f.visible() || t == null) {
			worldShowing = false;
			target = null;
			rect = null;
			return;
		}

		// 锚定：ti3 光标矩形（surface 像素）→ 平面局部；无/退化 → 窗口底部中央回退。
		int[] a = null;
		if(wlc.bridge != null) {
			try {
				a = wlc.bridge.imePopupAnchor();
			} catch(UnsatisfiedLinkError | RuntimeException e) {
				a = null; // 旧 .so 缺符号：永远走回退锚（窗口底部），功能仍可用
			}
		}
		lastAnchorRaw = a;
		SurfaceGeometry g = t.window.geometry;
		Rect cursor = null;
		if(a != null && a.length == 4 && (a[2] > 0 || a[3] > 0)) {
			// cursor rect 相对聚焦 surface 原点；平面局部 = 减 geometry 偏移
			// （WindowDisplay.intersect 正变换 geometryLocal+geometry 的逆）。
			cursor = new Rect(a[0] - g.x(), a[1] - g.y(), a[2], a[3]);
		}
		Rect r = ImeCandidateLayout.positionWorldPopup(cursor, g.width(), g.height(),
				f.width(), f.height(), GAP_PX);
		if(r == null) {
			// positionWorldPopup 对参数已有防御；理论上不可达。
			worldShowing = false;
			target = null;
			rect = null;
			return;
		}

		boolean was = worldShowing;
		worldShowing = true;
		target = t;
		rect = r;
		anchoredByCursorRect = cursor != null;
		if(!was) {
			WaylandCraftCommon.LOGGER.info(
					"[ime][world] worldVisible=true 窗口={} anchor={} rect=({},{} {}x{}) seq={}",
					windowTitle(t.window),
					anchoredByCursorRect ? "cursorRect" : "windowBottom",
					r.x(), r.y(), r.w(), r.h(), f.seq());
		}
		// 纹理就绪推进（渲染线程）：uploadFrame 失败计数/WARN 在 overlay 通道。
		ImeCandidateOverlay.ensureWorldTexture(f);
	}

	// ══════════════════════════════ 渲染（世界通道）══════════════════════════════

	/** WaylandCraft.renderWorld 末尾调用（COLLECT_SUBMITS）：窗口之后画 → 候选在上层。 */
	public static void render(LevelRenderContext ctx) {
		WindowDisplay d = target;
		Rect r = rect;
		Frame f = ImeCandidateOverlay.peekWorldFrame();
		if(!worldShowing || d == null || r == null || f == null || !f.visible()) return;
		if(!d.isValid()) return;
		if(!ImeCandidateOverlay.ensureWorldTexture(f)) return; // 纹理未就绪，本帧跳过
		Identifier texture = ImeCandidateOverlay.worldTextureId();

		// 与 WindowDisplay.render 同一套几何：顶点相对 origin，pose 平移到
		// origin-camera。localX/localY 已含 pixelScale*viewScale → 帧像素 1:1
		// 窗口像素，窗口缩放/距离观感自动联动。
		Vec3 localX = d.localX();
		Vec3 localY = d.localY();
		Vec3 cameraPos = ctx.levelState().cameraRenderState.pos;
		Vec3 originRel = d.origin().subtract(cameraPos);
		Vec3 lift = d.normal().scale(LIFT_BLOCKS); // 法线朝相机侧（锚定语义），防与窗口内容 z-fight

		Vec3 tl = localX.scale(r.x()).add(localY.scale(r.y())).add(lift);
		Vec3 bl = tl.add(localY.scale(r.h()));
		Vec3 br = bl.add(localX.scale(r.w()));
		Vec3 tr = tl.add(localX.scale(r.w()));

		PoseStack poseStack = ctx.poseStack();
		poseStack.pushPose();
		poseStack.translate(originRel.x, originRel.y, originRel.z);
		// cutout=false（translucent 混合，保留 cairo 圆角/半透背景）；
		// flipV=false：shm 行序（左上原点）与 NativeImage/Instance 的 tl→v=0
		// 约定一致（同 ImeCandidateOverlay.extract 的 GUI blit 语义）。
		RenderUtils.renderWindowTexture(texture, poseStack, ctx.submitNodeCollector(), false, false, tl, bl, br, tr);
		poseStack.popPose();
	}

	// ═══════════════════════════ 点选/滚轮回传（优先命中）═══════════════════════════

	/** 按压配对跟踪：press 在候选 quad 上被消费后，无论 release 落在哪都补
	 *  回传到最后命中坐标——保证 fcitx5 button(pressed)/button(released) 成对，
	 *  且不因用户松手瞬间抖出矩形而把 release 漏进窗口/游戏。 */
	private static final java.util.HashSet<Integer> pressedButtons = new java.util.HashSet<>();
	private static @Nullable int[] lastLocal;

	/**
	 * 捕获态鼠标点击命中候选 quad → ime_popup_click（popup 表面局部像素）。
	 * 调用方（WaylandCraft.onButtonPress）已保证在非拖拽独占态；本函数内部
	 * 再做完整门判定。返回 true = 已消费（候选 quad 优先于窗口本体）。
	 */
	public static boolean handleClick(int button, boolean pressed) {
		boolean tracked = pressedButtons.contains(button);
		if(!pressed && !tracked) return false; // 未跟踪的 release 不干预（窗口/GUI 原语义）
		int[] local = hitPopup();
		if(pressed) {
			if(local == null) return false;   // press 未命中候选 quad → 交还窗口
			pressedButtons.add(button);
			lastLocal = local;
		} else {
			// 走到这里必为 tracked release：无条件消费（松手抖出矩形也在最后
			// 命中坐标补 release，配对合同不破）。
			if(local == null) local = lastLocal != null ? lastLocal : new int[] {0, 0};
			pressedButtons.remove(button);
		}
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) return true; // 消费但不回传（同 GUI 旧库语义）
		try {
			wlc.bridge.imePopupClick(local[0], local[1], 0x110 + button, pressed);
		} catch(UnsatisfiedLinkError | RuntimeException e) {
			// 旧 .so：消费避免误点穿透到窗口。
		}
		return true;
	}

	/** 捕获态滚轮命中候选 quad → ime_popup_axis（fcitx5 翻页）。dy = GLFW 语义。 */
	public static boolean handleScroll(double dy) {
		int[] local = hitPopup();
		if(local == null) return false;
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) return true;
		try {
			// 取负对齐既有语义（GUI handleScroll / WaylandCraft.onScroll 同样 -scrollY）。
			wlc.bridge.imePopupAxis(local[0], local[1], -dy);
		} catch(UnsatisfiedLinkError | RuntimeException e) {
			// 消费即可。
		}
		return true;
	}

	/** 当前输入点 → 候选 rect 内 popup 局部像素；不在候选 quad 上 → null。 */
	private static @Nullable int[] hitPopup() {
		WindowDisplay d = target;
		Rect r = rect;
		if(!worldShowing || d == null || r == null) return null;
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || !d.isValid()) return null;
		double[] plane = inputPlaneCoords(wlc, d);
		if(plane == null) return null;
		return ImeCandidateLayout.worldHitToPopupLocal(r, plane[0], plane[1]);
	}

	/**
	 * 当前"指向"换算到窗口平面局部像素：
	 * - HARD_CAPTURE（J，鼠标锁定在窗口虚拟光标）→ pointerCapture 的 surface
	 *   局部坐标（surface 属于目标窗口才有效，否则不命中候选——视角射线不适用）；
	 * - CAPTURE/自由视角（G）→ 相机准星射线 ∩ 窗口平面。
	 */
	private static @Nullable double[] inputPlaneCoords(WaylandCraft wlc, WindowDisplay d) {
		if(wlc.pointerCapture != null) {
			WLCSurface s = wlc.pointerCapture.surface;
			if(s == null || wlc.windowOfSurface(s) != d.window) return null;
			SurfaceGeometry g = d.window.geometry;
			return ImeCandidateLayout.surfaceLocalToPlaneLocal(
					wlc.pointerCapture.x, wlc.pointerCapture.y, s.xSubpos, s.ySubpos, g.x(), g.y());
		}
		Minecraft mc = Minecraft.getInstance();
		if(mc.gameRenderer == null) return null;
		Camera camera = mc.gameRenderer.getMainCamera();
		if(camera == null) return null;
		WorldPlane.Intersection hit = d.getPlane().intersect(
				camera.position(), new Vec3(camera.forwardVector()));
		if(hit == null) return null;
		return new double[] { hit.local().x, hit.local().y };
	}

	// ══════════════════════════════ 观测 / 诊断 ══════════════════════════════

	/** 诊断窗口名（WLCToplevel 走既有 getWindowName，其余回退类标识）。 */
	private static String windowTitle(WLCAbstractWindow w) {
		return w instanceof WLCToplevel t ? WaylandCraft.getWindowName(t) : w.getClass().getSimpleName();
	}

	/** 世界形态显示中（五元组日志 worldVisible= 字段、/wl ime info）。 */
	public static boolean isWorldVisible() {
		return worldShowing;
	}

	/** 锚定模式：gui（GUI overlay 显示中）/ world（世界形态）/ none。 */
	public static String anchorMode() {
		if(ImeCandidateOverlay.isShowing()) return "gui";
		if(worldShowing) return "world";
		return "none";
	}

	/** /wl ime info 世界形态行：锚定模式 + rect + 原始 cursor rect（纯读，无副作用）。 */
	public static String describe() {
		String raw = lastAnchorRaw == null ? "null"
				: "(" + lastAnchorRaw[0] + "," + lastAnchorRaw[1] + ") " + lastAnchorRaw[2] + "x" + lastAnchorRaw[3];
		WindowDisplay d = target;
		Rect r = rect;
		return "anchor=" + anchorMode()
				+ (worldShowing
						? " window=" + (d != null ? windowTitle(d.window) : "?")
								+ " mode=" + (anchoredByCursorRect ? "cursorRect" : "windowBottom")
								+ " rect=" + (r != null ? "(" + r.x() + "," + r.y() + ") " + r.w() + "x" + r.h() : "null")
						: "")
				+ " ti3Rect=" + raw;
	}
}
