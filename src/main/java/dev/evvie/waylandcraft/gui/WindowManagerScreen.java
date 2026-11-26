package dev.evvie.waylandcraft.gui;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;

import org.joml.Matrix3x2fStack;
import org.lwjgl.glfw.GLFW;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.bridge.WLCAbstractWindow;
import dev.evvie.waylandcraft.bridge.WLCPopup;
import dev.evvie.waylandcraft.bridge.WLCSurface;
import dev.evvie.waylandcraft.bridge.WLCToplevel;
import dev.evvie.waylandcraft.bridge.WaylandCraftBridge;
import dev.evvie.waylandcraft.gui.WindowSelectionLogic.Candidate;
import dev.evvie.waylandcraft.gui.WindowSelectionLogic.Decision;
import dev.evvie.waylandcraft.gui.WindowSelectionLogic.Mode;
import dev.evvie.waylandcraft.render.RenderUtils;
import dev.evvie.waylandcraft.render.WindowFramebuffer;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.client.input.KeyEvent;
import net.minecraft.client.input.MouseButtonEvent;
import net.minecraft.network.chat.Component;

/**
 * 极简窗口管理屏：纯命令行模式下唯一保留的 GUI。
 *
 * 设计要点：
 * - 不透明纯色背景（类似 Linux 终端，无透明）
 * - 窗口以 1:1 物理像素渲染 framebuffer（与原版渲染方式一致）
 * - 窗口四周只有 1px 细边框（类似无边框手机屏幕的微边缘），
 *   聚焦窗口亮边框、非聚焦暗边框，不遮挡窗口内容
 * - 无任何按钮/列表/装饰；所有操作通过 /wl 命令完成
 */
public class WindowManagerScreen extends Screen {
	
	private WaylandCraft wlc;
	
	/** 纯色背景（不透明，接近终端深色） */
	private static final int BG_COLOR = 0xFF000000;
	/** 聚焦窗口边框（亮） */
	private static final int BORDER_FOCUSED = 0xFFFFFFFF;
	/** 非聚焦窗口边框（暗） */
	private static final int BORDER_UNFOCUSED = 0xFF505050;
	/** 边框宽度（物理像素） */
	private static final int BORDER_WIDTH = 1;
	
	private int guiScale;
	
	private boolean captureModeEnabled = false;
	
	private WLCToplevel focused = null;
	private WLCToplevel lastFocused = null;

	// ===== 本地直显兜底（缺陷 1：B 屏空白）=====
	// 选择/排序/回退逻辑本体是 WindowSelectionLogic 纯函数（有单测）；这里只做
	// bridge 数据适配与绘制。空白根因：旧实现无条件取 getMostRecentFocus() 并直接
	// 渲染其表面树——bridge 无 toplevel 时它是 null，而焦点序列表为空时会把**所有**
	// toplevel 按创建顺序兜底种入（非 null），其中最后一个是未映射/零尺寸辅助窗口
	// 时同样整屏画不出任何东西，且键盘焦点被抢给废窗口。两条路径都表现为"按 B 全空"。

	/** 用户在本屏手动选中的窗口（0=无）；选中即 focusSurface，进入既有表面树视图。 */
	private long manualSelectionHandle = 0;
	/** TAB 唤出的强制列表页（绘制一帧后消费复位）。 */
	private boolean forceListView = false;
	/** 上一渲染帧的决策模式（供 keyPressed/mouseClicked 判定当前处于列表/指引页）。 */
	private Mode lastMode = Mode.GUIDE;
	/** 上一渲染帧的卡片矩形（GUI 像素），点击命中用。 */
	private final ArrayList<SelectionCard> cards = new ArrayList<SelectionCard>();

	private record SelectionCard(long handle, int x0, int y0, int x1, int y1) {}
	
	// All window elements currently displayed, sorted by depth from bottom-most (root) to top-most (last leaf)
	public ArrayList<WindowElement> windows = new ArrayList<WindowElement>();
	
	private ImplicitGrab implicitGrab = null;
	
	public WindowManagerScreen(WaylandCraft wlc) {
		super(Component.literal("Window Manager"));
		this.wlc = wlc;
	}
	
	@Override
	protected void init() {
		wlc.bridge.activateKeyboard();
	}
	
	@Override
	public boolean isPauseScreen() {
		return false;
	}
	
	@Override
	public void extractRenderState(GuiGraphicsExtractor context, int i, int j, float f) {
		super.extractBlurredBackground(context);
		
		// 不透明纯色背景
		context.fill(0, 0, width, height, BG_COLOR);
		
		guiScale = (int) Minecraft.getInstance().getWindow().getGuiScale();
		wlc.bridge.setOutputBounds(width * guiScale, height * guiScale);
		
		WLCToplevel renderToplevel = null;
		lastFocused = focused;

		// 焦点选择：可用焦点 → 既有表面树视图；焦点为 null 或指向不可渲染的辅助
		// 窗口（本地直显从未真正建立键盘焦点的两种空白路径）→ 卡片列表兜底；
		// 无任何可渲染窗口 → 指引文案。判定本体见 WindowSelectionLogic.decide（纯函数）。
		WLCToplevel focusToplevel = wlc.bridge.getMostRecentFocus();
		List<Candidate> allCandidates = buildCandidates();
		List<Candidate> usable = WindowSelectionLogic.usableCandidates(allCandidates);
		Decision decision = WindowSelectionLogic.decide(forceListView, manualSelectionHandle,
				focusToplevel != null ? focusToplevel.getHandle() : 0, usable);
		lastMode = decision.mode();
		forceListView = false; // TAB 请求消费一次

		focused = null;
		if(decision.mode() == Mode.TREE) {
			focused = wlc.bridge.getToplevel(decision.handle());
		}
		wlc.bridge.focusSurface(focused); // LIST/GUIDE 下 focused=null：清焦点，按键不会漏进废窗口
		renderToplevel = focused;

		cards.clear();

		windows.clear();
		
		float guiScaleF = (float) Minecraft.getInstance().getWindow().getGuiScale();
		Matrix3x2fStack poseStack = context.pose();
		poseStack.pushMatrix();
		poseStack.scale(1 / guiScaleF, 1 / guiScaleF);
		
		if(renderToplevel != null) {
			prepareToplevel(renderToplevel);
			
			for(WindowElement element : windows) {
				WindowFramebuffer buf = element.window.framebuffer;
				if(buf == null) continue;
				
				int x = (int) element.x - buf.getXOff();
				int y = (int) element.y - buf.getYOff();
				int w = buf.getWidth();
				int h = buf.getHeight();
				
				// 细边框（物理像素，画在窗口外圈，不遮挡窗口内容）
				boolean isFocused = element.window == renderToplevel;
				int borderColor = isFocused ? BORDER_FOCUSED : BORDER_UNFOCUSED;
				int b = BORDER_WIDTH;
				context.fill(x - b, y - b, x + w + b, y, borderColor);         // top
				context.fill(x - b, y + h, x + w + b, y + h + b, borderColor); // bottom
				context.fill(x - b, y, x, y + h, borderColor);                 // left
				context.fill(x + w, y, x + w + b, y + h, borderColor);         // right
				
				RenderUtils.renderFramebuffer2D(context, buf, x, y, w, h);
			}
		}
		
		poseStack.popMatrix();

		if(decision.mode() == Mode.LIST) drawCardList(context, usable);
		else if(decision.mode() == Mode.GUIDE) drawGuide(context, allCandidates.isEmpty());

		super.extractRenderState(context, i, j, f);
	}

	/** bridge 枚举 → 候选描述；renderable=画得出内容（有非零尺寸 framebuffer）。 */
	private List<Candidate> buildCandidates() {
		List<Candidate> out = new ArrayList<Candidate>();
		for(WLCToplevel t : wlc.bridge.getToplevels()) {
			int w = t.geometry != null ? t.geometry.width() : 0;
			int h = t.geometry != null ? t.geometry.height() : 0;
			boolean renderable = t.isAlive() && t.framebuffer != null && t.geometry != null && w > 0 && h > 0;
			out.add(new Candidate(t.getHandle(), WaylandCraft.getWindowName(t), w, h, renderable));
		}
		return out;
	}

	/** 可渲染窗口列表页：数字键或点击选中 → focusSurface 进入既有表面树视图。 */
	private void drawCardList(GuiGraphicsExtractor context, List<Candidate> usable) {
		List<Candidate> shown = WindowSelectionLogic.visibleCards(usable);

		int rowH = font.lineHeight + 8;
		int cardW = Math.min(280, width - 40);
		int x0 = (width - cardW) / 2;
		int y0 = height / 2 - (shown.size() * rowH) / 2 - 12;

		context.text(font, "选择窗口（数字键 1-9 或点击；TAB 返回）", x0, y0 - 14, 0xFFFFAA00, false);

		for(int idx = 0; idx < shown.size(); idx++) {
			Candidate c = shown.get(idx);
			int top = y0 + idx * rowH;

			context.fill(x0, top, x0 + cardW, top + rowH - 2, 0xFF1A1A1A);
			context.fill(x0, top, x0 + cardW, top + 1, BORDER_UNFOCUSED);
			context.fill(x0, top + rowH - 3, x0 + cardW, top + rowH - 2, BORDER_UNFOCUSED);
			context.fill(x0, top, x0 + 1, top + rowH - 2, BORDER_UNFOCUSED);
			context.fill(x0 + cardW - 1, top, x0 + cardW, top + rowH - 2, BORDER_UNFOCUSED);

			String title = font.plainSubstrByWidth(c.title(), cardW - 150);
			String line = (idx + 1) + ". " + title + "   " + c.width() + "x" + c.height()
					+ "   0x" + Long.toHexString(c.handle() & 0xFFFF);
			context.text(font, line, x0 + 8, top + 4, 0xFFFFFFFF, false);

			cards.add(new SelectionCard(c.handle(), x0, top, x0 + cardW, top + rowH - 2));
		}

		if(usable.size() > shown.size()) {
			context.text(font, "…更多窗口请用 /wl list windows", x0, y0 + shown.size() * rowH + 2, 0xFF999999, false);
		}
	}

	/** 空列表指引页：明确告诉用户下一步做什么，绝不再画空白屏。
	 *  emptyBridge=true 桥接器完全无窗口（先 launch/capture）；
	 *  否则窗口存在但还没有可渲染内容（提示等待首帧/place 兜底）。 */
	private void drawGuide(GuiGraphicsExtractor context, boolean emptyBridge) {
		String l1 = "WaylandCraft 本地直显：还没有窗口";
		String l2 = "先运行 /wl launch <应用> 或 /wl capture 捕获桌面窗口";
		String l3 = "之后按 B 回到这里选择窗口；/wl place <handle> 摆位到面前";
		if(!emptyBridge) {
			l1 = "窗口已存在，但还没有可显示的内容（等待应用首帧）";
			l2 = "列表为空时可直接 /wl place <handle> 强制建显示";
			l3 = "仍空白请查看 /wl status 的 toplevels/mapped/displays 计数";
		}
		int y = height / 2 - 24;
		for(String line : new String[] {l1, l2, l3}) {
			context.text(font, line, width / 2 - font.width(line) / 2, y, 0xFFCCCCCC, false);
			y += 12;
		}
	}

	/** 选中卡片：focusSurface 后既有决策链直接进表面树视图。 */
	private void selectWindow(long handle) {
		manualSelectionHandle = handle;
		forceListView = false;
		WLCToplevel t = wlc.bridge.getToplevel(handle);
		if(t != null) wlc.bridge.focusSurface(t);
	}
	
	@Override
	public void extractBackground(GuiGraphicsExtractor guiGraphics, int i, int j, float f) {
	}
	
	private HoveredSurface surfaceUnderPointer(double x, double y) {
		for(int i = windows.size() - 1; i >= 0; i--) {
			WindowElement element = windows.get(i);
			
			float sx = (float) x - element.x;
			float sy = (float) y - element.y;
			
			for(WLCSurface surface = element.window.getSurfaceTreeLast(); surface != null; surface = surface.getPrevChild()) {
				float rx = sx - surface.xSubpos;
				float ry = sy - surface.ySubpos;
				
				int width = surface.width();
				int height = surface.height();
				
				if(rx < 0 || ry < 0 || rx > width || ry > height) {
					continue;
				}
				
				if(!surface.isAlive()) continue;
				
				if(wlc.bridge.inputRegionContains(surface, rx, ry)) {
					return new HoveredSurface(surface, rx, ry);
				}
			}
		}
		
		return null;
	}
	
	@Override
	public void mouseMoved(double x, double y) {
		x *= guiScale;
		y *= guiScale;
		
		HoveredSurface hovered = surfaceUnderPointer(x, y);
		
		if(implicitGrab != null && !implicitGrab.surface.isAlive()) implicitGrab = null;
		
		if(implicitGrab == null) {
			if(hovered != null) wlc.bridge.sendMotionRefocus(hovered.surface, hovered.rx, hovered.ry);
			else wlc.bridge.sendMotionOutside();
		}
		else {
			for(WindowElement elem : windows) {
				WLCSurface surface;
				for(surface = elem.window.getSurfaceTree(); surface != null && surface != implicitGrab.surface; surface = surface.getNextChild()) {}
				if(surface == implicitGrab.surface) {
					// Surface was found in this window elements' surface tree
					
					float rx = (float) x - elem.x - surface.xSubpos;
					float ry = (float) y - elem.y - surface.ySubpos;
					
					wlc.bridge.sendMotion(rx, ry);
					break;
				}
			}
		}
	}
	
	@Override
	public boolean mouseClicked(MouseButtonEvent event, boolean doubleClick) {
		if(super.mouseClicked(event, doubleClick)) return true;

		// 列表页/指引页优先命中卡片（GUI 单位坐标，与 drawCardList 记录的一致）
		if(lastMode == Mode.LIST) {
			for(SelectionCard c : cards) {
				if(event.x() >= c.x0() && event.x() < c.x1() && event.y() >= c.y0() && event.y() < c.y1()) {
					selectWindow(c.handle());
					return true;
				}
			}
			// 列表页不吃世界表面点击（此时 windows 为空，逻辑上也命中不到）
		}

		double x = event.x() * guiScale;
		double y = event.y() * guiScale;
		
		HoveredSurface hovered = surfaceUnderPointer(x, y);
		if(implicitGrab == null && hovered != null) {
			implicitGrab = new ImplicitGrab(hovered.surface);
		}
		
		if(implicitGrab != null && !implicitGrab.pressedMouseButtons.contains(event.button())) {
			implicitGrab.pressedMouseButtons.add(event.button());
			wlc.bridge.sendButton(0x110 + event.button(), 1);
			
			return true;
		}
		
		return false;
	}
	
	@Override
	public boolean mouseReleased(MouseButtonEvent event) {
		if(super.mouseReleased(event)) return true;
		
		if(implicitGrab != null && implicitGrab.pressedMouseButtons.contains(event.button())) {
			implicitGrab.pressedMouseButtons.remove(event.button());
			wlc.bridge.sendButton(0x110 + event.button(), 0);
			
			if(implicitGrab.pressedMouseButtons.isEmpty()) implicitGrab = null;
			
			return true;
		}
		
		return false;
	}
	
	@Override
	public boolean keyPressed(KeyEvent event) {
		if(event.key() == GLFW.GLFW_KEY_ESCAPE && !captureModeEnabled) {
			this.onClose();
			return true;
		}
		
		if(event.key() == GLFW.GLFW_KEY_Q && event.modifiers() == GLFW.GLFW_MOD_ALT) {
			captureModeEnabled = !captureModeEnabled;
			return true;
		}

		// TAB：在树视图/列表页之间切换（树视图下单向唤出列表；列表页下收起回焦点视图）
		if(event.key() == GLFW.GLFW_KEY_TAB) {
			forceListView = lastMode != Mode.LIST;
			return true;
		}

		// 列表页：数字键 1-9 选中卡片 → focusSurface 进入表面树视图
		if(lastMode == Mode.LIST) {
			int idx = WindowSelectionLogic.cardIndexForNumberKey(event.key(), cards.size());
			if(idx >= 0) {
				selectWindow(cards.get(idx).handle());
				return true;
			}
		}

		// Forward key press to current window
		if(focused != null) {
			int scancode = WaylandCraft.correctScancode(event.scancode());
			wlc.bridge.pressKey(scancode);
			return true;
		}
		
		return false;
	}
	
	@Override
	public boolean keyReleased(KeyEvent event) {
		if(super.keyReleased(event)) return true;
		
		if(focused != null) {
			int scancode = WaylandCraft.correctScancode(event.scancode());
			wlc.bridge.releaseKey(scancode);
			return true;
		}
		
		return false;
	}
	
	@Override
	public boolean mouseScrolled(double mouseX, double mouseY, double scrollX, double scrollY) {
		if(super.mouseScrolled(mouseX, mouseY, scrollX, scrollY)) return true;
		
		mouseX *= guiScale;
		mouseY *= guiScale;
		
		HoveredSurface hovered = surfaceUnderPointer(mouseX, mouseY);
		
		if(hovered != null) {
			wlc.bridge.sendScroll(0, -scrollY);
			wlc.bridge.sendScroll(1, -scrollX);
			return true;
		}
		
		return false;
	}
	
	@Override
	public void removed() {
		if(implicitGrab != null) {
			implicitGrab.pressedMouseButtons.forEach((button) -> wlc.bridge.sendButton(0x110 + button, 0));
			implicitGrab = null;
		}
		wlc.bridge.deactivateKeyboard();
	}
	
	private void prepareToplevel(WLCToplevel toplevel) {
		float x;
		float y;
		
		if(!toplevel.fullscreen || !captureModeEnabled) {
			x = Math.max(0, width * guiScale / 2.0f - toplevel.geometry.width() / 2.0f);
			y = Math.max(0, height * guiScale / 2.0f - toplevel.geometry.height() / 2.0f);
		}
		else {
			x = 0;
			y = 0;
		}
		
		x -= toplevel.geometry.x();
		y -= toplevel.geometry.y();
		
		windows.add(new WindowElement(toplevel, x, y));
		
		WindowTree tree = WindowTree.constructTree(wlc.bridge, toplevel);
		preparePopupTree(tree, x, y);
	}
	
	private void preparePopupTree(WindowTree tree, float x, float y) {
		if(tree.window instanceof WLCPopup) {
			WLCPopup popup = (WLCPopup) tree.window;
			
			x += popup.getParent().geometry.x();
			y += popup.getParent().geometry.y();
			
			x += popup.offsetX;
			y += popup.offsetY;
			
			x -= popup.geometry.x();
			y -= popup.geometry.y();
			
			windows.add(new WindowElement(popup, x, y));
		}
		
		for(WindowTree child : tree.children) {
			preparePopupTree(child, x, y);
		}
	}
	
	public static class WindowElement {
		
		public WLCAbstractWindow window;
		public float x;
		public float y;
		
		public WindowElement(WLCAbstractWindow window, float x, float y) {
			this.window = window;
			this.x = x;
			this.y = y;
		}
		
	}
	
	public static class WindowTree {
		
		public WLCAbstractWindow window;
		public ArrayList<WindowTree> children;
		
		private WindowTree(WLCAbstractWindow window) {
			this.window = window;
			this.children = new ArrayList<WindowTree>();
		}
		
		public static WindowTree constructTree(WaylandCraftBridge bridge, WLCToplevel toplevel) {
			WindowTree tree = new WindowTree(toplevel);
			
			for(WLCPopup popup : bridge.getMappedPopups()) {
				WLCAbstractWindow root;
				for(root = popup; !(root instanceof WLCToplevel); root = ((WLCPopup) root).getParent()) {}
				if(root != toplevel) continue;
				addRecursive(tree, popup);
			}
			
			return tree;
		}
		
		private static WindowTree addRecursive(WindowTree tree, WLCPopup popup) {
			WLCAbstractWindow parentWindow = popup.getParent();
			WindowTree parent;
			if(parentWindow instanceof WLCPopup) {
				parent = addRecursive(tree, (WLCPopup) parentWindow);
			}
			else {
				parent = tree;
			}
			
			for(WindowTree child : parent.children) {
				if(child.window == popup) return child;
			}
			
			WindowTree child = new WindowTree(popup);
			parent.children.add(child);
			return child;
		}
		
	}
	
	private static record HoveredSurface(WLCSurface surface, float rx, float ry) {}
	
	private static class ImplicitGrab {
		
		public final WLCSurface surface;
		public HashSet<Integer> pressedMouseButtons = new HashSet<Integer>();
		
		public ImplicitGrab(WLCSurface surface) {
			this.surface = surface;
		}
		
	}
	
}
