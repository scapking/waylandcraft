package dev.evvie.waylandcraft;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.stream.Stream;

import org.jetbrains.annotations.Nullable;
import org.lwjgl.glfw.GLFW;

import com.mojang.blaze3d.platform.InputConstants;

import dev.evvie.waylandcraft.WindowDisplay.DisplayHitResult;
import dev.evvie.waylandcraft.bridge.WLCAbstractWindow;
import dev.evvie.waylandcraft.bridge.WLCAbstractWindow.SurfaceGeometry;
import dev.evvie.waylandcraft.bridge.WLCPopup;
import dev.evvie.waylandcraft.bridge.WLCSurface;
import dev.evvie.waylandcraft.bridge.WLCToplevel;
import dev.evvie.waylandcraft.bridge.WaylandCraftBridge;
import dev.evvie.waylandcraft.bridge.WaylandCraftBridge.ResizeRequest;
import dev.evvie.waylandcraft.bridge.WaylandCraftBridge.Size;
import dev.evvie.waylandcraft.desktop.XDGDesktopManager;
import dev.evvie.waylandcraft.grabs.DNDGrab;
import dev.evvie.waylandcraft.grabs.MoveGrab;
import dev.evvie.waylandcraft.grabs.PointerGrabMap;
import dev.evvie.waylandcraft.grabs.PointerGrabMap.ImplicitGrab;
import dev.evvie.waylandcraft.grabs.ResizeGrab;
import dev.evvie.waylandcraft.gui.WaylandHudRenderer;
import dev.evvie.waylandcraft.gui.WindowManagerScreen;
import dev.evvie.waylandcraft.item.WindowItem;
import dev.evvie.waylandcraft.item.WindowItemManager;
import dev.evvie.waylandcraft.render.WindowInHandRenderer;
import dev.evvie.waylandcraft.render.WindowInItemFrameRenderer;
import dev.evvie.waylandcraft.render.model.WindowItemModel;
import dev.evvie.waylandcraft.render.SharedWindowDisplay;
import dev.evvie.waylandcraft.shared.WindowShareManager;
import dev.evvie.waylandcraft.shared.AudioCaptureManager;
import dev.evvie.waylandcraft.shared.AudioPlaybackManager;
import dev.evvie.waylandcraft.settings.WaylandCraftSettings;
import dev.evvie.waylandcraft.settings.WaylandCraftSettingsManager;
import dev.evvie.waylandcraft.network.SharedWindowClientHandler;
import dev.evvie.waylandcraft.network.SharedWindowInteractionPayload;
import dev.evvie.waylandcraft.command.WaylandCraftCommand;
import dev.evvie.waylandcraft.utils.CursorShape;
import dev.evvie.waylandcraft.capture.PipeWireCaptureManager;
import dev.evvie.waylandcraft.shared.RemoteWindowRenderer;
import net.fabricmc.api.ClientModInitializer;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientTickEvents;
import net.fabricmc.fabric.api.client.item.v1.ItemTooltipCallback;
import net.fabricmc.fabric.api.client.keymapping.v1.KeyMappingHelper;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayConnectionEvents;
import net.fabricmc.fabric.api.client.rendering.v1.level.LevelExtractionContext;
import net.fabricmc.fabric.api.client.rendering.v1.level.LevelRenderContext;
import net.fabricmc.fabric.api.client.rendering.v1.level.LevelRenderEvents;
import net.fabricmc.fabric.api.networking.v1.PacketSender;
import net.minecraft.ChatFormatting;
import net.minecraft.client.Camera;
import net.minecraft.client.KeyMapping;
import net.minecraft.client.Minecraft;
import net.minecraft.client.multiplayer.ClientPacketListener;
import net.minecraft.client.player.LocalPlayer;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.Identifier;
import net.minecraft.world.item.Item.TooltipContext;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.item.TooltipFlag;
import net.minecraft.world.phys.HitResult;
import net.minecraft.world.phys.Vec3;

public class WaylandCraft implements ClientModInitializer {
	
	private static final KeyMapping.Category KEYBIND_CATEGORY = KeyMapping.Category.register(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "keys"));
	
	public static WaylandCraft instance;
	
	public WaylandCraftSettingsManager settingsManager;
	public WaylandCraftSettings settings;
	
	public WaylandCraftBridge bridge = null;
	public String waylandSocket = "";
	
	/** Set when the native bridge could not be initialized (e.g. Android launcher
	 * with a bionic runtime); the mod disables itself instead of crashing. */
	private boolean nativeDisabled = false;
	
	public ArrayList<WindowDisplay> displays = new ArrayList<WindowDisplay>();
	
	public boolean overridePickBlock = false;
	public HitResult trueGameHitResult = null;
	
	public WLCToplevel pinnedToplevel = null;
	
	// 多人显示功能
	public RemoteWindowRenderer remoteWindowRenderer = new RemoteWindowRenderer();
	public ArrayList<SharedWindowDisplay> sharedDisplays = new ArrayList<SharedWindowDisplay>();
	public WindowShareManager windowShareManager;
	
	// 共享窗口音频：发送端捕获（PipeWire 按进程）+ 接收端播放（OpenAL）
	public AudioCaptureManager audioCaptureManager;
	public AudioPlaybackManager audioPlaybackManager = new AudioPlaybackManager();
	
	public WindowItemManager itemManager = new WindowItemManager();
	public XDGDesktopManager xdgManager;
	public PipeWireCaptureManager captureManager = new PipeWireCaptureManager();
	
	// 窗口实例别名（4 位随机，如 k7xq）
	public WindowAliasRegistry windowAliases = new WindowAliasRegistry();
	// 窗口模板（临时 + 永久）
	public WindowTemplateManager templateManager = new WindowTemplateManager();
	// 窗口自动布局（方块/圆球模板，围绕初始化坐标，默认关闭）
	public WindowLayoutManager layoutManager = new WindowLayoutManager(this);
	
	public KeyMapping keyOpenScreen;
	public KeyMapping keyCaptureKeyboard;
	// 切换鼠标隐藏（沉浸游玩；默认 H，可在按键设置改）
	public KeyMapping keyToggleCursor;
	
	public WindowInHandRenderer windowInHandRenderer = new WindowInHandRenderer();
	public WindowInItemFrameRenderer windowInItemFrameRenderer = new WindowInItemFrameRenderer();
	public WaylandHudRenderer hudRenderer = new WaylandHudRenderer(this);
	
	public PointerGrabMap pointerGrabs = new PointerGrabMap(this);
	
	// HitResult of currently hovered WindowDisplay
	// Only non-null, when no exclusive pointer grabs are currently active
	public DisplayHitResult hoveredDisplay = null;
	
	// 当前 hover 的共享窗口（手机端 viewer-only 无本地窗口时也可用；与 hoveredDisplay 互斥）
	public SharedWindowDisplay hoveredSharedDisplay = null;
	// hover 共享窗口时的窗口内像素坐标（相对窗口左上角，含 xoff/yoff）
	public double hoveredSharedX = 0;
	public double hoveredSharedY = 0;
	// 键盘捕获绑定的共享窗口（G/Alt+Q 后按键走网络转发；与 pointerCapture 对应）
	public SharedWindowDisplay sharedKeyboardCapture = null;
	// MOUSE_MOVE 转发节流：上次发送的窗口/像素/时间
	private long lastSharedMouseHandle = -1;
	private double lastSharedMouseX = Double.NaN;
	private double lastSharedMouseY = Double.NaN;
	private long lastSharedMouseSend = 0;
	
	public KeyboardCaptureMode keyboardCaptureMode = KeyboardCaptureMode.NONE;
	
	public PointerCapture pointerCapture = null;
	
	private boolean playerUsingWindowItem = false;
	private boolean playerWasUsingWindowItem = false;
	
	public @Nullable CursorShape cursorShape = null;
	
	@Override
	public void onInitializeClient() {
		WaylandCraftCommon.LOGGER.info("Initializing WaylandCraft");
		
		instance = this;
		
		keyOpenScreen = KeyMappingHelper.registerKeyMapping(new KeyMapping("waylandcraft.key.windowManager", InputConstants.Type.KEYSYM, GLFW.GLFW_KEY_B, KEYBIND_CATEGORY));
		keyCaptureKeyboard = KeyMappingHelper.registerKeyMapping(new KeyMapping("waylandcraft.key.captureKeyboard", InputConstants.Type.KEYSYM, GLFW.GLFW_KEY_G, KEYBIND_CATEGORY));
		keyToggleCursor = KeyMappingHelper.registerKeyMapping(new KeyMapping("waylandcraft.key.toggleCursor", InputConstants.Type.KEYSYM, GLFW.GLFW_KEY_H, KEYBIND_CATEGORY));
		
		LevelRenderEvents.COLLECT_SUBMITS.register(this::renderWorld);
		LevelRenderEvents.END_EXTRACTION.register(this::updateWorld);
		ClientTickEvents.END_CLIENT_TICK.register(this::onClientTick);
		ClientPlayConnectionEvents.JOIN.register(this::onClientJoin);
		ItemTooltipCallback.EVENT.register(this::addWindowItemTooltip);
		ClientTickEvents.START_CLIENT_TICK.register(itemManager);
		
		WaylandCraftCommon.instance.windowItemInteractionProvider = itemManager;
		
		WindowItemModel.register();
		hudRenderer.register();
		SharedWindowClientHandler.register();
		
		// 初始化窗口共享管理器
		windowShareManager = new WindowShareManager(this);
		audioCaptureManager = new AudioCaptureManager(this);
		WaylandCraftCommand.register();
	}
	
	/* Update bridge and clients. May be called at any state of the game, even outside of a level
	 * Called after game render in Minecraft::runTick
	 */
	public void update() {
		if(bridge == null && !nativeDisabled) {
			try {
				bridge = WaylandCraftBridge.start();
				waylandSocket = bridge.getSocket();
				xdgManager = new XDGDesktopManager(this);
				settingsManager = new WaylandCraftSettingsManager(this);
				templateManager.init(Minecraft.getInstance().gameDirectory);
				layoutManager.setEnabled(settings != null && settings.getLayoutEnabled());
				
				WaylandCraftCommon.LOGGER.info("Server started on " + waylandSocket);
			} catch (Throwable t) {
				// Native library unavailable (e.g. Android launcher with a bionic
				// runtime): disable the mod instead of crashing the game.
				nativeDisabled = true;
				WaylandCraftCommon.LOGGER.error("WaylandCraft native is unavailable, disabling mod: {}", t.toString());
				return;
			}
		}
		if(bridge == null) {
			return;
		}
		bridge.update();
		
		// 更新窗口共享（捕获+发送图像）
		if(windowShareManager != null) {
			windowShareManager.update();
		}
		
		// 更新共享窗口音频（poll native PCM + 发送）
		if(audioCaptureManager != null) {
			audioCaptureManager.tick();
		}
		
		// 更新 Portal 桌面捕获帧
		if(captureManager != null) {
			captureManager.tick();
		}
	}
	
	public void renderWorld(LevelRenderContext ctx) {
		// 本地窗口需要 bridge（native）才能渲染；共享窗口走网络层数据，
		// 不依赖本地 bridge —— Android 手机端（native 不可用）也能查看共享窗口。
		if(bridge != null) {
			displays.forEach((d) -> d.render(ctx));
		}
		
		// 渲染共享窗口（网络层数据，bridge 为 null 时也必须渲染）
		for(SharedWindowDisplay sharedDisplay : sharedDisplays) {
			sharedDisplay.clampIfResized();
			sharedDisplay.render(ctx);
		}
	}
	
	public void updateWorld(LevelExtractionContext ctx) {
		Camera camera = ctx.camera();
		// 共享窗口 hover/交互检测：不依赖 bridge —— 手机端 viewer-only（bridge==null）
		// 也要运行，否则 hoveredSharedDisplay 永远为 null，共享窗口交互全部失效。
		processPointerMotion(camera);
		
		if(bridge == null) return; // native disabled (e.g. Android launcher): stay inert, never crash
		for(WLCPopup popup : bridge.getMappedPopups()) {
			WLCAbstractWindow root = popup;
			while((root = ((WLCPopup) root).getParent()) instanceof WLCPopup);
			
			WLCToplevel toplevel = (WLCToplevel) root;
			boolean toplevelHasWindow = hasDisplayFor(toplevel);
			boolean popupHasWindow = hasDisplayFor(popup);
			if(toplevelHasWindow && !popupHasWindow) {
				getOrCreateDisplay(popup);
			}
			else if(!toplevelHasWindow && popupHasWindow) {
				displays.removeIf((w) -> w.window == popup);
			}
		}
		
		displays.removeIf((d) -> !d.isValid());
		displays.forEach((d) -> d.updateGeometry());
		
		// 窗口分辨率变化后重新执行垂直钳制（底部不低于地面 0.4 格）
		displays.forEach((d) -> d.clampIfResized());
		
		// 维护窗口实例别名（清理已消失窗口，为新窗口分配随机别名）
		HashSet<Long> aliveHandles = new HashSet<>();
		for(WLCToplevel t : bridge.getToplevels()) {
			aliveHandles.add(t.getHandle());
			windowAliases.getOrCreate(t.getHandle());
		}
		windowAliases.cleanup(aliveHandles);
		
		// 处理等待窗口出现的永久模板应用
		templateManager.tick(this);
		
		// 窗口自动布局：每 tick 围绕初始化坐标重排（默认关闭，需先 /wl layout init）
		layoutManager.tick();
		
		for(WLCPopup popup : bridge.getMappedPopups()) {
			anchorToParent(popup);
		}
		
		updateDisplayRequests();
		
		// 对所有已映射窗口幂等地尝试给物品（服务端 giveItemIfMissing 检查背包已有则不重复给）。
		// 不用 getNewToplevels()（一次性消费）：若服务端 10 tick 冷却或时序错过，
		// 新窗口的物品会永久漏发；改为每 tick 全量检查，窗口出现后总能补上。
		itemManager.giveItemsIfMissing(bridge.getMappedToplevels());
		
		boolean inWMScreen = Minecraft.getInstance().screen instanceof WindowManagerScreen;
		
		// Make sure the toplevels are focused in their respective order and being refocused when a toplevel disappears
		if(!inWMScreen) {
			WLCToplevel focus = bridge.getMostToLeastRecentFocus()
					.filter((t) -> hasDisplayFor(t))
					.findFirst()
					.orElse(null);
			
			bridge.focusSurface(focus);
		}
		
		if(Minecraft.getInstance().player == null || !Minecraft.getInstance().player.isUsingItem()) playerUsingWindowItem = false;
		if(playerUsingWindowItem) {
			ItemStack item = Minecraft.getInstance().player.getUseItem();
			if(item.is(WindowItem.WINDOW)) {
				WLCToplevel toplevel = getToplevel(item);
				
				if(toplevel != null) {
					WindowDisplay display = getOrCreateDisplay(toplevel);
					if(!playerWasUsingWindowItem) {
						display.anchorDistance = 2.0;
					}
					
					display.doGrabMove(camera.position(), new Vec3(camera.forwardVector()), new Vec3(camera.upVector()), camera.yRot());
					
					WaylandCraft.instance.bridge.focusSurface(toplevel);
				}
			}
			else playerUsingWindowItem = false;
		}
		playerWasUsingWindowItem = playerUsingWindowItem;
		
		updateOutputSize(inWMScreen);
	}
	
	public void startUsingWindowItem() {
		playerUsingWindowItem = true;
	}
	
	public void enableKeyboardCapture(boolean hardCapture) {
		if(keyboardCaptureMode != KeyboardCaptureMode.NONE) return;
		
		// 共享窗口优先：hover 共享窗口时绑定到共享窗口。
		// 手机端 viewer-only（bridge==null）本地窗口不可用，共享窗口是唯一可捕获对象。
		if(hoveredSharedDisplay != null) {
			keyboardCaptureMode = hardCapture ? KeyboardCaptureMode.HARD_CAPTURE : KeyboardCaptureMode.CAPTURE;
			sharedKeyboardCapture = hoveredSharedDisplay;
			return;
		}
		
		if(bridge == null) return; // native disabled: no bridge to activate
		
		keyboardCaptureMode = hardCapture ? KeyboardCaptureMode.HARD_CAPTURE : KeyboardCaptureMode.CAPTURE;
		bridge.activateKeyboard();
		
		// 立即绑定当前 hover 的窗口（键盘+鼠标一步到位，不用等下一次指针移动）：
		// 进入绑定 = 键盘捕获 + 鼠标锁定（视角不再移动，鼠标事件全部转发该窗口）。
		// 若玩家没 hover 窗口，鼠标仍可自由转动视角，直到 hover 到窗口才锁定。
		if(hoveredDisplay != null && hoveredDisplay.dist >= 0) {
			WLCSurface surface = hoveredDisplay.surface;
			Vec3 rel = hoveredDisplay.surfaceLocalRelative;
			if(bridge.maybeLockPointer(surface)) {
				pointerCapture = new PointerCapture(surface, rel.x, rel.y);
			}
		}
	}
	
	/**
	 * 退出键盘捕获。顺序：**先解除键盘绑定，再解除鼠标绑定**——
	 * 玩家先恢复 WASD/空格等角色控制，鼠标视角随后恢复，
	 * 避免鼠标先解锁时玩家还按着键导致视角乱转（用户要求的游戏优化）。
	 */
	public void disableKeyboardCapture() {
		if(keyboardCaptureMode == KeyboardCaptureMode.NONE) return;
		
		// 第一步：解除键盘绑定（恢复 Minecraft 角色控制）
		keyboardCaptureMode = KeyboardCaptureMode.NONE;
		sharedKeyboardCapture = null;
		// 第二步：解除鼠标绑定（恢复视角控制）
		if(bridge != null) {
			bridge.deactivateKeyboard();
			disablePointerCapture();
		}
	}
	
	public void onClientTick(Minecraft minecraft) {
		if(minecraft.player == null) return;
		
		if(keyOpenScreen.consumeClick()) {
			if(bridge == null) {
				minecraft.getChatListener().handleSystemMessage(Component.literal("WaylandCraft viewer-only mode: local window capture unavailable on this platform; you can still view shared windows"), false);
				return;
			}
			keyboardCaptureMode = KeyboardCaptureMode.NONE;
			pointerGrabs.releaseAll();
			minecraft.setScreen(new WindowManagerScreen(WaylandCraft.instance));
			return;
		}
		
		if(keyCaptureKeyboard.consumeClick()) {
			// 共享窗口 hover 时（含手机端 viewer-only）也能进入键盘捕获；无窗口可绑定时提示
			if(bridge == null && hoveredSharedDisplay == null) {
				minecraft.getChatListener().handleSystemMessage(Component.literal("WaylandCraft: 没有可捕获的共享窗口（对准一个共享窗口再按）"), false);
				return;
			}
			enableKeyboardCapture(false);
			return;
		}
		
		if(keyToggleCursor.consumeClick()) {
			if(settingsManager == null) return;
			boolean hide = !settings.getHideCursor();
			settingsManager.setBooleanSetting(WaylandCraftSettings.HIDE_CURSOR, hide);
			minecraft.getChatListener().handleSystemMessage(Component.literal("WaylandCraft: 鼠标隐藏已" + (hide ? "开启" : "关闭")), false);
			return;
		}
	}
	
	private void onClientJoin(ClientPacketListener listener, PacketSender sender, Minecraft minecraft) {
		if(bridge == null) {
			// Native library unavailable (e.g. Android launcher, Windows, macOS):
			// never pretend the compositor is running and never dereference the
			// null bridge. The mod stays in viewer-only mode — shared windows
			// (SharedWindowClientHandler) still render, capture commands no-op.
			minecraft.getChatListener().handleSystemMessage(Component.literal("WaylandCraft viewer-only mode: local window capture unavailable on this platform; you can still view shared windows"), false);
			return;
		}
		minecraft.getChatListener().handleSystemMessage(Component.literal("Wayland compositor running on " + waylandSocket), false);
		itemManager.giveItemsIfMissing(bridge.getMappedToplevels());
	}
	
	@Nullable
	public static WLCToplevel getToplevel(ItemStack item) {
		if(item == null) return null;
		if(WaylandCraft.instance == null || WaylandCraft.instance.bridge == null) return null;
		
		Long data = item.get(WindowItem.WINDOW_HANDLE);
		if(data == null) return null;
		
		long handle = data.longValue();
		return WaylandCraft.instance.bridge.getToplevel(handle);
	}
	
	private void addWindowItemTooltip(ItemStack itemStack, TooltipContext ctx, TooltipFlag flag, List<Component> list) {
		Long handle = itemStack.get(WindowItem.WINDOW_HANDLE);
		if(handle != null) {
			String text = "Handle 0x" + Long.toHexString(handle.longValue());
			Component component = Component
					.literal(text)
					.withStyle(ChatFormatting.GRAY);
			list.add(component);
		}
	}
	
	private void updateDisplayRequests() {
		// Hide all windows that were minimized and unset minimize requested state
		// 钉住的窗口（pinnedToplevel）不受 minimize 影响，保持在世界中显示
		displays.removeIf((w) -> w.window instanceof WLCToplevel && ((WLCToplevel) w.window) != pinnedToplevel && ((WLCToplevel) w.window).requests.minimize);
		Stream.of(bridge.getToplevels()).forEach((t) -> t.requests.minimize = false);
		
		// Handle any maximize or unmaximize requests
		for(WLCToplevel toplevel : bridge.getMappedToplevels()) {
			if(toplevel.requests.maximize && toplevel.requests.unmaximize) {
				// Both requests shouldn't happen at the same time
				toplevel.restoreGeometry = null;
			}
			else if(toplevel.requests.maximize) {
				// Maximize toplevel and store its old geometry
				toplevel.restoreGeometry = toplevel.geometry;
				bridge.maximizeToplevel(toplevel);
			}
			else if(toplevel.requests.unmaximize) {
				// Unmaximize toplevel and attempt to restore old geometry
				SurfaceGeometry newGeometry = toplevel.restoreGeometry;
				if(newGeometry == null) newGeometry = toplevel.geometry;
				
				// resizeToplevel also unsets the maximize flag
				bridge.resizeToplevel(toplevel, newGeometry.width(), newGeometry.height());
				toplevel.restoreGeometry = null;
			}
			
			toplevel.requests.maximize = toplevel.requests.unmaximize = false;
		}
		
		// Handle any fullscreen or unfullscreen requests
		for(WLCToplevel toplevel : bridge.getToplevels()) {
			if(toplevel.requests.fullscreen && toplevel.requests.unfullscreen) {
				// Both requests shouldn't happen at the same time
				toplevel.restoreGeometry = null;
			}
			else if(toplevel.requests.fullscreen) {
				// Fullscreen toplevel and store its old geometry
				toplevel.restoreGeometry = toplevel.geometry;
				bridge.fullscreenToplevel(toplevel);
			}
			else if(toplevel.requests.unfullscreen) {
				// Unfullscreen toplevel and attempt to restore old geometry
				SurfaceGeometry newGeometry = toplevel.restoreGeometry;
				if(newGeometry == null) newGeometry = toplevel.geometry;
				
				// resizeToplevel also unsets the fullscreen flag
				bridge.resizeToplevel(toplevel, newGeometry.width(), newGeometry.height());
				toplevel.restoreGeometry = null;
			}
			
			toplevel.requests.fullscreen = toplevel.requests.unfullscreen = false;
		}
		
		Integer moveRequest = bridge.checkMoveRequest();
		if(moveRequest != null) {
			ImplicitGrab implicit = pointerGrabs.dropImplicitMatching(moveRequest.intValue());
			if(implicit != null) {
				// The serial matched an active implicit grab
				pointerGrabs.startExclusive(new MoveGrab(implicit));
			}
		}
		
		ResizeRequest resizeRequest = bridge.checkResizeRequest();
		if(resizeRequest != null) {
			ImplicitGrab implicit = pointerGrabs.dropImplicitMatching(resizeRequest.serial());
			if(implicit != null) {
				// The serial matched an active implicit grab
				pointerGrabs.startExclusive(new ResizeGrab(implicit, resizeRequest.edges()));
			}
		}
		
		Integer dndRequest = bridge.checkDndRequest();
		if(dndRequest != null) {
			ImplicitGrab implicit = pointerGrabs.dropImplicitMatching(dndRequest);
			if(implicit != null) {
				WaylandCraftCommon.LOGGER.info("DND STARTED");
				// The serial matched an active implicit grab
				pointerGrabs.startExclusive(new DNDGrab(implicit));
			}
			else {
				// Couldn't match implicit grab, have to cancel dnd
				WaylandCraftCommon.LOGGER.info("drag and drop did not match implicit grab");
				bridge.dndCancel();
			}
		}
	}
	
	private void updateOutputSize(boolean inWMScreen) {
		int outputWidth = Minecraft.getInstance().getWindow().getWidth();
		int outputHeight = Minecraft.getInstance().getWindow().getHeight();
		
		Size size = bridge.getOutputSize();
		if(size.width() != outputWidth || size.height() != outputHeight) {
			bridge.resizeOutput(outputWidth, outputHeight);
			if(!inWMScreen) bridge.setOutputBounds(outputWidth, outputHeight);
		}
	}
	
	public @Nullable WindowDisplay getDisplay(WLCAbstractWindow window) {
		return displays.stream().filter((w) -> w.window == window).findAny().orElse(null);
	}
	
	public WindowDisplay getOrCreateDisplay(WLCAbstractWindow window) {
		WindowDisplay display = getDisplay(window);
		if(display != null) return display;
		
		display = new WindowDisplay(window);
		displays.add(display);
		
		return display;
	}
	
	public @Nullable WindowDisplay findCoreDisplay() {
		if(layoutManager == null) return null;
		long handle = layoutManager.getCoreHandle();
		for(WindowDisplay d : displays) {
			if(d.window instanceof WLCToplevel t && t.getHandle() == handle) return d;
		}
		return null;
	}
	
	public static String getWindowName(WLCToplevel toplevel) {
		return toplevel.title != null && !toplevel.title.isBlank() ? toplevel.title : "Unknown";
	}
	
	public boolean hasDisplayFor(WLCAbstractWindow window) {
		return getDisplay(window) != null;
	}
	
	public void disablePointerCapture() {
		if(pointerCapture == null) return;
		bridge.unlockPointer();
		pointerCapture = null;
	}
	
	/**
	 * 控制窗口时显示的光标。
	 * - pointerCapture 激活（窗口绑定模式）→ **强制隐藏**虚拟光标：鼠标事件全部
	 *   在绑定窗口内，沉浸游玩；被控应用自身渲染的光标在窗口画面里仍可见。
	 * - 仅 hover 未绑定 → 按 hideCursor 设置（H 键切换）决定显示窗口真实光标或隐藏。
	 * - 非窗口状态 → 返回 null（Minecraft 默认光标）。
	 */
	private CursorShape controlCursor() {
		if(pointerCapture != null) return CursorShape.HIDE;
		if(settings != null && settings.getHideCursor()) return CursorShape.HIDE;
		return bridge.getCursorShape();
	}
	
	private void processPointerMotion(Camera camera) {
		this.cursorShape = null;
		
		if(pointerCapture != null) {
			if(!pointerCapture.surface.isAlive()) {
				pointerCapture = null;
				return;
			}
			
			this.cursorShape = controlCursor();
			
			if(!bridge.maybeLockPointer(pointerCapture.surface)) {
				disablePointerCapture();
			}
			
			return;
		}
		
		// Reset hovered display and pick block override
		this.hoveredDisplay = null;
		this.hoveredSharedDisplay = null;
		this.overridePickBlock = false;
		
		if(Minecraft.getInstance().screen instanceof WindowManagerScreen) {
			return;
		}
		else if(Minecraft.getInstance().screen != null) {
			pointerGrabs.releaseAll();
			if(bridge != null) bridge.sendMotionOutside();
			return;
		}
		
		Vec3 pos = camera.position();
		Vec3 look = new Vec3(camera.forwardVector());
		Vec3 up = new Vec3(camera.upVector());
		
		DisplayHitResult finalHitResult = null;
		double finalDistance = Double.POSITIVE_INFINITY;
		for(WindowDisplay display : displays) {
			DisplayHitResult hit = display.intersect(pos, look);
			if(hit == null || hit.isMiss()) continue;
			
			double dist = hit.position.distanceToSqr(pos);
			if(finalHitResult == null || dist < finalDistance) {
				finalHitResult = hit;
				finalDistance = dist;
			}
		}
		
		// 共享窗口射线检测（viewer-only 无本地窗口时也能 hover/交互）
		SharedWindowDisplay.SharedHit finalSharedHit = null;
		double finalSharedDistance = Double.POSITIVE_INFINITY;
		for(SharedWindowDisplay sharedDisplay : sharedDisplays) {
			if(!sharedDisplay.isValid()) continue;
			SharedWindowDisplay.SharedHit hit = sharedDisplay.intersect(pos, look);
			if(hit == null || hit.dist() < 0) continue; // 只命中正面
			
			if(finalSharedHit == null || hit.dist() < finalSharedDistance) {
				finalSharedHit = hit;
				finalSharedDistance = hit.dist();
			}
		}
		
		// Check if game hit result closer
		// Must use trueGameHitResult because the game hit result is overridden by overridePickBlock
		HitResult gameHitResult = trueGameHitResult;
		double gameHitDistance = (gameHitResult == null || gameHitResult.getType() == HitResult.Type.MISS) ? Double.POSITIVE_INFINITY : gameHitResult.getLocation().distanceToSqr(pos);
		
		// 统一比较（直线距离：本地/共享命中点的 distanceToSqr == dist^2，量纲一致）：
		// 最近者胜出；共享窗口比本地窗口/方块近时，共享窗口接管 hover。
		double localDist = finalHitResult != null ? Math.sqrt(finalDistance) : Double.POSITIVE_INFINITY;
		double sharedDist = finalSharedHit != null ? finalSharedDistance : Double.POSITIVE_INFINITY;
		double gameDist = gameHitDistance == Double.POSITIVE_INFINITY ? Double.POSITIVE_INFINITY : Math.sqrt(gameHitDistance);
		
		if(sharedDist < localDist && sharedDist < gameDist) {
			// 共享窗口优先（比本地窗口和方块都近）
			finalHitResult = null;
		}
		else if(localDist < gameDist) {
			finalSharedHit = null;
		}
		else {
			finalHitResult = null;
			finalSharedHit = null;
		}
		
		// 窗口控制距离无上限：不限制玩家与窗口的交互距离（原版 blockInteractionRange 仅作用于挖矿/放方块，不作用于窗口）
		// 只要窗口正面在视线内（dist >= 0），多远都能 hover/点击/滚轮控制窗口。
		
		if(!pointerGrabs.isExclusiveGrabActive()) {
			hoveredDisplay = finalHitResult;
			hoveredSharedDisplay = finalSharedHit != null ? finalSharedHit.display() : null;
			if(hoveredSharedDisplay != null) {
				hoveredSharedX = finalSharedHit.x();
				hoveredSharedY = finalSharedHit.y();
			}
		}
		
		// Check for pointer grab and short-circuit if any
		if(pointerGrabs.isGrabActive()) {
			this.overridePickBlock = true;
			if(bridge != null) this.cursorShape = controlCursor();
			
			pointerGrabs.moveWorld(pos, look, up, camera.yRot(), camera.xRot());
			if(finalHitResult != null) {
				pointerGrabs.hover(finalHitResult.target.window, finalHitResult.surface, finalHitResult.surfaceLocalRelative.x, finalHitResult.surfaceLocalRelative.y);
			}
			else {
				pointerGrabs.hoverNone();
			}
			
			return;
		}
		
		/* All of the following code will only be executed when there aren't any active pointer grabs */
		
		if(hoveredSharedDisplay != null) {
			// 共享窗口 hover：拦截拾取，鼠标移动转发（节流，避免每帧发包）
			this.overridePickBlock = true;
			if(bridge != null) this.cursorShape = controlCursor();
			
			long now = System.currentTimeMillis();
			long handle = hoveredSharedDisplay.getWindowHandle();
			if(handle != lastSharedMouseHandle
					|| Math.abs(hoveredSharedX - lastSharedMouseX) > 0.5
					|| Math.abs(hoveredSharedY - lastSharedMouseY) > 0.5) {
				if(now - lastSharedMouseSend >= 30) {
					lastSharedMouseSend = now;
					lastSharedMouseHandle = handle;
					lastSharedMouseX = hoveredSharedX;
					lastSharedMouseY = hoveredSharedY;
					SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.MOUSE_MOVE,
							hoveredSharedX, hoveredSharedY, 0, 0);
				}
			}
			return;
		}
		
		if(hoveredDisplay != null && !canStartInteracting()) hoveredDisplay = null;
		
		if(hoveredDisplay != null) {
			this.overridePickBlock = true;
		}
		
		if(hoveredDisplay != null && hoveredDisplay.dist >= 0) {
			WLCSurface surface = hoveredDisplay.surface;
			Vec3 rel = hoveredDisplay.surfaceLocalRelative;
			
			if(bridge != null) this.cursorShape = controlCursor();
			bridge.sendMotionRefocus(surface, rel.x, rel.y);
			
			if(keyboardCaptureMode != KeyboardCaptureMode.NONE && bridge.maybeLockPointer(surface)) {
				pointerCapture = new PointerCapture(surface, rel.x, rel.y);
			}
			
			// Focus on hover
			if(settings.getFocusOnHover() && hoveredDisplay.target.window instanceof WLCToplevel toplevel) {
				bridge.focusSurface(toplevel);
			}
		}
		else if(bridge != null) {
			bridge.sendMotionOutside();
		}
	}
	
	/* Handle mouse button input
	 * Returns true when the mouse button action has been consumed
	 */
	public boolean onButtonPress(long windowHandle, int button, int action, int modifiers) {
		if(pointerCapture != null) {
			if(action == 1 && !pointerCapture.pressedButtons.contains(button)) {
				bridge.sendButton(0x110 + button, 1);
				pointerCapture.pressedButtons.add(button);
			}
			else if(action == 0 && pointerCapture.pressedButtons.contains(button)) {
				bridge.sendButton(0x110 + button, 0);
				pointerCapture.pressedButtons.remove(button);
			}
			else if(action == 0) {
				// Forward release to minecraft if it wasn't part of this pointer capture
				return false;
			}
			return true;
		}
		
		if(action == 0 && pointerGrabs.isGrabActive(button)) {
			pointerGrabs.release(button);
			return true;
		}
		
		if(pointerGrabs.isExclusiveGrabActive()) return true;
		
		// 共享窗口点击：转发到发送端注入真实窗口（X11 XTest；wayland 注入待接入）。
		// 手机端 viewer-only（bridge==null）本地窗口不可交互，共享窗口是唯一交互对象。
		if(hoveredSharedDisplay != null && canStartInteracting()) {
			long handle = hoveredSharedDisplay.getWindowHandle();
			if(action == 1) {
				SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.MOUSE_CLICK,
						hoveredSharedX, hoveredSharedY, glfwButtonToX11(button), 0);
			}
			else if(action == 0) {
				SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.MOUSE_RELEASE,
						hoveredSharedX, hoveredSharedY, glfwButtonToX11(button), 0);
			}
			return true;
		}
		
		// Handle implicit pointer grab button presses
		if(action == 1) {
			// Start new implicit grab when conditions are met
			if(!pointerGrabs.isImplicitActive() && hoveredDisplay != null && hoveredDisplay.dist >= 0) {
				pointerGrabs.startImplicit(hoveredDisplay);
				WLCAbstractWindow window = hoveredDisplay.target.window;
				if(window instanceof WLCToplevel) bridge.focusSurface((WLCToplevel) window);
			}
			
			// If an implicit pointer grab is now active, capture the button press
			if(pointerGrabs.isImplicitActive()) {
				pointerGrabs.sendImplicitButton(button);
				return true;
			}
			
			// If clicking on a window at all, the button press should be captured, even if it wasn't passed on to the application
			if(hoveredDisplay != null) return true;
		}
		
		return false;
	}
	
	private boolean canStartInteracting() {
		LocalPlayer player = Minecraft.getInstance().player;
		if(player == null) return false;
		if(player.isUsingItem()) return false;
		return true;
	}
	
	/* Handle mouse being turned in game
	 * Returns true when the mouse move has been consumed
	 */
	public boolean onMouseTurn(double dx, double dy) {
		if(pointerCapture == null) return false;
		
		bridge.sendRelativeMotion(dx, dy);
		return true;
	}
	
	/* Handle mouse scroll input
	 * Returns true when the mouse scroll action has been consumed
	 */
	public boolean onScroll(long windowHandle, double scrollX, double scrollY) {
		if(playerUsingWindowItem) {
			WLCToplevel toplevel = getToplevel(Minecraft.getInstance().player.getUseItem());
			if(toplevel != null) {
				WindowDisplay display = getDisplay(toplevel);
				if(display != null) {
					display.adjustAnchorDistance(scrollY);
					return true;
				}
			}
		}

		if(pointerGrabs.isExclusiveGrabActive()) {
			pointerGrabs.onScroll(scrollX, scrollY);
			return true;
		}
		
		// 共享窗口滚轮：转发到发送端（无修饰键时滚动内容；修饰键组合暂不支持共享窗口变换）
		if(hoveredSharedDisplay != null) {
			boolean ctrl = InputConstants.isKeyDown(Minecraft.getInstance().getWindow(), GLFW.GLFW_KEY_LEFT_CONTROL);
			boolean alt = InputConstants.isKeyDown(Minecraft.getInstance().getWindow(), GLFW.GLFW_KEY_LEFT_ALT);
			if(!ctrl && !alt) {
				// 编码：低16位 = scrollX*100，高16位 = scrollY*100（与 handleInteraction SCROLL 解码一致）
				int data = ((int) Math.round(scrollX * 100) & 0xFFFF) | (((int) Math.round(scrollY * 100) & 0xFFFF) << 16);
				SharedWindowClientHandler.sendInteraction(hoveredSharedDisplay.getWindowHandle(),
						SharedWindowInteractionPayload.InteractionType.SCROLL,
						hoveredSharedX, hoveredSharedY, data, 0);
			}
			return true;
		}
		
		// 悬停在窗口上时，修饰键+滚轮控制窗口变换
		if(hoveredDisplay != null && hoveredDisplay.dist >= 0) {
			boolean ctrl = InputConstants.isKeyDown(Minecraft.getInstance().getWindow(), GLFW.GLFW_KEY_LEFT_CONTROL);
			boolean alt = InputConstants.isKeyDown(Minecraft.getInstance().getWindow(), GLFW.GLFW_KEY_LEFT_ALT);
			
			if(ctrl || alt) {
				WLCAbstractWindow window = hoveredDisplay.target.window;
				if(window instanceof WLCToplevel) {
					WindowDisplay display = getDisplay((WLCToplevel) window);
					if(display != null) {
						if(ctrl && alt) {
							// Ctrl+Alt+滚轮 = 缩放
							display.adjustScale(scrollY);
						} else if(ctrl) {
							// Ctrl+滚轮 = 旋转
							display.rotateBy(scrollY * 0.1);
						}
						return true;
					}
				}
			}
			// 无修饰键 → 不拦截，继续执行下面的 bridge.sendScroll
		}
		
		if(hoveredDisplay != null) {
			if(hoveredDisplay.dist < 0) return true;
			
			bridge.sendScroll(0, -scrollY);
			bridge.sendScroll(1, -scrollX);
			
			WLCAbstractWindow window = hoveredDisplay.target.window;
			if(window instanceof WLCToplevel) bridge.focusSurface((WLCToplevel) window);
			
			return true;
		}
		
		return false;
	}
	
	/* Handle keyboard input
	 * Returns true when the key press action has been consumed
	 * This code just completely naively assumes that the scancode received by GLFW
	 * is also the correct matching Wayland scancode for the default XKBConfig.
	 * For X11 and Wayland hosts, this is a huge hack but should mostly work for now
	 */
	// Ctrl + 方向键：调整面前的窗口（优先 hover 的窗口，否则视线中心最近的窗口）
	public boolean onKeyPress(long windowHandle, int key, int scancode, int action, int modifiers) {
		// Ctrl + 方向键：布局启用时核心标记移动到该方向相邻窗口；未启用布局时调整面前的窗口。
		// 共享窗口坐标由发送端决定（每帧 payload 携带 pivot），接收端不移动共享窗口。
		if(action == GLFW.GLFW_PRESS && (modifiers & GLFW.GLFW_MOD_CONTROL) != 0) {
			int dir = switch(key) {
				case GLFW.GLFW_KEY_UP -> 0;
				case GLFW.GLFW_KEY_DOWN -> 1;
				case GLFW.GLFW_KEY_LEFT -> 2;
				case GLFW.GLFW_KEY_RIGHT -> 3;
				default -> -1;
			};
			if(dir >= 0) {
				WaylandCraftCommon.LOGGER.info("[move] Ctrl+方向键 dir={} layoutEnabled={} layoutInit={} localDisplays={} sharedDisplays={}",
					dir, layoutManager.isEnabled(), layoutManager.isInitialized(), displays.size(), sharedDisplays.size());
				if(layoutManager.isEnabled() && layoutManager.isInitialized()) {
					// Ctrl+方向键：核心标记移动（无聊天输出，静默切换）
					layoutManager.moveCore(dir);
					return true;
				}
				moveFrontWindow(dir);
				return true;
			}
		}
		
		if(key == GLFW.GLFW_KEY_Q && modifiers == GLFW.GLFW_MOD_ALT) {
			if(action == 0) return true;
			
			if(keyboardCaptureMode != KeyboardCaptureMode.HARD_CAPTURE) {
				enableKeyboardCapture(true);
			}
			else {
				disableKeyboardCapture();
			}
			return true;
		}
		
		if(keyboardCaptureMode == KeyboardCaptureMode.NONE) return false;
		
		if(keyboardCaptureMode == KeyboardCaptureMode.CAPTURE && key == GLFW.GLFW_KEY_ESCAPE) {
			disableKeyboardCapture();
			return true;
		}
		
		// 共享键盘捕获：按键走网络转发到发送端注入真实窗口
		if(sharedKeyboardCapture != null) {
			long handle = sharedKeyboardCapture.getWindowHandle();
			if(action == GLFW.GLFW_PRESS) {
				SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.KEY_PRESS,
						0, 0, 0, glfwKeyToKeysym(key));
			}
			else if(action == GLFW.GLFW_RELEASE) {
				SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.KEY_RELEASE,
						0, 0, 0, glfwKeyToKeysym(key));
			}
			return true;
		}
		
		if(bridge == null) return true;
		
		if(action == GLFW.GLFW_PRESS) {
			bridge.pressKey(scancode);
		}
		else if(action == GLFW.GLFW_RELEASE) {
			bridge.releaseKey(scancode);
		}
		
		return true;
	}
	
	/**
	 * GLFW keycode → X11 keysym（远端 XTest 注入用）。
	 * 覆盖字母/数字/功能键/方向键/修饰键/常用符号；未知键原样返回（可能无效但不会崩）。
	 */
	public static int glfwKeyToKeysym(int key) {
		if(key >= GLFW.GLFW_KEY_A && key <= GLFW.GLFW_KEY_Z) {
			return key + 0x20; // 'A'-'Z' (0x41-0x5A) → 'a'-'z' (0x61-0x7A)
		}
		if(key >= GLFW.GLFW_KEY_0 && key <= GLFW.GLFW_KEY_9) {
			return key; // '0'-'9' (0x30-0x39) 与 X11 数字 keysym 一致
		}
		if(key >= GLFW.GLFW_KEY_F1 && key <= GLFW.GLFW_KEY_F12) {
			return 0xFFBE + (key - GLFW.GLFW_KEY_F1); // XK_F1 = 0xFFBE
		}
		if(key >= GLFW.GLFW_KEY_KP_0 && key <= GLFW.GLFW_KEY_KP_9) {
			return 0xFFB0 + (key - GLFW.GLFW_KEY_KP_0); // XK_KP_0 = 0xFFB0
		}
		return switch(key) {
			case GLFW.GLFW_KEY_ESCAPE -> 0xFF1B;      // XK_Escape
			case GLFW.GLFW_KEY_ENTER, GLFW.GLFW_KEY_KP_ENTER -> 0xFF0D; // XK_Return
			case GLFW.GLFW_KEY_TAB -> 0xFF09;         // XK_Tab
			case GLFW.GLFW_KEY_BACKSPACE -> 0xFF08;   // XK_BackSpace
			case GLFW.GLFW_KEY_SPACE -> 0x20;         // XK_space
			case GLFW.GLFW_KEY_LEFT_SHIFT -> 0xFFE1;  // XK_Shift_L
			case GLFW.GLFW_KEY_RIGHT_SHIFT -> 0xFFE2; // XK_Shift_R
			case GLFW.GLFW_KEY_LEFT_CONTROL -> 0xFFE3; // XK_Control_L
			case GLFW.GLFW_KEY_RIGHT_CONTROL -> 0xFFE4; // XK_Control_R
			case GLFW.GLFW_KEY_LEFT_ALT -> 0xFFE9;    // XK_Alt_L
			case GLFW.GLFW_KEY_RIGHT_ALT -> 0xFFEA;   // XK_Alt_R
			case GLFW.GLFW_KEY_LEFT_SUPER -> 0xFFEB;  // XK_Super_L
			case GLFW.GLFW_KEY_RIGHT_SUPER -> 0xFFEC; // XK_Super_R
			case GLFW.GLFW_KEY_CAPS_LOCK -> 0xFFE5;   // XK_Caps_Lock
			case GLFW.GLFW_KEY_UP -> 0xFF52;          // XK_Up
			case GLFW.GLFW_KEY_DOWN -> 0xFF54;        // XK_Down
			case GLFW.GLFW_KEY_LEFT -> 0xFF51;        // XK_Left
			case GLFW.GLFW_KEY_RIGHT -> 0xFF53;       // XK_Right
			case GLFW.GLFW_KEY_HOME -> 0xFF50;        // XK_Home
			case GLFW.GLFW_KEY_END -> 0xFF57;         // XK_End
			case GLFW.GLFW_KEY_PAGE_UP -> 0xFF55;     // XK_Page_Up
			case GLFW.GLFW_KEY_PAGE_DOWN -> 0xFF56;   // XK_Page_Down
			case GLFW.GLFW_KEY_DELETE -> 0xFFFF;      // XK_Delete
			case GLFW.GLFW_KEY_INSERT -> 0xFF63;      // XK_Insert
			case GLFW.GLFW_KEY_MINUS -> 0x2D;         // '-'
			case GLFW.GLFW_KEY_EQUAL -> 0x3D;         // '='
			case GLFW.GLFW_KEY_LEFT_BRACKET -> 0x5B;  // '['
			case GLFW.GLFW_KEY_RIGHT_BRACKET -> 0x5D; // ']'
			case GLFW.GLFW_KEY_BACKSLASH -> 0x5C;     // '\'
			case GLFW.GLFW_KEY_SEMICOLON -> 0x3B;     // ';'
			case GLFW.GLFW_KEY_APOSTROPHE -> 0x27;    // '\''
			case GLFW.GLFW_KEY_GRAVE_ACCENT -> 0x60;  // '`'
			case GLFW.GLFW_KEY_COMMA -> 0x2C;         // ','
			case GLFW.GLFW_KEY_PERIOD -> 0x2E;        // '.'
			case GLFW.GLFW_KEY_SLASH -> 0x2F;         // '/'
			case GLFW.GLFW_KEY_KP_ADD -> 0xFFAB;      // XK_KP_Add
			case GLFW.GLFW_KEY_KP_SUBTRACT -> 0xFFAD; // XK_KP_Subtract
			case GLFW.GLFW_KEY_KP_MULTIPLY -> 0xFFAA; // XK_KP_Multiply
			case GLFW.GLFW_KEY_KP_DIVIDE -> 0xFFAF;   // XK_KP_Divide
			default -> key;
		};
	}
	
	/**
	 * GLFW 鼠标按钮 → X11 按钮号（XTest 用）：1=左 2=中 3=右
	 */
	public static int glfwButtonToX11(int button) {
		return switch(button) {
			case 0 -> 1;  // 左键
			case 1 -> 3;  // 右键
			case 2 -> 2;  // 中键
			default -> button + 1;
		};
	}
	
	public static int correctScancode(int scancode) {
		if(GLFW.glfwGetPlatform() == GLFW.GLFW_PLATFORM_WAYLAND) {
			scancode += 8;
		}
		return scancode;
	}
	
	/**
	 * 用 Ctrl+方向键调整"面前的窗口"位置。
	 * @param dir 0=上 1=下 2=左 3=右（以玩家视角为基准）
	 */
	private void moveFrontWindow(int dir) {
		if(settings == null) return;
		
		WindowDisplay target = null;
		if(hoveredDisplay != null && hoveredDisplay.dist >= 0) {
			target = hoveredDisplay.target;
		}
		else {
			Camera cam = Minecraft.getInstance().gameRenderer.getMainCamera();
			Vec3 pos = cam.position();
			Vec3 look = new Vec3(cam.forwardVector());
			double best = Double.POSITIVE_INFINITY;
			for(WindowDisplay d : displays) {
				DisplayHitResult hit = d.intersect(pos, look);
				if(hit == null || hit.isMiss()) continue;
				double dist = hit.position.distanceToSqr(pos);
				if(dist < best) {
					best = dist;
					target = d;
				}
			}
		}
		if(target == null) {
			return;
		}
		
		double step = settings.getMoveStep();
		if(step <= 0) step = 0.5;
		
		Vec3 look = new Vec3(Minecraft.getInstance().gameRenderer.getMainCamera().forwardVector());
		look = new Vec3(look.x, 0, look.z);
		if(look.lengthSqr() < 1e-6) look = new Vec3(0, 0, 1);
		look = look.normalize();
		Vec3 right = look.cross(new Vec3(0, 1, 0)); // 玩家右手方向（水平）
		
		Vec3 move = switch(dir) {
			case 0 -> new Vec3(0, step, 0);   // 上
			case 1 -> new Vec3(0, -step, 0);  // 下
			case 2 -> right.scale(-step);     // 左
			case 3 -> right.scale(step);      // 右
			default -> Vec3.ZERO;
		};
		
		target.pivot = target.pivot.add(move);
		target.clampVertical();
	}
	
	private void anchorToParent(WLCPopup popup) {
		WindowDisplay window = displays.stream().filter((w) -> w.window == popup).findAny().orElse(null);
		WindowDisplay parent = displays.stream().filter((w) -> w.window == popup.getParent()).findAny().orElse(null);
		
		if(window == null || parent == null) return;
		
		// If the parent is also a popup, first make it anchor itself
		if(parent.window instanceof WLCPopup) {
			anchorToParent((WLCPopup) parent.window);
		}
		
		window.rotate(parent.normal(), parent.down());
		
		int x = popup.offsetX - popup.geometry.x() + parent.window.geometry.x();
		int y = popup.offsetY - popup.geometry.y() + parent.window.geometry.y();
		
		window.moveOrigin(parent.localToWorld(x, y, 0.01));
	}
	
	public static enum KeyboardCaptureMode {
		
		NONE, CAPTURE, HARD_CAPTURE;
		
	}
	
	public static class PointerCapture {
		
		public final WLCSurface surface;
		
		// Pointer capture entry surface-local coordinates
		public double x;
		public double y;
		
		public HashSet<Integer> pressedButtons = new HashSet<Integer>();
		
		public PointerCapture(WLCSurface surface, double x, double y) {
			this.surface = surface;
			this.x = x;
			this.y = y;
		}
		
	}
	
}

