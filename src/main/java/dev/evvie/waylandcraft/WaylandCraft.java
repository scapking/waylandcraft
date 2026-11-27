package dev.evvie.waylandcraft;

import java.io.File;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
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
import dev.evvie.waylandcraft.localmode.LocalDirectDisplay;
import dev.evvie.waylandcraft.localmode.LocalModePolicy;
import dev.evvie.waylandcraft.localmode.LocalModePolicy.WindowDisplayMode;
import dev.evvie.waylandcraft.placement.WindowPlacementMemory;
import dev.evvie.waylandcraft.item.WindowHandle;
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
import net.minecraft.client.multiplayer.ServerData;
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
	
	/** ⑥ /wl status：native 是否因加载失败被整体禁用 */
	public boolean isNativeDisabled() {
		return nativeDisabled;
	}
	
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

	// fix/app-launcher-lifecycle：单实例应用生命周期（陈旧检测/交接诊断/收养终止）
	public final dev.evvie.waylandcraft.appsession.AppLifecycle appLifecycle =
			new dev.evvie.waylandcraft.appsession.AppLifecycle(this);
	
	// HitResult of currently hovered WindowDisplay
	// Only non-null, when no exclusive pointer grabs are currently active
	public DisplayHitResult hoveredDisplay = null;
	
	// 诊断：本地键盘首次转发已打日志（避免每键刷屏）
	private boolean keyboardForwardLogged = false;
	// 诊断：tick 焦点日志降噪（窗口变化或 ≥5s 才打一次）
	private String lastFocusLogTitle = "";
	private long lastFocusLogTime = 0;
	
	// 当前 hover 的共享窗口（手机端 viewer-only 无本地窗口时也可用；与 hoveredDisplay 互斥）
	public SharedWindowDisplay hoveredSharedDisplay = null;
	// hover 共享窗口时的窗口内像素坐标（相对窗口左上角，含 xoff/yoff）
	public double hoveredSharedX = 0;
	public double hoveredSharedY = 0;
	// 键盘捕获绑定的共享窗口（G/J 后按键走网络转发；与 pointerCapture 对应）
	public SharedWindowDisplay sharedKeyboardCapture = null;
	// 共享窗口"游戏模式"指针捕获：J 绑定共享窗口后，鼠标事件全部转发该窗口（即使不 hover）
	public SharedPointerCapture sharedPointerCapture = null;
	// 共享窗口键盘转发配对跟踪：press 记入、release 移除；hover/绑定窗口切换或退出绑定时
	// 向旧窗口补发 release——防止远端窗口一直维持 Shift/CapsLock 等按键状态（"卡住大小写"）
	private long lastKeyForwardHandle = -1;
	private final java.util.HashSet<Integer> pressedForwardKeys = new java.util.HashSet<>();
	// 已发送给共享窗口的修饰键掩码（GLFW_MOD_*）：转发普通键前把远端修饰键状态对齐到
	// 本次事件的 modifiers，保证 Ctrl/Shift/Alt 组合键在远端生效（GLFW 漏发修饰键事件时也能兜底）
	private int forwardedMods = 0;
	// MOUSE_MOVE 转发节流：上次发送的窗口/像素/时间
	private long lastSharedMouseHandle = -1;
	private double lastSharedMouseX = Double.NaN;
	private double lastSharedMouseY = Double.NaN;
	private long lastSharedMouseSend = 0;
	/** v1.4.6：hover 全 miss 诊断行的 2s 节流时间戳。 */
	private long lastHoverDiagMs = 0;

	// v0.13.4：状态日志——一个文件包含所有子系统状态（覆盖式）
	// 写入：每 STATUS_REFRESH_INTERVAL_MS 刷一次；初始化时立即刷一次。
	// 文件路径：<gameDir>/waylandcraft/status.log
	// v0.13.5 fallback：<gameDir>/waylandcraft-status.log（兼容老 user 没 waylandcraft/ 子目录）
	private File statusLogFile = null;
	private long lastStatusRefreshMs = 0;
	private static final long STATUS_REFRESH_INTERVAL_MS = 30_000; // 30 秒
	
	public KeyboardCaptureMode keyboardCaptureMode = KeyboardCaptureMode.NONE;
	
	public PointerCapture pointerCapture = null;
	
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
		ClientPlayConnectionEvents.DISCONNECT.register(this::onClientDisconnect);
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
	
	// upstream 7f1de7b: 设置变更响应器注册（后续 f23fdcb 接入终端启动链路）
	private void registerSettingsResponders() {
		settingsManager.registerResponder(WaylandCraftSettings.TERMINAL_CHOICE, (value) -> {
			// fork 惯例：新 native 入口包 try/catch（旧版 .so 缺符号时不致命，见 setAudioLogFile 等）
			try {
				bridge.setPreferredTerminal((String) value);
			} catch (Throwable t) {
				WaylandCraftCommon.LOGGER.warn("[terminal] setPreferredTerminal failed: {}", t.toString());
			}
		});
		// v0.14：嵌套输入法路由开关 imeNested（auto|on|off）。responder 在
		// 注册时立即触发一次，故这同时是启动期同步；后续 /wl settings set
		// 或 GUI 改动都会重新下发。隔离 HOME=gameDir/waylandcraft/ime-nested。
		settingsManager.registerResponder(WaylandCraftSettings.IME_NESTED, (value) -> {
			try {
				String home = new File(Minecraft.getInstance().gameDirectory,
						"waylandcraft/ime-nested").getAbsolutePath();
				bridge.setImeNested((String) value, home);
			} catch (Throwable t) {
				WaylandCraftCommon.LOGGER.warn("[ime][nested] setImeNested failed: {}", t.toString());
			}
		});
		// 内置引擎来源档 imeSource（auto|fcitx5|builtin）：responder 注册即
		// 触发启动期同步；Rust 侧校验非法值（不改状态、进 ime.log）。
		settingsManager.registerResponder(WaylandCraftSettings.IME_SOURCE, (value) -> {
			try {
				bridge.setImeSource((String) value);
			} catch (Throwable t) {
				WaylandCraftCommon.LOGGER.warn("[ime][source] setImeSource failed: {}", t.toString());
			}
		});
		// 日志分级 verboseLogging：responder 注册即触发启动期同步（Java 生效档
		// = 系统属性覆盖 > settings；WcLogging.applySetting 内部下发 Rust，
		// native 缺失静默）。分级表见 docs/LOGGING.md。
		settingsManager.registerResponder(WaylandCraftSettings.VERBOSE_LOGGING, (value) -> {
			try {
				dev.evvie.waylandcraft.utils.WcLogging.applySetting(
					value instanceof Boolean b ? b : true);
			} catch (Throwable t) {
				WaylandCraftCommon.LOGGER.warn("[logging] verboseLogging responder failed: {}", t.toString());
			}
		});
	}

	/* Update bridge and clients. May be called at any state of the game, even outside of a level
	 * Called after game render in Minecraft::runTick
	 */
	public void update() {
		if(bridge == null && !nativeDisabled) {
			try {
				// Rust 侧 [system_ime] 日志文件必须在 native start()（含 SystemIme 初始化）
				// 之前设置，否则 BUILD/PHASE 初始化日志进不了文件（eprintln 只进 stderr）。
				try {
					WaylandCraftBridge.setImeLogFileStatic(new File(Minecraft.getInstance().gameDirectory, "waylandcraft-ime.log").getAbsolutePath());
				} catch (Throwable t) {
					WaylandCraftCommon.LOGGER.warn("[system_ime] setImeLogFileStatic failed: {}", t.toString());
				}
				bridge = WaylandCraftBridge.start();
				waylandSocket = bridge.getSocket();
				// 进程退出也要终止本会话收养的应用树（hook 只装一次；native 升级 kill
				// 在 hook 线程内完成，防止 JVM 先死导致 SIGTERM 无后续）
				dev.evvie.waylandcraft.appsession.AppLifecycle.installShutdownHookOnce();
				// Rust 侧 [kb-debug] 日志写入独立文件（eprintln 只进 stderr，latest.log 看不到）。
				// 用户上传 waylandcraft-kb.log 即可定位 Rust 键盘链路（收到/焦点/发出）。
				try {
					bridge.setKbLogFile(new File(Minecraft.getInstance().gameDirectory, "waylandcraft-kb.log").getAbsolutePath());
				} catch (Throwable t) {
					WaylandCraftCommon.LOGGER.warn("[kb] setKbLogFile failed: {}", t.toString());
				}
				// Rust 侧 [audio] 全链路日志写入独立文件（PID→拓扑→节点→link→回调）。
				try {
					bridge.setAudioLogFile(new File(Minecraft.getInstance().gameDirectory, "waylandcraft-audio.log").getAbsolutePath());
				} catch (Throwable t) {
					WaylandCraftCommon.LOGGER.warn("[audio] setAudioLogFile failed: {}", t.toString());
				}
				// Rust 侧 [system_ime] 全链路日志写入独立文件
				// （probe→connect→registry→enter→enable→commit→错误）。
				try {
					bridge.setImeLogFile(new File(Minecraft.getInstance().gameDirectory, "waylandcraft-ime.log").getAbsolutePath());
				} catch (Throwable t) {
					WaylandCraftCommon.LOGGER.warn("[system_ime] setImeLogFile failed: {}", t.toString());
				}
				xdgManager = new XDGDesktopManager(this);
				settingsManager = new WaylandCraftSettingsManager(this);
				registerSettingsResponders();
				templateManager.init(Minecraft.getInstance().gameDirectory);
				layoutManager.setEnabled(settings != null && settings.getLayoutEnabled());
				
				WaylandCraftCommon.LOGGER.info("Server started on " + waylandSocket);
			} catch (Throwable t) {
				// Native library unavailable (e.g. Android launcher with a bionic
				// runtime): disable the mod instead of crashing the game.
				// P2-11：start() 成功但后续步骤（getSocket/setXxxLogFile/XDG 初始化）
				// 抛异常时必须把 bridge 置回 null——否则进入"bridge 非 null 但
				// nativeDisabled=true"的半死状态：update() 每 tick 继续对可能只初始化
				// 了一半的 native 实例调 bridge.update()，其他 null 检查路径全部失真。
				bridge = null;
				nativeDisabled = true;
				WaylandCraftCommon.LOGGER.error("WaylandCraft native is unavailable, disabling mod: {}", t.toString());
				return;
			}
		}
		if(bridge == null) {
			return;
		}
		// v0.12.4 修：bridge.update() 内部调多个 native 方法（update / updateSurfaceTree /
		// updateSurfaceData 等），native 抛 RuntimeException 会穿透到 MC tick 致命崩溃。
		// 现实：wayland 异步模型下 surface 在两次 native 调用间暴毙（关窗口）是合法事件，
		// 不应该炸游戏。这里包一层 try/catch：失败仅本帧跳过更新，**不**致命。
		// 详见 bridge.update() 内部的细粒度 try/catch。
		try {
			bridge.update();
		} catch (RuntimeException e) {
			WaylandCraftCommon.LOGGER.error("[waylandcraft] bridge.update failed: {}", e.toString());
		}

		// 更新窗口共享（捕获+发送图像）
		if(windowShareManager != null) {
			try {
				windowShareManager.update();
			} catch (RuntimeException e) {
				WaylandCraftCommon.LOGGER.error("[waylandcraft] windowShareManager.update failed: {}", e.toString());
			}
		}
		
		// 更新共享窗口音频（poll native PCM + 发送）
		if(audioCaptureManager != null) {
			audioCaptureManager.tick();
		}
		
		// 更新 Portal 桌面捕获帧
		if(captureManager != null) {
			captureManager.tick();
		}

		// v0.13.4：状态日志——每 30s 刷一次 status.log（覆盖式）
		refreshStatusLogIfNeeded();
	}

	/**
	 * v0.13.4：把当前 native + Java 子系统状态写入覆盖式 status.log。
	 * 一个文件包含全部子系统状态（native lib / egl / host_bridge /
	 * ime_ti3 / xwayland-satellite / audio / portal）——排查问题不必再
	 * 切 4 个独立日志。
	 * <p>每 {@link #STATUS_REFRESH_INTERVAL_MS} 刷一次；第一次调用时
	 * （bridge 刚初始化后）立即刷。
	 * <p>写入失败不抛异常——日志写不出不能影响游戏运行。
	 */
	private void refreshStatusLogIfNeeded() {
		if(bridge == null) return;
		long now = System.currentTimeMillis();
		boolean firstTime = (statusLogFile == null);
		if(!firstTime && (now - lastStatusRefreshMs) < STATUS_REFRESH_INTERVAL_MS) return;
		lastStatusRefreshMs = now;
		if(statusLogFile == null) {
			// v0.13.5：fallback 路径——优先 waylandcraft/status.log，失败回退到 gameDir 根。
			// 一些老 user 没 waylandcraft/ 子目录权限，FileWriter 写不进。
			File primary = new File(Minecraft.getInstance().gameDirectory, "waylandcraft/status.log");
			File fallback = new File(Minecraft.getInstance().gameDirectory, "waylandcraft-status.log");
			statusLogFile = primary.canWrite() || canCreateParentDir(primary) ? primary : fallback;
		}
		try {
			String json = bridge.getStatusReport(Thread.currentThread().getName());
			// v0.13.5：写前确保父目录存在（防御 gameDir 没 waylandcraft/ 子目录）。
			File parent = statusLogFile.getParentFile();
			if(parent != null && !parent.exists()) parent.mkdirs();
			// 覆盖式写入：FileWriter(file, false) = 不 append，直接覆盖。
			try (java.io.FileWriter w = new java.io.FileWriter(statusLogFile, false)) {
				w.write(json);
			}
		} catch (Throwable t) {
			// 日志写不出不影响游戏——但用最高级日志（ERROR）以便 grep
			WaylandCraftCommon.LOGGER.error("[waylandcraft] status.log write failed: {}", t.toString());
		}
	}

	/** v0.13.5：检查能否创建给定文件的父目录（写权限 + 可创建）。 */
	private static boolean canCreateParentDir(File f) {
		File parent = f.getParentFile();
		if(parent == null) return false;
		if(parent.exists()) return parent.canWrite();
		// 父目录不存在——试创建
		try {
			return parent.mkdirs() || parent.exists();
		} catch (SecurityException e) {
			return false;
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

		// sync14：IME 候选窗世界形态（键盘捕获往应用窗口打字时贴在窗口旁的
		// 候选 quad）。放在窗口之后提交：同深度序下后画在上层，且候选 quad
		// 自身沿窗口法线向相机抬升——候选永远盖在窗口内容上。内部自带全套门
		// 判定（未显示即空操作），bridge==null 时恒静默。
		dev.evvie.waylandcraft.ime.ImeCandidateWorld.render(ctx);
	}
	
	// P2-12：本 tick 的 toplevel 列表快照（updateWorld 帧首失效，帧内复用）
	private WLCToplevel[] tickToplevels = null;
	private WLCToplevel[] tickMappedToplevels = null;

	private WLCToplevel[] toplevelsThisTick() {
		if(tickToplevels == null) tickToplevels = bridge.getToplevels();
		return tickToplevels;
	}

	private WLCToplevel[] mappedToplevelsThisTick() {
		if(tickMappedToplevels == null) tickMappedToplevels = bridge.getMappedToplevels();
		return tickMappedToplevels;
	}

	public void updateWorld(LevelExtractionContext ctx) {
		Camera camera = ctx.camera();
		// 共享窗口 hover/交互检测：不依赖 bridge —— 手机端 viewer-only（bridge==null）
		// 也要运行，否则 hoveredSharedDisplay 永远为 null，共享窗口交互全部失效。
		processPointerMotion(camera);
		
		if(bridge == null) return; // native disabled (e.g. Android launcher): stay inert, never crash
		// P2-12：tick 级快照失效点。updateWorld → updateDisplayRequests → giveItemsIfMissing
		// 一帧内多处 getToplevels()/getMappedToplevels()——后者对**每个** toplevel 调一次
		// isMapped()（JNI 跨界×N×多次/tick）。帧首失效、本帧首次使用时建立，之后复用。
		tickToplevels = null;
		tickMappedToplevels = null;
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
		for(WLCToplevel t : toplevelsThisTick()) {
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
		// ⑩ 服务端无 mod 时物品体系整体不存在（give 被静默丢弃）：改走本地直显——
		// 不再依赖"手持物品"路径，为新 toplevel 直接建 display 并以初始姿态摆放，
		// 窗口照样可见/可点/可拖（拖拽走 PointerGrabMap 世界射线，本就纯客户端）。
		if(effectiveDisplayMode() == WindowDisplayMode.ITEM) {
			if(serverHasMod) {
				// 标准档：payload 仅在服务端真有 mod 时发送（反作弊红线：客户端零背包写入）
				itemManager.giveItemsIfMissing(mappedToplevelsThisTick());
			}
			else {
				// 显式 items 档（无 mod、无反作弊的可信服）：客户端自维护物品，
				// 服务端每次背包回同步后由本循环补回——风险与语义见 LocalModePolicy。
				itemManager.giveItemsLocallyIfMissing(mappedToplevelsThisTick());
			}
		}
		else {
			// 本地直显。目标判定放宽为"存活 且（mapped 或 Java 层已有 framebuffer）"
			// 且必须用 toplevelsThisTick 全量枚举——旧实现只看 isMapped 单信号
			// （缺陷 C：buffer attach 事件丢失/协商噪声时窗口恒不显示且无报错）。
			for(WLCToplevel t : toplevelsThisTick()) {
				LocalDirectDisplay.DisplayTarget target = new LocalDirectDisplay.DisplayTarget(
						t.getHandle(),
						t.isAlive(),
						isMappedSafe(t),
						framebufferReady(t),
						hasDisplayFor(t),
						localHiddenHandles.contains(t.getHandle()));
				if(!LocalDirectDisplay.shouldCreateDisplay(target)) continue;

				WindowDisplay d = getOrCreateDisplay(t);
				placeNewLocalDisplay(d, t, camera);
			}
		}
		
		boolean inWMScreen = Minecraft.getInstance().screen instanceof WindowManagerScreen;
		
		// Make sure the toplevels are focused in their respective order and being refocused when a toplevel disappears
		if(!inWMScreen) {
			// 绑定模式下焦点跟随 hover 的窗口（用户鼠标指向哪个窗口，键盘就进哪个窗口）；
			// 未绑定才按"最近焦点"排序。之前无条件用最近焦点 —— 如果 hover 窗口不是最近焦点，
			// 每 tick 会把焦点从 hover 窗口抢走/或 focus 为 null 时清掉焦点 → 按键进不去窗口。
			if(keyboardCaptureMode != KeyboardCaptureMode.NONE && hoveredSharedDisplay == null) {
				ensureKeyboardFocus("tick");
			} else {
				WLCToplevel focus = bridge.getMostToLeastRecentFocus()
						.filter((t) -> hasDisplayFor(t))
						.findFirst()
						.orElse(null);
				
				bridge.focusSurface(focus);
			}
		}
		
		ItemStack useItem;
		if((useItem = getPlayerUseWindowItem()) != null) {
			WLCToplevel toplevel = getToplevel(useItem);
			
			if(toplevel != null) {
				WindowDisplay display = getOrCreateDisplay(toplevel);
				if(!playerWasUsingWindowItem) {
					display.anchorDistance = 2.0;
				}
				
				display.doGrabMove(camera.position(), new Vec3(camera.forwardVector()), new Vec3(camera.upVector()), camera.yRot());
				
				WaylandCraft.instance.bridge.focusSurface(toplevel);
			}
		}
		playerWasUsingWindowItem = useItem != null;
		
		updateOutputSize(inWMScreen);
	}
	
	/** isMapped() 在 surface 尚未挂上的极早期帧会 NPE；判活 + 判 surface 后再问。 */
	private static boolean isMappedSafe(WLCToplevel t) {
		return t.isAlive() && t.getSurfaceTree() != null && t.isMapped();
	}

	/** Java 渲染层已有真实尺寸 framebuffer：buffer attach 丢失时的兜底可显示信号。 */
	private static boolean framebufferReady(WLCToplevel t) {
		return t.isAlive() && t.framebuffer != null && t.geometry != null
				&& t.geometry.width() > 0 && t.geometry.height() > 0;
	}

	/**
	 * 新建本地直显显示的首帧姿态（需求 A：布局优先）。
	 *  - 布局开启且该窗口参与布局 → 摆到布局中心，下一 tick 由 WindowLayoutManager
	 *    精排到槽位（与物品窗口同一条布局路径；/wl place 只是临时偏移）。
	 *  - 布局不活跃但有按服记忆 → 直接回位。
	 *  - 否则玩家面前默认距离（旧行为）。
	 */
	private void placeNewLocalDisplay(WindowDisplay d, WLCToplevel t, Camera camera) {
		boolean layoutActive = layoutManager != null && layoutManager.isEnabled() && layoutManager.isInitialized()
				&& (settings == null || settings.getLayoutAutoJoin() || layoutManager.containsHandle(t.getHandle()));
		WindowPlacementMemory.Placement remembered = layoutActive ? null : lookupRememberedPlacement(t);

		switch(LocalDirectDisplay.chooseInitialPose(layoutActive, remembered != null)) {
			case LAYOUT -> {
				d.pivot = layoutManager.centerPos();
				d.rotateToYawDegrees(settings.getLayoutInitYaw());
				d.clampVertical();
			}
			case REMEMBERED -> {
				d.pivot = new Vec3(remembered.x(), remembered.y(), remembered.z());
				d.rotateToYawDegrees(remembered.yawDeg());
				d.anchorDistance = LocalDirectDisplay.clampPlaceDistance(remembered.distance());
			}
			case CAMERA_FRONT -> {
				d.anchorDistance = LocalDirectDisplay.DEFAULT_PLACE_DISTANCE;
				d.doGrabMove(camera.position(), new Vec3(camera.forwardVector()), new Vec3(camera.upVector()), camera.yRot());
			}
		}
	}

	public @Nullable ItemStack getPlayerUseWindowItem() {
		LocalPlayer player = Minecraft.getInstance().player;
		if(player == null) return null;
		
		ItemStack useItem = player.getUseItem();
		if(useItem == null) return null;
		
		return useItem.getItem() instanceof WindowItem ? useItem : null;
	}
	
	/**
	 * 键盘捕获绑定（G=纯键盘，J=键盘+鼠标分工）：
	 *  - hardCapture=true（J / HARD_CAPTURE）：现状不变——键盘 + 鼠标锁定
	 *    （PointerCapture / SharedPointerCapture 都建，鼠标事件全部转发窗口）。
	 *  - hardCapture=false（G / CAPTURE）：只绑键盘——只 activateKeyboard() +
	 *    设 keyboardCaptureMode=CAPTURE + sharedKeyboardCapture（共享窗口 hover 时），
	 *    **不创建 PointerCapture / SharedPointerCapture**，鼠标继续自由转视角。
	 */
	public void enableKeyboardCapture(boolean hardCapture) {
		if(keyboardCaptureMode != KeyboardCaptureMode.NONE) return;

		// 上游 3822b75：进入捕获前释放所有按住的 MC 键位。捕获期间按键释放事件被
		// 转发给窗口、不再回到 KeyMapping，若不 releaseAll，进捕获前按住的键会卡住。
		KeyMapping.releaseAll();

		WaylandCraftCommon.LOGGER.info("[kb-debug] enableKeyboardCapture hardCapture={} hoveredShared={} hoveredLocal={} bridge={}",
			hardCapture,
			hoveredSharedDisplay != null ? "yes" : "no",
			hoveredDisplay != null ? (hoveredDisplay.dist >= 0 ? "hit" : "miss") : "none",
			bridge != null ? "yes" : "no");
		
		// 共享窗口优先：hover 共享窗口时绑定到共享窗口。
		// 手机端 viewer-only（bridge==null）本地窗口不可用，共享窗口是唯一可捕获对象。
		// G（CAPTURE）只设 sharedKeyboardCapture，不设 sharedPointerCapture。
		if(hoveredSharedDisplay != null) {
			keyboardCaptureMode = hardCapture ? KeyboardCaptureMode.HARD_CAPTURE : KeyboardCaptureMode.CAPTURE;
			sharedKeyboardCapture = hoveredSharedDisplay;
			if(hardCapture) {
				sharedPointerCapture = new SharedPointerCapture(
					hoveredSharedDisplay.getWindowHandle(), hoveredSharedDisplay, hoveredSharedX, hoveredSharedY);
			}
			return;
		}
		
		if(bridge == null) return; // native disabled: no bridge to activate
		
		keyboardCaptureMode = hardCapture ? KeyboardCaptureMode.HARD_CAPTURE : KeyboardCaptureMode.CAPTURE;
		bridge.activateKeyboard();
		
		// 关键：绑定后立即把键盘焦点给当前 hover 的本地窗口（或最近焦点窗口）。
		// Rust keyboard_key 只有在有 surface 获得 focus 时才转发按键（data.focus.is_some()），
		// 之前 G 键只 activateKeyboard() 不设焦点 → 所有按键被 Rust 丢弃 → 键盘绑定形同虚设。
		// J 键之前"看起来能用"是因为鼠标 hover 路径（onMouseTurn）会顺手 focusSurface。
		ensureKeyboardFocus("enableKeyboardCapture");
		
		// 仅 hardCapture（J）才创建 PointerCapture 锁定鼠标。
		// 关键修复：不再用 bridge.maybeLockPointer 作为创建 pointerCapture 的前提——
		// 那要求被控应用自己申请过 zwp_pointer_constraints 指针锁（浏览器/桌面应用都不会），
		// 导致 J 绑定后鼠标事件依然落到 Minecraft。现在一律创建捕获：
		//   应用已申请锁 → relativeLocked，用相对移动；
		//   未申请（浏览器）→ 绝对虚拟光标（onMouseTurn 里 sendMotionRefocus 移动窗口内光标）。
		// G（CAPTURE）不创建 pointerCapture：鼠标仍自由转动视角，直到 hover 窗口才交互。
		if(hardCapture && hoveredDisplay != null && hoveredDisplay.dist >= 0) {
			WLCSurface surface = hoveredDisplay.surface;
			Vec3 rel = hoveredDisplay.surfaceLocalRelative;
			PointerCapture pc = new PointerCapture(surface, rel.x, rel.y);
			pc.relativeLocked = bridge.maybeLockPointer(surface);
			pointerCapture = pc;
		}
	}
	
	/**
	 * 确保本地键盘焦点存在且正确。优先级：
	 *   1. 当前 hover 的本地窗口（绑定模式下焦点跟随鼠标指向的窗口）；
	 *   2. 最近焦点窗口（有对应 display 的）；
	 *   3. 兜底：displays 里第一个 toplevel —— 保证绑定后**必有**焦点。
	 * Rust keyboard_key 只有在某 wl_keyboard 有 focus 时才转发按键；
	 * 之前 G 键如果既没 hover 窗口、最近焦点列表又为空 → kbFocus == null →
	 * focusSurface 不调用 → 按键全被 Rust 丢弃 → "键盘输入不能穿透窗口"。
	 * focusSurface 是幂等的（Rust 侧 "Surface already focused" 短路），每帧调用成本极低。
	 */
	private void ensureKeyboardFocus(String origin) {
		if(bridge == null) return;
		WLCToplevel kbFocus = null;
		String source = "none";
		if(hoveredDisplay != null && hoveredDisplay.dist >= 0 && hoveredDisplay.target.window instanceof WLCToplevel t) {
			kbFocus = t;
			source = "hover";
		}
		else {
			kbFocus = bridge.getMostToLeastRecentFocus()
					.filter((t) -> hasDisplayFor(t))
					.findFirst()
					.orElse(null);
			if(kbFocus != null) source = "recent";
		}
		if(kbFocus == null) {
			for(WindowDisplay d : displays) {
				if(d.window instanceof WLCToplevel t) {
					kbFocus = t;
					source = "fallback";
					break;
				}
			}
		}
		if(kbFocus != null) {
			bridge.focusSurface(kbFocus);
			// 日志降噪：tick 每帧调用，只在"焦点窗口变化"或"距上次 ≥5 秒"时打一次，
			// 避免每秒刷 20 行淹没按键日志；onKeyPress 入口不打（高频）。
			if(!"onKeyPress".equals(origin)) {
				String title = WaylandCraft.getWindowName(kbFocus);
				long now = System.currentTimeMillis();
				boolean changed = !title.equals(lastFocusLogTitle);
				if(changed || now - lastFocusLogTime > 5000) {
					lastFocusLogTitle = title;
					lastFocusLogTime = now;
					WaylandCraftCommon.LOGGER.info("[kb] {} 焦点={} (来源={}, hovered={}, displays={})",
						origin, title, source,
						hoveredDisplay != null ? "yes" : "no", displays.size());
				}
			}
		} else {
			WaylandCraftCommon.LOGGER.warn("[kb] {} 无任何可聚焦窗口（displays={} toplevels={}）——按键将全部被 Rust 丢弃",
				origin, displays.size(), bridge.getToplevels().length);
		}
	}
	
	/**
	 * sync14 世界形态候选窗的目标窗口：与 ensureKeyboardFocus **同一条优先链**
	 * （hover 的本地窗口 → 最近焦点窗口），但不做"displays 第一个"兜底——
	 * 候选窗锚错窗口比不显示更糟。native 的 ti3 光标矩形/候选帧都源自当前
	 * 聚焦会话，与此处按键入 focusSurface 的窗口一致。null = 无有效目标。
	 */
	public @Nullable WindowDisplay imeWorldTargetDisplay() {
		if(hoveredDisplay != null && hoveredDisplay.dist >= 0 && hoveredDisplay.target.window instanceof WLCToplevel t) {
			WindowDisplay d = getDisplay(t);
			if(d != null && d.isValid()) return d;
		}
		if(bridge == null) return null;
		WLCToplevel recent = bridge.getMostToLeastRecentFocus()
				.filter((w) -> hasDisplayFor(w))
				.findFirst()
				.orElse(null);
		if(recent == null) return null;
		WindowDisplay d = getDisplay(recent);
		return d != null && d.isValid() ? d : null;
	}

	/**
	 * surface 所属的窗口对象（沿 surface 树上溯到根，再匹配各 display 的树根）。
	 * HARD_CAPTURE 候选窗命中判定用：虚拟光标在 surface 局部，需知道它属于
	 * 哪个窗口才能换算到该窗口的平面局部像素。无匹配 → null。
	 */
	public @Nullable WLCAbstractWindow windowOfSurface(WLCSurface s) {
		if(s == null) return null;
		WLCSurface root = s;
		while(root.getParent() != null) root = root.getParent();
		for(WindowDisplay d : displays) {
			if(d.window.getSurfaceTree() == root) return d.window;
		}
		return null;
	}

	/**
	 * 退出键盘捕获。顺序：**先解除键盘绑定，再解除鼠标绑定**——
	 * 玩家先恢复 WASD/空格等角色控制，鼠标视角随后恢复，
	 * 避免鼠标先解锁时玩家还按着键导致视角乱转（用户要求的游戏优化）。
	 */
	public void disableKeyboardCapture() {
		if(keyboardCaptureMode == KeyboardCaptureMode.NONE) return;
		
		WaylandCraftCommon.LOGGER.info("[kb-debug] disableKeyboardCapture mode={} sharedCap={}",
			keyboardCaptureMode, sharedKeyboardCapture != null ? "yes" : "no");
		
		// 第零步：补发所有仍按住的共享键 release（防止退出绑定后远端窗口卡 Shift/CapsLock）
		long fwd = sharedKeyboardCapture != null ? sharedKeyboardCapture.getWindowHandle() : -1;
		if(fwd < 0 && hoveredSharedDisplay != null) fwd = hoveredSharedDisplay.getWindowHandle();
		releaseAllForwardedKeys(fwd);
		
		// 第一步：解除键盘绑定（恢复 Minecraft 角色控制）
		keyboardCaptureMode = KeyboardCaptureMode.NONE;
		sharedKeyboardCapture = null;
		sharedPointerCapture = null;
		forwardedMods = 0;
		// 第二步：解除鼠标绑定（恢复视角控制）
		if(bridge != null) {
			bridge.deactivateKeyboard();
			disablePointerCapture();
		}
	}
	
	public void onClientTick(Minecraft minecraft) {
		if(minecraft.player == null) return;
		updateServerModCapability();
		tickGiveVerification();
		appLifecycle.tick();
		// v1.4.7 光影兼容：刷新一帧补偿系数（内部对 Iris API 探测 2s 节流，
		// settings 为 null/未装 Iris 恒恒等，零成本）。渲染侧读 current() 无锁。
		dev.evvie.waylandcraft.render.ShaderCompatPolicy.refresh(
			settings != null ? settings.getShaderCompat() : "auto");
		checkKeybinds(minecraft);
	}
	
	// ===== ⑩ 服务端能力检测 =====
	// 窗口物品的"存在性"（物品注册、give、背包持有、失效清理）全部锚定在服务端
	// ServerItemManager + ServerPlayerMixin + 静态注册表；vanilla 服务端对未知
	// payload 静默丢弃、对未知物品 ID 直接拒绝，客户端收不到任何错误。
	// 唯一可靠的探测是 Fabric 的通道注册可见性：服务端装了 mod 才会注册
	// ServerboundGiveItemsPayload 的接收通道，canSend 才为 true。
	// 探测结果驱动运行模式：true=标准模式（物品体系），false=本地直显模式。
	public volatile boolean serverHasMod = true;
	private int serverModCheckCountdown = 0;

	/**
	 * 运行期强制本地直显（C 自愈）：give 后窗口物品迟迟不回背包 = 服务端并不会
	 * 管物品（canSend 探得 true 但实际无响应，典型为反作弊/代理服协商噪声）。
	 * 仅影响当前连接，重连归零重探；/wl localitems 任一档设置也会清掉。
	 */
	private volatile boolean localDisplayOverride = false;

	/** 本地直显模式下被 /wl hide 主动隐藏的窗口：自动摆位不得"复活"它们（否则下帧即重建）。 */
	public final Set<Long> localHiddenHandles = new HashSet<Long>();

	// give 静默失败探测：记录句柄 + 5s（100 tick）倒计时，到点背包仍无该物品 → 判探测虚高
	private long giveVerifyHandle = -1;
	private int giveVerifyTicks = 0;

	/** 命令面在发出 give payload 后调用：安排一次"物品是否真的回来了"的回查。 */
	public void armGiveVerification(long toplevelHandle) {
		giveVerifyHandle = toplevelHandle;
		giveVerifyTicks = 100;
	}

	private void tickGiveVerification() {
		if(giveVerifyTicks <= 0) return;
		if(--giveVerifyTicks > 0) return;
		long handle = giveVerifyHandle;
		giveVerifyHandle = -1;
		if(handle < 0 || !serverHasMod || localDisplayOverride) return;
		// 只读背包（零写入）：真实 mod 服务端此时应已把物品同步回来
		if(!itemManager.hasWindowItemFor(handle)) {
			localDisplayOverride = true;
			WaylandCraftCommon.LOGGER.info("[servermod] give payload unanswered after 5s despite canSend=true - forcing local direct-display until rejoin");
			Minecraft.getInstance().getChatListener().handleSystemMessage(Component.literal("§eWaylandCraft: 服务端声称支持物品但 5 秒无回包（疑似代理/反作弊服协商噪声），本连接已切换本地直显——用 §e/wl place <handle>§e 摆位，重连会重新探测"), false);
			maybeShowLocalModeTutorial();
		}
	}

	private void updateServerModCapability() {
		if(Minecraft.getInstance().level == null) return;
		if(serverModCheckCountdown-- > 0) return;
		serverModCheckCountdown = 20; // 每秒复测一次：容忍配置期注册延迟/换服时序

		boolean has = net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking.canSend(
				dev.evvie.waylandcraft.network.ServerboundGiveItemsPayload.TYPE);
		if(has == serverHasMod) return;

		serverHasMod = has;
		WaylandCraftCommon.LOGGER.info("[servermod] server-side WaylandCraft {}", has ? "PRESENT - item system active" : "MISSING - local direct-display mode");
		if(!has) {
			Minecraft.getInstance().getChatListener().handleSystemMessage(Component.literal("§eWaylandCraft 本地模式：服务端未安装 mod。窗口以本地直显方式运行（可见/可拖/键盘鼠标交互正常）；物品持久化与多人共享需服务端安装 waylandcraft-server.jar"), false);
			// 教学聊天与上面这条合并呈现：仅本服首次进入本地模式时追加（每服一次，不刷屏）
			maybeShowLocalModeTutorial();
		}
	}

	/** 命令/共享入口的能力守卫：vanilla 服务端下给出明确错误而非静默失败 */
	public boolean requireServerMod(String feature) {
		if(LocalModePolicy.serverModFeatureAllowed(serverHasMod)) return true;
		Minecraft.getInstance().getChatListener().handleSystemMessage(Component.literal("§c✘ " + feature + "需要服务端安装 WaylandCraft mod（waylandcraft-server.jar）；当前为本地模式，该功能静默不可用"), false);
		return false;
	}

	// ===== ⑩ 显示模式解析（能力探测 × localItems 档 × 运行期自愈覆盖）=====

	/** 生效显示档：按服务器记忆优先于全局（/wl localitems 在世界内设置时写按服记录）。 */
	public String effectiveLocalItems() {
		String global = settings != null ? settings.getLocalItems() : LocalModePolicy.LOCAL_ITEMS_AUTO;
		Map<String, String> per = settings != null ? settings.localItemsByServer() : Map.of();
		return WindowPlacementMemory.resolveLocalItems(currentServerKey(), per, global);
	}

	/** 当前世界内窗口驱动模式（updateWorld/命令面共用；纯读，无副作用）。 */
	public WindowDisplayMode effectiveDisplayMode() {
		return LocalModePolicy.resolveDisplayMode(serverHasMod, effectiveLocalItems(), localDisplayOverride);
	}

	/** give 静默失败自愈触发后清掉运行期覆盖（/wl localitems 设置命令调用；重连也会自动清）。 */
	public void clearLocalDisplayOverride() {
		localDisplayOverride = false;
	}

	/**
	 * 当前服务器标识：多人=服务器 IP；单人=存档目录名（=经 sanitize 的世界名，
	 * 跨会话稳定，单人同理）；无法确定（标题界面等）→ ""（不做持久化）。
	 */
	public String currentServerKey() {
		Minecraft mc = Minecraft.getInstance();
		ServerData sd = mc.getCurrentServer();
		if(sd != null) {
			return WindowPlacementMemory.serverKey(sd.ip, null);
		}
		var integrated = mc.getSingleplayerServer();
		if(integrated != null) {
			try {
				String folder = integrated.getWorldPath(net.minecraft.world.level.storage.LevelResource.ROOT)
						.getFileName().toString();
				return WindowPlacementMemory.serverKey(null, folder);
			} catch(Exception ignored) {
				// 存档路径异常不应阻断游戏——退化为"无标识不持久化"
			}
		}
		return "";
	}

	/**
	 * handle → 记忆别名（标题 slug + 同名编号）。保存与恢复共用本方法对
	 * bridge.getToplevels() 的同一枚举顺序，编号才稳定；只在窗口新建/摆位
	 * 命令/断开时调用，不在每 tick 热路径。
	 */
	public Map<Long, String> memoryAliases() {
		Map<Long, String> out = new HashMap<Long, String>();
		if(bridge == null) return out;
		List<Long> handles = new ArrayList<Long>();
		List<String> titles = new ArrayList<String>();
		for(WLCToplevel t : bridge.getToplevels()) {
			handles.add(t.getHandle());
			titles.add(getWindowName(t));
		}
		return WindowPlacementMemory.memoryAliasesByHandle(handles, titles);
	}

	/** 记一个窗口的摆位并落盘（place/move/rotate/grab 命令与断开清理共用）。 */
	public void rememberPlacementFor(WindowDisplay d, WLCToplevel t) {
		if(settings == null || settingsManager == null || d == null || t == null) return;
		String key = currentServerKey();
		if(key.isEmpty()) return;
		String alias = memoryAliases().get(t.getHandle());
		if(alias == null) return;
		WindowPlacementMemory.remember(settings.localWindowPlacements(), key, alias,
				new WindowPlacementMemory.Placement(d.pivot.x, d.pivot.y, d.pivot.z, d.yawDegrees(), d.anchorDistance));
		settingsManager.writeSettings();
	}

	/** 断开/换服前把当前全部显示摆位写入按服记忆（单人存档同理）。 */
	public void saveAllLocalPlacements() {
		if(settings == null || settingsManager == null || bridge == null) return;
		String key = currentServerKey();
		if(key.isEmpty()) return;
		Map<Long, String> aliases = memoryAliases();
		boolean touched = false;
		for(WindowDisplay d : displays) {
			if(!(d.window instanceof WLCToplevel t)) continue;
			String alias = aliases.get(t.getHandle());
			if(alias == null) continue;
			WindowPlacementMemory.remember(settings.localWindowPlacements(), key, alias,
					new WindowPlacementMemory.Placement(d.pivot.x, d.pivot.y, d.pivot.z, d.yawDegrees(), d.anchorDistance));
			touched = true;
		}
		if(touched) settingsManager.writeSettings();
	}

	private WindowPlacementMemory.Placement lookupRememberedPlacement(WLCToplevel t) {
		if(settings == null) return null;
		String key = currentServerKey();
		if(key.isEmpty()) return null;
		String alias = memoryAliases().get(t.getHandle());
		if(alias == null) return null;
		return WindowPlacementMemory.lookup(settings.localWindowPlacements(), key, alias);
	}

	/** 首次（每服一次）进入本地直显模式的教学聊天；标记持久化在 settings。 */
	public void maybeShowLocalModeTutorial() {
		if(settings == null || settingsManager == null || bridge == null) return;
		if(effectiveDisplayMode() != WindowDisplayMode.LOCAL_DIRECT) return;
		String key = currentServerKey();
		if(key.isEmpty() || WindowPlacementMemory.isMarked(settings.localModeTutorialShown(), key)) return;
		var chat = Minecraft.getInstance().getChatListener();
		chat.handleSystemMessage(Component.literal("§e▬ WaylandCraft 本地直显模式（服务端无 mod，无需背包物品）▬"), false);
		chat.handleSystemMessage(Component.literal(" §e1) 启动：§f/wl launch <应用> §7（或 §f/wl capture §7捕获桌面窗口）"), false);
		chat.handleSystemMessage(Component.literal(" §e2) 摆位：§f窗口出现即自动进世界；§f/wl place <handle> [距离1-6] §e重摆到面前"), false);
		chat.handleSystemMessage(Component.literal(" §7（布局开启时以布局槽位为准，place 为临时偏移会被布局重排覆盖；位置/朝向按本服务器记忆，重进自动回位）"), false);
		chat.handleSystemMessage(Component.literal(" §e3) 交互：§7看向窗口按 §fG §7绑定键盘（ESC 退出）；按 §fB §7开窗口管理屏（TAB 切换窗口）"), false);
		chat.handleSystemMessage(Component.literal(" §7默认不碰背包物品（反作弊安全）。可信服可用 §f/wl localitems items §7启用客户端物品手感——反作弊服开启可能被判定刷物品，风险自负。"), false);
		WindowPlacementMemory.mark(settings.localModeTutorialShown(), key);
		settingsManager.writeSettings();
	}
		
	private void checkKeybinds(Minecraft minecraft) {
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
			// G 键：纯键盘绑定（enableKeyboardCapture(false)）——只绑键盘不锁鼠标，视角仍可转动。
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
		// 换服立即重探能力，且不继承上一连接的运行期状态（直显覆盖/隐藏集合/回查）
		serverModCheckCountdown = 0;
		localDisplayOverride = false;
		localHiddenHandles.clear();
		giveVerifyTicks = 0;
		giveVerifyHandle = -1;
		updateServerModCapability();
		if(effectiveDisplayMode() == WindowDisplayMode.ITEM) {
			if(serverHasMod) itemManager.giveItemsIfMissing(bridge.getMappedToplevels());
			else itemManager.giveItemsLocallyIfMissing(bridge.getMappedToplevels());
		}
		// 同一进程重进同一 vanilla 服不会发生 serverHasMod 翻转，join 路径必须
		// 自己负责教学触发（内部按 serverKey 去重，每服一次）
		maybeShowLocalModeTutorial();
	}
	
	private void onClientDisconnect(ClientPacketListener listener, Minecraft minecraft) {
		// ⑩ 摆位记忆落盘必须在 displays.clear() 之前（位置数据就存在 display 上）
		saveAllLocalPlacements();
		displays.clear();
		// P1-2：断线必须彻底退出键盘/指针捕获。旧实现只清 displays——
		// keyboardCaptureMode/sharedKeyboardCapture/sharedPointerCapture/hovered* 全部残留，
		// 回到标题界面后按键仍被捕获分支吞掉（角色失控、WASD 失灵），且交互事件
		// 持续发往已关闭连接。disableKeyboardCapture() 统一完成：
		// releaseAllForwardedKeys（补发 release，防远端卡键）→ 清空 mode/shared* 绑定字段
		// → deactivateKeyboard + disablePointerCapture。sendInteraction 在无连接时
		// 为安全 no-op（见 SharedWindowClientHandler.sendInteraction 的连接守卫）。
		disableKeyboardCapture();
		hoveredDisplay = null;
		hoveredSharedDisplay = null;
		itemManager.reset();
		// 清掉仅对当前连接有效的运行期状态（隐藏集合/直显覆盖按服重建）
		localHiddenHandles.clear();
		localDisplayOverride = false;
		// fix/app-launcher-lifecycle：断开即本会话结束——主动终止本会话 spawn 的
		// 全部应用进程树（SIGTERM→限时→SIGKILL），从源头杜绝跨会话孤儿吞掉下次启动。
		// 非阻塞：升级 kill 在 native 后台线程，不拖断线收尾。
		appLifecycle.onSessionEnd();
	}
	
	@Nullable
	public static WLCToplevel getToplevel(ItemStack item) {
		if(item == null) return null;
		if(WaylandCraft.instance == null || WaylandCraft.instance.bridge == null) return null;
		
		WindowHandle data = item.get(WindowItem.WINDOW_HANDLE);
		if(data == null) return null;
		if(!data.matchesPlayer(Minecraft.getInstance().player)) return null;
		
		return WaylandCraft.instance.bridge.getToplevel(data.handle());
	}
	
	private void addWindowItemTooltip(ItemStack itemStack, TooltipContext ctx, TooltipFlag flag, List<Component> list) {
		WindowHandle handle = itemStack.get(WindowItem.WINDOW_HANDLE);
		if(handle != null) {
			String text = "Handle 0x" + Long.toHexString(handle.handle());
			Component component = Component
					.literal(text)
					.withStyle(ChatFormatting.GRAY);
			list.add(component);
			String owner = "Owner " + handle.player();
			component = Component
					.literal(owner)
					.withStyle(ChatFormatting.GRAY);
			list.add(component);
		}
	}
	
	private void updateDisplayRequests() {
		// Hide all windows that were minimized and unset minimize requested state
		// 钉住的窗口（pinnedToplevel）不受 minimize 影响，保持在世界中显示
		displays.removeIf((w) -> w.window instanceof WLCToplevel && ((WLCToplevel) w.window) != pinnedToplevel && ((WLCToplevel) w.window).requests.minimize);
		Stream.of(toplevelsThisTick()).forEach((t) -> t.requests.minimize = false);

		// Handle any maximize or unmaximize requests
		for(WLCToplevel toplevel : mappedToplevelsThisTick()) {
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
		for(WLCToplevel toplevel : toplevelsThisTick()) {
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
		if(sharedPointerCapture != null) return CursorShape.HIDE;
		if(pointerCapture != null) return CursorShape.HIDE;
		if(settings != null && settings.getHideCursor()) return CursorShape.HIDE;
		return bridge.getCursorShape();
	}
	
	private void processPointerMotion(Camera camera) {
		this.cursorShape = null;
		
		// 共享窗口"游戏模式"：指针已锁定到绑定窗口，跳过 hover 检测（视角不转、鼠标全进窗口）
		if(sharedPointerCapture != null) {
			if(sharedPointerCapture.display == null || !sharedPointerCapture.display.isValid()) {
				disableKeyboardCapture();
				return;
			}
			this.cursorShape = CursorShape.HIDE;
			return;
		}
		
		if(pointerCapture != null) {
			if(!pointerCapture.surface.isAlive()) {
				pointerCapture = null;
				return;
			}
			
			this.cursorShape = controlCursor();
			
			// 修复：不再因 maybeLockPointer 失败就解除捕获（浏览器从未申请指针锁）。
			// 捕获是否激活由 onMouseTurn 里的 relativeLocked 决定相对/绝对光标，
			// 这里只保留捕获本身（surface 存活即有效），鼠标事件全部转发绑定窗口。
			
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

		// v1.4.6 hover 诊断（用户反馈"点击还是 Minecraft 十字架"）：有窗口却
		// 始终命中不了时，2s 节流打一行各 display 的命中要素——plane 是否相交、
		// surface 链/尺寸、区域判定结果。第一因从"零线索"变成一行日志。
		if(finalHitResult == null && !displays.isEmpty() && bridge != null) {
			long now = System.currentTimeMillis();
			if(now - lastHoverDiagMs >= 2000) {
				lastHoverDiagMs = now;
				StringBuilder sb = new StringBuilder();
				int i = 0;
				for(WindowDisplay d : displays) {
					if(i++ >= 4) { sb.append(" +").append(displays.size() - 4).append(" more"); break; }
					WLCAbstractWindow w = d.window;
					WLCSurface root = w.getSurfaceTree();
					sb.append('[').append(i).append(" geom=").append(w.geometry)
							.append(" root=").append(root == null ? "null"
									: (root.isAlive() ? "ok" : "DEAD") + "/" + root.width() + "x" + root.height())
							.append(" last=").append(w.getSurfaceTreeLast() == null ? "null" : "chain")
							.append(']');
				}
				WaylandCraftCommon.LOGGER.info("[wlcraft][hover-diag] displays={} misses: {}", displays.size(), sb);
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
			
			// 只有 HARD_CAPTURE（J）hover 到窗口才锁鼠标；CAPTURE（G）是纯键盘绑定，
			// 鼠标必须保持自由（hover 只负责选焦点窗口，不拦截视角）。
			if(keyboardCaptureMode == KeyboardCaptureMode.HARD_CAPTURE) {
				// 绑定中 hover 到窗口 → 立即锁定鼠标（不再要求应用申请过指针锁）
				PointerCapture pc = new PointerCapture(surface, rel.x, rel.y);
				pc.relativeLocked = bridge.maybeLockPointer(surface);
				pointerCapture = pc;
			}
			
			// Focus on hover：绑定模式（G/J）下 hover 窗口即切换键盘焦点——
			// 这是绑定后键盘输入跟随窗口的关键（Rust keyboard_key 只转发给有 focus 的 surface）；
			// 未捕获时才尊重 focusOnHover 设置。
			if((keyboardCaptureMode != KeyboardCaptureMode.NONE || settings.getFocusOnHover())
					&& hoveredDisplay.target.window instanceof WLCToplevel toplevel) {
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
	/** GUI 逻辑坐标鼠标位置（MC 26.1.2 mouseHandler，与 overlay 绘制同坐标系）。 */
	private static double imeMouseX() {
		Minecraft mc = Minecraft.getInstance();
		return mc.mouseHandler.getScaledXPos(mc.getWindow());
	}

	private static double imeMouseY() {
		Minecraft mc = Minecraft.getInstance();
		return mc.mouseHandler.getScaledYPos(mc.getWindow());
	}

	public boolean onButtonPress(long windowHandle, int button, int action, int modifiers) {
		// IME 候选窗 overlay 优先拦截（Phase 3）：仅在 overlay 显示期间且点在
		// 其区域内才消费——事件经 imePopupClick 回传嵌套 fcitx5 的 popup
		// surface（引擎自绘命中，waylandpointer.cpp 取证）。不满足则整段跳过，
		// 既有窗口直通语义零改变；键盘（含 ESC 关聊天框）不经此路径，无影响。
		if(dev.evvie.waylandcraft.ime.ImeCandidateOverlay.handleClick(
				imeMouseX(), imeMouseY(), button, action != 0)) {
			return true;
		}

		// sync14 世界形态候选 quad 优先命中（键盘捕获 → 应用窗口旁的候选窗）：
		// 与 GUI 拦截同款规则——仅候选显示中且输入点（HARD_CAPTURE=窗口虚拟
		// 光标，CAPTURE=准星射线）落在候选 quad 上才消费；拖拽/缩放窗口独占
		// 时不抢。press/release 配对由 ImeCandidateWorld 内部跟踪。
		if(!pointerGrabs.isExclusiveGrabActive()
				&& dev.evvie.waylandcraft.ime.ImeCandidateWorld.handleClick(button, action != 0)) {
			return true;
		}

		if(pointerCapture != null) {
			if(action == 1 && !pointerCapture.pressedButtons.contains(button)) {
				// v1.4.5：button 自足——按下前强制 refocus 当前虚拟光标位。
				// button 只在 pointer 已有焦点时才发出（seat.rs），而 enter 靠
				// hover 轮询建立；任何一次 hover 中断（切屏/树重建）都会让
				// 随后的点击静默丢弃（用户症状"点击几乎不可用"）。重发
				// motion_refocus 使 enter→frame→button 永远成序。
				bridge.sendMotionRefocus(pointerCapture.surface, pointerCapture.x, pointerCapture.y);
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
		
		// 共享窗口"游戏模式"：点击/释放发往绑定窗口的虚拟光标位置（不再依赖 hover）
		if(sharedPointerCapture != null) {
			long handle = sharedPointerCapture.windowHandle;
			if(action == 1) {
				if(!sharedPointerCapture.pressedButtons.contains(button)) {
					SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.MOUSE_CLICK,
							sharedPointerCapture.x, sharedPointerCapture.y, glfwButtonToX11(button), 0);
					sharedPointerCapture.pressedButtons.add(button);
				}
			} else if(action == 0 && sharedPointerCapture.pressedButtons.contains(button)) {
				SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.MOUSE_RELEASE,
						sharedPointerCapture.x, sharedPointerCapture.y, glfwButtonToX11(button), 0);
				sharedPointerCapture.pressedButtons.remove(button);
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
				// v1.4.5：按下前强制 enter（同 pointerCapture 分支的理由）——
				// button 只在 pointer 有焦点时才发，enter 依赖上一 tick 的
				// hover；树重建/hover 中断后的第一次点击最易整颗丢失。
				if(hoveredDisplay.surface != null && hoveredDisplay.surfaceLocalRelative != null) {
					bridge.sendMotionRefocus(hoveredDisplay.surface,
							hoveredDisplay.surfaceLocalRelative.x, hoveredDisplay.surfaceLocalRelative.y);
				}
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
		// 共享窗口"游戏模式"：鼠标移动 → 绑定窗口内虚拟光标移动（视角不再转动）
		if(sharedPointerCapture != null) {
			SharedWindowDisplay d = sharedPointerCapture.display;
			if(d == null || !d.isValid()) {
				disableKeyboardCapture();
				return true;
			}
			double w = d.getWidth() > 0 ? d.getWidth() : 1;
			double h = d.getHeight() > 0 ? d.getHeight() : 1;
			double nx = Math.max(0, Math.min(w, sharedPointerCapture.x + dx));
			double ny = Math.max(0, Math.min(h, sharedPointerCapture.y + dy));
			if(nx != sharedPointerCapture.x || ny != sharedPointerCapture.y) {
				sharedPointerCapture.x = nx;
				sharedPointerCapture.y = ny;
				SharedWindowClientHandler.sendInteraction(sharedPointerCapture.windowHandle,
						SharedWindowInteractionPayload.InteractionType.MOUSE_MOVE, nx, ny, 0, 0);
			}
			return true;
		}
		
		if(pointerCapture == null) return false;
		
		// 应用申请过指针锁（游戏类）→ 相对移动（原路径）
		if(pointerCapture.relativeLocked) {
			bridge.sendRelativeMotion(dx, dy);
		}
		else {
			// 浏览器/桌面应用未申请指针锁 → 绝对虚拟光标：鼠标移动换算成窗口内像素移动，
			// 用 sendMotionRefocus 移动窗口内光标（应用自身光标会跟随，点击落在光标处）。
			double w = Math.max(1, pointerCapture.surface.width());
			double h = Math.max(1, pointerCapture.surface.height());
			double nx = Math.max(0, Math.min(w, pointerCapture.x + dx));
			double ny = Math.max(0, Math.min(h, pointerCapture.y + dy));
			if(nx != pointerCapture.x || ny != pointerCapture.y) {
				pointerCapture.x = nx;
				pointerCapture.y = ny;
				bridge.sendMotionRefocus(pointerCapture.surface, pointerCapture.x, pointerCapture.y);
			}
		}
		return true;
	}
	
	/* Handle mouse scroll input
	 * Returns true when the mouse scroll action has been consumed
	 */
	public boolean onScroll(long windowHandle, double scrollX, double scrollY) {
		// IME 候选窗 overlay 优先拦截（Phase 3，与 onButtonPress 同规则：
		// 仅 overlay 区域内消费；窗外滚轮维持窗口缩放/聊天历史等既有语义）。
		if(dev.evvie.waylandcraft.ime.ImeCandidateOverlay.handleScroll(
				imeMouseX(), imeMouseY(), scrollY)) {
			return true;
		}

		// sync14 世界形态候选 quad 滚轮优先（翻页 > 手持窗口的距离缩放）：
		// 仅候选显示中且输入点命中 quad 才消费，与 onButtonPress 同款规则。
		if(!pointerGrabs.isExclusiveGrabActive()
				&& dev.evvie.waylandcraft.ime.ImeCandidateWorld.handleScroll(scrollY)) {
			return true;
		}

		ItemStack useItem = getPlayerUseWindowItem();
		if(useItem != null) {
			WLCToplevel toplevel = getToplevel(useItem);
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

		// v1.4.6（用户反馈"部分场景 app 无法滑动但可点击，过一会又自己恢复"）：
		// axis 与 button 同款前提——pointer 有焦点才发；焦点被 hover 中断/leave
		// 清掉后 scroll 静默丢弃，而点击路径 v1.4.5 已有 refocus 兜底所以"能点
		// 不能滚"，下一次 hover/点击重建焦点后又"自己恢复"。此分支把 J 模式
		// 的滚轮按最后虚拟光标位强制 refocus 后再发，滚动从此自足。
		if(pointerCapture != null && pointerCapture.surface.isAlive()) {
			bridge.sendMotionRefocus(pointerCapture.surface, pointerCapture.x, pointerCapture.y);
			bridge.sendScroll(0, -scrollY);
			bridge.sendScroll(1, -scrollX);
			return true;
		}

		// 共享窗口"游戏模式"：滚轮发往绑定窗口（即使不 hover）
		if(sharedPointerCapture != null) {
			int data = ((int) Math.round(scrollX * 100) & 0xFFFF) | (((int) Math.round(scrollY * 100) & 0xFFFF) << 16);
			SharedWindowClientHandler.sendInteraction(sharedPointerCapture.windowHandle,
					SharedWindowInteractionPayload.InteractionType.SCROLL,
					sharedPointerCapture.x, sharedPointerCapture.y, data, 0);
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
			
			// v1.4.6：滚动自足（与点击同款）——axis 需要 pointer focus，hover 曾
			// 中断（leave 已发）时直接滚会静默丢；先按当前命中位 refocus 再发。
			if(hoveredDisplay.surface != null && hoveredDisplay.surfaceLocalRelative != null) {
				bridge.sendMotionRefocus(hoveredDisplay.surface,
						hoveredDisplay.surfaceLocalRelative.x, hoveredDisplay.surfaceLocalRelative.y);
			}
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
	/**
	 * P1-1 纯函数（可单测）：J 键事件是否被绑定逻辑**本地消费**（true = 不转发给窗口）。
	 * 语义矩阵（与 onKeyPress 的 J 分支一致）：
	 *  - NONE：press 触发绑定（消费），release/repeat 对应的 press 从未转发 → 全部本地消费；
	 *  - HARD_CAPTURE：J 是控制键（绑定已建立），press 提示、repeat/release 静默 → 全部本地消费；
	 *  - CAPTURE（G 模式）：J 是窗口普通键——press 会被转发，因此 release **必须**同样转发，
	 *    否则远端窗口维持"J 一直按住"（卡键）。返回 false 交给统一转发路径。
	 */
	static boolean consumeJKeyLocally(KeyboardCaptureMode mode, int action) {
		return mode != KeyboardCaptureMode.CAPTURE;
	}

	// Ctrl + 方向键：调整面前的窗口（优先 hover 的窗口，否则视线中心最近的窗口）
	public boolean onKeyPress(long windowHandle, int key, int scancode, int action, int modifiers) {
		// [kb-debug] onKeyPress 入口：P2-13 每键刷屏 → debug（发布模式默认 INFO 不再逐键打）
		WaylandCraftCommon.LOGGER.debug("[kb-debug] onKeyPress 入口 key={} scancode={} action={} mode={} bridge={} sharedCap={} hoveredLocal={} hoveredShared={}",
			key, scancode, action, keyboardCaptureMode,
			bridge != null ? "yes" : "no",
			sharedKeyboardCapture != null ? "yes" : "no",
			hoveredDisplay != null ? (hoveredDisplay.dist >= 0 ? "hit" : "miss") : "none",
			hoveredSharedDisplay != null ? "yes" : "no");

		// J 键：进入"游戏模式"（绑定键盘+鼠标，鼠标事件全部进窗口、隐藏鼠标）。
		// 用户需求：J 默认进入绑定，**按 ESC 之前不退出**——已在绑定中时再按 J 不解除。
		// G 纯键盘绑定（CAPTURE）下按 J **作为普通键转发给窗口**（G 模式按键全部
		// 归窗口，J 也不例外），绝不触发/升级绑定；仅 HARD_CAPTURE 下按 J 提示一次。
		// P1-1 修复：旧实现 `action == 0 return true` 在**所有**模式吞掉 J 的 RELEASE——
		// CAPTURE 模式下 press 已转发进窗口而 release 被吞，远端窗口永远维持 J 按住（卡键）。
		// 现在"本地消费"（含吞 release）只适用于 press 确实被绑定逻辑消费的
		// NONE（press→绑定）与 HARD_CAPTURE（J 为控制键）；CAPTURE 下 press/release/repeat
		// 全部落入下方统一转发路径，保持配对。
		if(key == GLFW.GLFW_KEY_J) {
			if(consumeJKeyLocally(keyboardCaptureMode, action)) {
				// PRESS 才触发绑定/提示（REPEAT 与 RELEASE 静默消费，与原语义一致）
				if(action == GLFW.GLFW_PRESS) {
					if(keyboardCaptureMode == KeyboardCaptureMode.NONE) {
						if(bridge == null && hoveredSharedDisplay == null) {
							Minecraft.getInstance().getChatListener().handleSystemMessage(Component.literal("WaylandCraft: 没有可捕获的窗口（对准一个窗口再按 J）"), false);
							return true;
						}
						enableKeyboardCapture(true);
						return true;
					}
					if(keyboardCaptureMode == KeyboardCaptureMode.HARD_CAPTURE) {
						Minecraft.getInstance().getChatListener().handleSystemMessage(Component.literal("WaylandCraft: 已在绑定模式，按 ESC 退出"), false);
					}
				}
				return true;
			}
			// CAPTURE（G 模式）：不拦截，继续向下走统一转发路径
		}

		// ESC：任何捕获模式下都退出绑定（键盘+鼠标全部解绑），
		// 不再把 ESC 转发给窗口——避免"按 ESC 想退出却作用到浏览器"。
		// 未捕获时 ESC 照常转发给 hover 的共享窗口或漏给游戏（打开游戏菜单）。
		if(keyboardCaptureMode != KeyboardCaptureMode.NONE && key == GLFW.GLFW_KEY_ESCAPE) {
			if(action == 0) return true;
			disableKeyboardCapture();
			return true;
		}

		// Ctrl + 方向键：**与相邻窗口交换位置**（0=上 1=下 2=左 3=右）。
		// 布局启用并初始化时：核心窗口与该方向相邻窗口交换排序（位置真的互换），
		// 左/右环绕、上/下跨层环绕。布局未启用时：无条件调用 moveFrontWindow(dir)
		// （v0.2.37 语义自由移动窗口）。
		if(action == GLFW.GLFW_PRESS && (modifiers & GLFW.GLFW_MOD_CONTROL) != 0) {
			int dir = switch(key) {
				case GLFW.GLFW_KEY_UP -> 0;
				case GLFW.GLFW_KEY_DOWN -> 1;
				case GLFW.GLFW_KEY_LEFT -> 2;
				case GLFW.GLFW_KEY_RIGHT -> 3;
				default -> -1;
			};
			if(dir >= 0) {
				WaylandCraftCommon.LOGGER.info("[swap] Ctrl+方向键 dir={} layoutEnabled={} layoutInit={} localDisplays={} sharedDisplays={}",
					dir, layoutManager.isEnabled(), layoutManager.isInitialized(), displays.size(), sharedDisplays.size());
				if(layoutManager.isEnabled() && layoutManager.isInitialized()) {
					layoutManager.swapCore(dir);
				} else {
					moveFrontWindow(dir);
				}
				return true;
			}
		}

		if(keyboardCaptureMode == KeyboardCaptureMode.NONE) {
			// 未捕获：hover 共享窗口时按键直接转发（对准窗口即操作窗口，无需先绑定）
			if(hoveredSharedDisplay != null) {
				forwardSharedKey(hoveredSharedDisplay.getWindowHandle(), key, action, modifiers);
				return true;
			}
			// hover 已丢失：补发旧窗口仍按住的键 release（防止远端窗口卡 Shift/CapsLock）
			if(!pressedForwardKeys.isEmpty() && lastKeyForwardHandle >= 0) {
				releaseAllForwardedKeys(lastKeyForwardHandle);
			}
			return false;
		}

		// 共享键盘捕获：按键走网络转发到发送端注入真实窗口
		if(sharedKeyboardCapture != null) {
			forwardSharedKey(sharedKeyboardCapture.getWindowHandle(), key, action, modifiers);
			return true;
		}

		if(bridge == null) return true;

		// 本地路径焦点自愈：转发前确保焦点在 hover 窗口（幂等；防止焦点被意外清掉
		// 后按键被 Rust 丢弃——这是"键盘输入不能穿透窗口"的最后一道保险）。
		ensureKeyboardFocus("onKeyPress");

		// [kb-debug] 本地转发：P2-13 每键刷屏 → debug（诊断时 -Dwaylandcraft.debug 或 log4j 配置开）
		WaylandCraftCommon.LOGGER.debug("[kb-debug] 本地转发 key={} scancode={} action={} ({} -> bridge)",
			key, scancode, action,
			action == GLFW.GLFW_PRESS ? "pressKey" : action == GLFW.GLFW_RELEASE ? "releaseKey" : "repeatKey");

		// 本地 bridge 路径三态完整透传（Rust keyboardInput 0=release 1=press 2=repeat）。
		// 之前 REPEAT 在这里被吞掉 → 长按失效（窗口收不到重复按键）。
		// pressedForwardKeys/forwardedMods 只属于共享窗口网络转发，本地路径直接透传。
		if(action == GLFW.GLFW_PRESS) {
			bridge.pressKey(scancode);
		}
		else if(action == GLFW.GLFW_RELEASE) {
			bridge.releaseKey(scancode);
		}
		else if(action == GLFW.GLFW_REPEAT) {
			bridge.repeatKey(scancode);
		}

		return true;
	}

	/**
	 * 转发单个按键到共享窗口（配对跟踪：press 记入、release 移除；目标窗口切换时先补发旧窗口 release，防卡键）。
	 *
	 * 修饰键处理（修复"浏览器快捷键失效/只有单键有效"）：
	 *  - 修饰键事件本身：立即转发 press/release，并更新 forwardedMods；
	 *  - 普通键事件：先调用 syncForwardedModifiers 把远端修饰键状态对齐到本次事件的
	 *    modifiers（只发差异），再转发该键。这样即使某些平台（如 Android 物理键盘）
	 *    GLFW 漏发修饰键事件，只要普通键事件的 modifiers 正确，组合键就能在远端生效。
	 */
	private void forwardSharedKey(long handle, int key, int action, int modifiers) {
		if(handle != lastKeyForwardHandle) {
			releaseAllForwardedKeys(lastKeyForwardHandle);
			lastKeyForwardHandle = handle;
		}

		// 修饰键：按事件本身更新状态并转发（忽略 event.modifiers，避免重复/错乱）
		int modBit = glfwModifierBit(key);
		if(modBit != 0) {
			if(action == GLFW.GLFW_PRESS) {
				if((forwardedMods & modBit) == 0) {
					forwardedMods |= modBit;
					forwardModifierKey(handle, key, true);
				}
			}
			else if(action == GLFW.GLFW_RELEASE) {
				if((forwardedMods & modBit) != 0) {
					forwardedMods &= ~modBit;
					forwardModifierKey(handle, key, false);
				}
			}
			return;
		}

		// 普通键：先把远端修饰键状态同步到本次事件的 modifiers，再转发该键。
		// ⑨ v1.4.9：key 字段由 X11 keysym 改为 **GLFW keycode** 原值——
		// wayland owner 直接走 scancode 注入链（glfwGetKeyScancode→bridge），
		// X11 owner 在本地 glfwKeyToKeysym 转换（两端同批交付，协议语义同步变更）。
		syncForwardedModifiers(handle, modifiers);

		if(action == GLFW.GLFW_PRESS) {
			pressedForwardKeys.add(key);
			SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.KEY_PRESS,
					0, 0, 0, key);
		}
		else if(action == GLFW.GLFW_RELEASE) {
			pressedForwardKeys.remove(key);
			SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.KEY_RELEASE,
					0, 0, 0, key);
		}
		else if(action == GLFW.GLFW_REPEAT) {
			// 长按透传（需求1 补全）：共享窗口网络转发路径之前只处理 PRESS/RELEASE，
			// REPEAT 被静默丢弃 → 远端窗口长按失效。⑨ 改发 KEY_REPEAT 专用类型：
			// wayland owner 走 bridge.repeatKey（wl repeat 语义），X11 owner 降级重复 down；
			// 配对集合保持原有记录（press 已加入），只确保万一漏记时补上。
			pressedForwardKeys.add(key);
			SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.KEY_REPEAT,
					0, 0, 0, key);
		}
	}

	/** 转发单个修饰键 press/release（与普通键同一配对集合，releaseAllForwardedKeys 统一兜底） */
	private void forwardModifierKey(long handle, int key, boolean pressed) {
		if(pressed) {
			pressedForwardKeys.add(key);
			SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.KEY_PRESS,
					0, 0, 0, key);
		}
		else {
			pressedForwardKeys.remove(key);
			SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.KEY_RELEASE,
					0, 0, 0, key);
		}
	}

	/**
	 * 把远端修饰键状态对齐到 desiredMods（GLFW_MOD_* 掩码），只发差异。
	 * 保证 Ctrl+B / Shift+字母 / Ctrl+C/V 等组合键在远端 X11 窗口生效。
	 */
	private void syncForwardedModifiers(long handle, int desiredMods) {
		final int[] modKeys = {
			GLFW.GLFW_KEY_LEFT_CONTROL, GLFW.GLFW_KEY_LEFT_SHIFT,
			GLFW.GLFW_KEY_LEFT_ALT, GLFW.GLFW_KEY_LEFT_SUPER
		};
		final int[] modBits = {
			GLFW.GLFW_MOD_CONTROL, GLFW.GLFW_MOD_SHIFT,
			GLFW.GLFW_MOD_ALT, GLFW.GLFW_MOD_SUPER
		};
		for(int i = 0; i < modBits.length; i++) {
			boolean want = (desiredMods & modBits[i]) != 0;
			boolean have = (forwardedMods & modBits[i]) != 0;
			if(want && !have) {
				forwardedMods |= modBits[i];
				forwardModifierKey(handle, modKeys[i], true);
			}
			else if(!want && have) {
				forwardedMods &= ~modBits[i];
				forwardModifierKey(handle, modKeys[i], false);
			}
		}
	}

	/** GLFW key → GLFW_MOD_* 位（非修饰键返回 0） */
	private static int glfwModifierBit(int key) {
		return switch(key) {
			case GLFW.GLFW_KEY_LEFT_CONTROL, GLFW.GLFW_KEY_RIGHT_CONTROL -> GLFW.GLFW_MOD_CONTROL;
			case GLFW.GLFW_KEY_LEFT_SHIFT, GLFW.GLFW_KEY_RIGHT_SHIFT -> GLFW.GLFW_MOD_SHIFT;
			case GLFW.GLFW_KEY_LEFT_ALT, GLFW.GLFW_KEY_RIGHT_ALT -> GLFW.GLFW_MOD_ALT;
			case GLFW.GLFW_KEY_LEFT_SUPER, GLFW.GLFW_KEY_RIGHT_SUPER -> GLFW.GLFW_MOD_SUPER;
			default -> 0;
		};
	}

	/** 向窗口补发所有仍按住的键的 release（窗口切换/退出绑定时调用，防止远端窗口卡 Shift/CapsLock） */
	private void releaseAllForwardedKeys(long handle) {
		forwardedMods = 0;
		if(handle < 0 || pressedForwardKeys.isEmpty()) return;
		for(int key : pressedForwardKeys) {
			SharedWindowClientHandler.sendInteraction(handle, SharedWindowInteractionPayload.InteractionType.KEY_RELEASE,
					0, 0, 0, key);
		}
		pressedForwardKeys.clear();
		lastKeyForwardHandle = -1;
	}

	/**
	 * GLFW keycode → X11 keysym。
	 * ⑨ v1.4.9 起 owner X11 分支在本地做此转换（wire 上 key=GLFW keycode）；
	 * wayland owner 不用此函数（走 scancode 链）。
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
	
	/**
	 * 键码校正。**实测证据：GLFW 在 Linux（X11 与 Wayland）返回的 scancode 已经是
	 * X11/xkb keycode（= evdev + 8）**：
	 *   - 实测日志 mixin onPress key=87(W) scancode=25、key=280(CapsLock) scancode=66
	 *     —— W 的 X11 keycode=25（evdev=17）、CapsLock 的 X11 keycode=66（evdev=58）。
	 *
	 * Rust 侧（keyboard_update_xkb / keyboard_key）统一用 `key - 8` 把这里传过去的
	 * X11 keycode 还原为 evdev 键码：xkb_state 更新、pressed_keys、以及发往窗口的
	 * wl_keyboard.key 键码（协议要求 evdev）全部正确。
	 *
	 * **曾经在这里 wayland 平台 `scancode += 8` 是"键盘穿透"总根因**：GLFW 已给
	 * X11 keycode，再 +8 → evdev+16，Rust -8 还原后仍是 X11 keycode（evdev+8）→
	 * 发给窗口的键码全部错位 +8：
	 *   - Caps Lock 发 66（evdev 66 不是 Caps Lock）→ xkb 锁定位不翻转、Firefox 不切换 → 永远小写；
	 *   - Ctrl_L 发 37（evdev 37 不是 Ctrl）→ 修饰键错位 → Ctrl+C/L 等快捷键全部失效；
	 *   - 普通字母（W=25 → evdev 25=KEY_P）→ 窗口收到别的键。
	 * 修复：不再 +8，原样返回（两个平台 GLFW 都已是 X11 keycode）。
	 */
	public static int correctScancode(int scancode) {
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
		window.moveOrigin(parent.localToWorld(popup.offsetX, popup.offsetY, 0.01));
	}
	
	public static enum KeyboardCaptureMode {
		
		NONE, CAPTURE, HARD_CAPTURE;
		
	}
	
	public static class PointerCapture {
		
		public final WLCSurface surface;
		
		// Pointer capture entry surface-local coordinates
		public double x;
		public double y;
		
		// 应用是否申请过 zwp_pointer_constraints 指针锁（游戏类）：
		// true → onMouseTurn 用 sendRelativeMotion（相对移动，锁内标准路径）；
		// false（浏览器/桌面应用）→ 用绝对虚拟光标 sendMotionRefocus 移动窗口内光标。
		public boolean relativeLocked = false;
		
		public HashSet<Integer> pressedButtons = new HashSet<Integer>();
		
		public PointerCapture(WLCSurface surface, double x, double y) {
			this.surface = surface;
			this.x = x;
			this.y = y;
		}
		
	}
	
	/** 共享窗口"游戏模式"指针捕获：J 绑定共享窗口后鼠标事件全部转发该窗口（不依赖 hover） */
	public static class SharedPointerCapture {
		
		public final long windowHandle;
		public final SharedWindowDisplay display;
		
		// 窗口内虚拟光标位置（像素，相对窗口左上角，含 xoff/yoff）
		public double x;
		public double y;
		
		public HashSet<Integer> pressedButtons = new HashSet<Integer>();
		
		public SharedPointerCapture(long windowHandle, SharedWindowDisplay display, double x, double y) {
			this.windowHandle = windowHandle;
			this.display = display;
			this.x = x;
			this.y = y;
		}
		
	}
	
}

