package dev.evvie.waylandcraft.shared;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.RejectedExecutionException;

import org.jetbrains.annotations.Nullable;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.bridge.WLCToplevel;
import dev.evvie.waylandcraft.network.SharedWindowClientHandler;
import dev.evvie.waylandcraft.network.SharedWindowImagePayload;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking;
import dev.evvie.waylandcraft.render.SharedWindowDisplay;
import dev.evvie.waylandcraft.utils.WcLogging;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayConnectionEvents;
import net.minecraft.client.Minecraft;
import net.minecraft.world.phys.Vec3;

/**
 * 窗口共享管理器（优化版）
 * 
 * 优化内容：
 * 1. 码率限速（Token Bucket）
 * 2. 自适应质量（根据带宽利用率动态调整scale）
 * 3. 像素差异检测（跳过无变化帧）
 */
public class WindowShareManager {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-share-manager");
	
	private final WaylandCraft clientMod;
	private final WaylandCraftCommon serverMod;
	
	private ImageCapture.CaptureConfig captureConfig;
	private final FrameRateController frameRateController;
	private final DiffUpdateManager diffUpdateManager;
	
	private final Map<Long, ShareState> shareStates = new ConcurrentHashMap<>();
	private boolean sharingEnabled = true;
	
	// === 码率限速 ===
	private long bytesSentThisSecond = 0;
	private long currentSecondStart = 0;
	
	// === 自适应（带宽不足时降 fps，而非降 scale）===
	// v0.9.18：原自适应是降 scale（缩放窗口 UI），违反"远程跟本地渲染一致"底线。
	// 改为动态帧率因子：带宽超限时下调实际 fps（保 scale=1.0 不变），充足时恢复。
	private float adaptiveFpsFactor = 1.0f; // 乘以 maxFps 得到实际帧率上限
	private int adaptiveEvalCounter = 0;
	private int adaptiveOverLimitCount = 0;    // 连续超限帧数
	private int adaptiveUnderUtilCount = 0;    // 连续低利用帧数
	private static final int ADAPTIVE_EVAL_INTERVAL = 60; // 每60帧评估一次
	private static final float ADAPTIVE_FPS_MIN = 0.25f;  // 最低降到目标帧率的 25%
	private static final float ADAPTIVE_FPS_MAX = 1.0f;
	private static final float ADAPTIVE_FPS_DOWN = 0.9f;  // 超限时降 10%
	private static final float ADAPTIVE_FPS_UP = 1.1f;    // 低利用时提 10%
	
	// === 帧间统计 ===
	private long adaptiveFrameBytes = 0; // 当前评估周期内的总字节
	private boolean lastFrameOverLimit = false;
	
	// === 心跳帧 ===
	// 无内容变化时，最多间隔这么久强制发一帧，保证接收端纹理持续刷新
	private static final long HEARTBEAT_INTERVAL_MS = 2000;
	
	// === JPEG 大小保护 / 编码耗时 ===
	// 整帧 capture+encode（含降级重编码）超过 20ms 时 LOGGER.debug 输出，用于诊断发送端是否吃紧
	private static final long ENCODE_WARN_THRESHOLD_NANOS = 20_000_000L; // 20ms
	
	public WindowShareManager(WaylandCraft clientMod) {
		this.clientMod = clientMod;
		this.serverMod = null;
		// ⑨ v1.4.9 用户裁定默认：scale=1.0（底线绝不动）、quality=0.95（默认最高档；
		// 1.0 是 JPEG 病态档——体积暴涨 40%+ 无视觉增益，故"最高"取 0.95）、
		// fps=0（解除帧率上限，跟随性能；diff 跳帧+每窗在途 1 帧编码自然节流）。
		// maxBitrate=40000（40Mbps 宽松天花板）：它是画质自适应环的负载信号——
		// 正常单窗/局域网永远用不满（≈不限速），仅多窗高画质并发时触发
		// "降 fps → 降 quality"的自动降载（默认最高、按负载自动降低/提高）。
		this.captureConfig = new ImageCapture.CaptureConfig(1.0f, 0.95f, 0, true, 40000, 3, 0, false, "jpeg", 0.02f);
		this.frameRateController = new FrameRateController();
		this.diffUpdateManager = new DiffUpdateManager();
		
		registerClientEvents();
		LOGGER.info("WindowShareManager initialized (client)");
	}
	
	public WindowShareManager(WaylandCraftCommon serverMod) {
		this.clientMod = null;
		this.serverMod = serverMod;
		this.captureConfig = null;
		this.frameRateController = new FrameRateController();
		this.diffUpdateManager = new DiffUpdateManager();
		
		LOGGER.info("WindowShareManager initialized (server)");
	}
	
	private void registerClientEvents() {
		ClientPlayConnectionEvents.DISCONNECT.register((handler, server) -> {
			handleDisconnect();
		});
	}
	
	/**
	 * 共享窗口音频捕获（只捕获共享窗口所属进程的声音）。
	 * 一次只跟一个窗口：最近 startSharing 的窗口。
	 */
	private AudioCaptureManager audioCapture;
	
	/** 当前音频跟随的窗口 handle（0 = 无音频）。⑧ 意图值：主线程写，executor 线程读 */
	private volatile long audioWindowHandle = 0;
	
	// ===== ⑧ v1.4.8 异步基础设施（命名/守护/惰性重建规范与 relay 线程池一致） =====
	//
	// 音频同步 executor：AudioCaptureManager.start/stop 内含同步 JNI（native PipeWire
	// boot 预算 10s + ack 15s + 线程 join）+ X11 枚举。旧实现全部在命令线程/渲染线程
	// 串行执行，`/wl share start all` 就是 N 次"停旧起新"全套阻塞——"瞬间卡一下"的
	// 主病灶。现在主线程只更新意图（audioWindowHandle），executor 每次 reconcile 只
	// 应用**最新**意图：N 连发 start 天然合并成 1 次真实启动。
	private final Object audioExecLock = new Object();
	private java.util.concurrent.ExecutorService audioSyncExecutor;
	/** 实际已应用的音频捕获窗口（仅 audioSyncExecutor 线程读写） */
	private long audioAppliedHandle = 0;
	
	// 编码 executor：JPEG/PNG 编码 + 大小保护重编码全部出渲染线程（1080p PNG 编码
	// 50–200ms/帧在旧路径直接压在渲染线程 = "分享时特别卡"的稳态病灶）。
	// 每窗口至多一帧在途（帧序 + 过载自然丢新帧，与 viewer 解码管线同范式）。
	private final Object encodeExecLock = new Object();
	private java.util.concurrent.ExecutorService encodeExecutor;
	private static final int ENCODE_THREADS = 2;
	/** 首帧风暴预算：新窗口首帧每 tick 最多放行这么多个（start all 第二波卡顿的闸门） */
	static final int FIRST_FRAME_CAPTURES_PER_TICK = 2;
	
	public boolean startSharing(long windowHandle, String windowTitle) {
		return startSharing(windowHandle, windowTitle, null);
	}

	/**
	 * 开始共享窗口。
	 * @param targetPlayer 定向共享目标玩家名（null/空 = 公开共享给所有人）
	 */
	public boolean startSharing(long windowHandle, String windowTitle, String targetPlayer) {
		if(clientMod == null) {
			LOGGER.warn("Cannot start sharing on server side");
			return false;
		}
		// ⑩ 共享注册/帧中继 100% 依赖服务端 mod，vanilla 服务端下会静默失效——明确拒绝
		if(!clientMod.requireServerMod("多人共享")) return false;

		if(shareStates.containsKey(windowHandle)) {
			LOGGER.warn("Window 0x{} is already being shared", Long.toHexString(windowHandle));
			return false;
		}
		
		ShareState state = new ShareState(windowHandle, windowTitle);
		shareStates.put(windowHandle, state);
		
		SharedWindowClientHandler.requestWindowRegister(windowHandle, windowTitle, targetPlayer);
		
		// ⑧ 音频捕获意图更新（executor 内实际应用，见 syncAudioState）。
		// appId 解析留在主线程：一次廉价 JNI 查表，重活（X11 枚举 + PipeWire 会话）在 executor。
		if(WaylandCraft.instance != null && WaylandCraft.instance.audioCaptureManager != null) {
			audioCapture = WaylandCraft.instance.audioCaptureManager;
			long prevAudioHandle = audioWindowHandle;
			audioWindowHandle = windowHandle;
			audioCapture.setActiveWindow(windowHandle);
			WLCToplevel toplevel = getLocalWindow(windowHandle);
			String appId = toplevel != null ? toplevel.appID : null;
			requestAudioSync(windowHandle, prevAudioHandle, windowTitle, appId);
		}
		
		LOGGER.info("Started sharing window 0x{}: {}", Long.toHexString(windowHandle), windowTitle);
		return true;
	}

	/**
	 * 开始共享一个 X11 窗口（XGetImage 抓帧）。
	 * 用于微信等 X11-only 应用：它们不在 xdg toplevel 列表里，
	 * 没有 wayland framebuffer，必须直接从 X server 抓像素。
	 *
	 * @param xid X window id（也作为共享 handle 使用）
	 * @param title 窗口标题
	 * @param displayName X display（如 ":2"，null = 默认 DISPLAY）
	 * @param appId 应用 ID（WM_CLASS 实例名），用于音频 PID 匹配
	 * @param pid 窗口进程 PID（_NET_WM_PID）
	 */
	public boolean startX11Sharing(long xid, String title, String displayName, String appId, int pid) {
		return startX11Sharing(xid, title, displayName, appId, pid, null);
	}

	/**
	 * 开始共享一个 X11 窗口（XGetImage 抓帧）。
	 * 用于微信等 X11-only 应用：它们不在 xdg toplevel 列表里，
	 * 没有 wayland framebuffer，必须直接从 X server 抓像素。
	 *
	 * @param xid X window id（也作为共享 handle 使用）
	 * @param title 窗口标题
	 * @param displayName X display（如 ":2"，null = 默认 DISPLAY）
	 * @param appId 应用 ID（WM_CLASS 实例名），用于音频 PID 匹配
	 * @param pid 窗口进程 PID（_NET_WM_PID）
	 * @param targetPlayer 定向共享目标玩家名（null/空 = 公开共享给所有人）
	 */
	public boolean startX11Sharing(long xid, String title, String displayName, String appId, int pid, String targetPlayer) {
		if(clientMod == null) {
			LOGGER.warn("Cannot start sharing on server side");
			return false;
		}
		// ⑩ 同上：共享必须有服务端中继
		if(!clientMod.requireServerMod("多人共享")) return false;

		if(shareStates.containsKey(xid)) {
			LOGGER.warn("X11 window 0x{} is already being shared", Long.toHexString(xid));
			return false;
		}

		// 校验窗口可访问
		dev.evvie.waylandcraft.utils.X11Capture.Geometry geo =
			dev.evvie.waylandcraft.utils.X11Capture.getGeometry(displayName, xid);
		if(geo == null) {
			LOGGER.warn("Cannot access X11 window 0x{} on display '{}'", Long.toHexString(xid), displayName);
			return false;
		}

		ShareState state = new ShareState(xid, title, ShareState.Source.X11, displayName, xid);
		state.x11Width = geo.width();
		state.x11Height = geo.height();
		state.x11RootX = geo.rootX();
		state.x11RootY = geo.rootY();
		state.x11AppId = appId;
		state.x11Pid = pid;
		shareStates.put(xid, state);

		SharedWindowClientHandler.requestWindowRegister(xid, title, targetPlayer);

		// ⑧ 音频捕获意图更新（X11 窗口有 _NET_WM_PID，复用按 PID 捕获链路；
		// 同步阻塞问题与 wayland 路径同源，异步化方式一致）
		if(WaylandCraft.instance != null && WaylandCraft.instance.audioCaptureManager != null) {
			audioCapture = WaylandCraft.instance.audioCaptureManager;
			long prevAudioHandle = audioWindowHandle;
			audioWindowHandle = xid;
			audioCapture.setActiveWindow(xid);
			requestAudioSync(xid, prevAudioHandle, title, appId);
		}

		LOGGER.info("Started sharing X11 window 0x{}: {} ({}x{} on {})",
			Long.toHexString(xid), title, geo.width(), geo.height(), displayName);
		return true;
	}
	
	public boolean stopSharing(long windowHandle) {
		ShareState state = shareStates.remove(windowHandle);
		if(state == null) {
			LOGGER.warn("Window 0x{} is not being shared", Long.toHexString(windowHandle));
			return false;
		}
		
		SharedWindowClientHandler.requestWindowUnregister(windowHandle);
		
		// 只停"音频跟随窗口"本身的音频；其他窗口共享不受影响。
		// ⑧ stop/fallback 的 native 阻塞（PipeWire join + 新会话 boot）挪入 executor 串行队列
		if(audioWindowHandle == windowHandle) {
			audioWindowHandle = 0;
			if(audioCapture != null) {
				requestAudioSync(0, 0, null, null);
			}
			// P2-18：跟随窗口停止后回落到仍在共享的"最近一个"窗口继续捕获；
			// 没有候选时明确提示（旧实现直接置 0，其余共享窗口从此无声且无日志线索）。
			fallbackAudioFollow(windowHandle);
		}
		
		diffUpdateManager.clearWindow(windowHandle);
		frameRateController.reset(windowHandle);
		ImageCapture.clearDiffCache(windowHandle);
		ImageCapture.cleanupPbo(windowHandle);
		ImageCapture.cleanupScaleFbo(windowHandle);
		// ⑧ 编码在途帧持有的 raw slot / diff 基准缓冲随窗口释放
		ImageCapture.cleanupRawSlots(windowHandle);
		
		LOGGER.info("Stopped sharing window 0x{}", Long.toHexString(windowHandle));
		return true;
	}
	
	public void update() {
		if(clientMod == null || !sharingEnabled) return;
		
		// 重置每秒码率计数器
		long now = System.currentTimeMillis();
		if(now - currentSecondStart > 1000) {
			bytesSentThisSecond = 0;
			currentSecondStart = now;
		}
		
		// ⑧ 首帧风暴预算：start all 后 N 个新窗口不再同 tick 全部放行首帧
		// （旧路径每窗首帧 2–3 次全帧同步回读 + 编码，N 倍同帧执行 = "瞬间卡一下"的
		// 第二波；预算把错峰摊到后续 tick，稳态老窗口完全不受影响）
		for(ShareState state : pickCaptureOrder(shareStates.values(), FIRST_FRAME_CAPTURES_PER_TICK)) {
			updateSharedWindow(state);
		}
	}
	
	/**
	 * ⑧ 纯函数（可单测）：本轮 update() 允许处理的窗口。稳态窗（frameCount&gt;0）直通；
	 * 首帧窗（frameCount==0）消耗预算，超出者顺延后续 tick。null 元素安全跳过。
	 */
	static List<ShareState> pickCaptureOrder(Iterable<ShareState> states, int firstFrameBudget) {
		List<ShareState> out = new ArrayList<>();
		int budget = Math.max(0, firstFrameBudget);
		for(ShareState s : states) {
			if(s == null) continue;
			if(s.frameCount == 0) {
				if(budget <= 0) continue;
				budget--;
			}
			out.add(s);
		}
		return out;
	}
	
	/**
	 * 更新单个共享窗口（⑧ v1.4.8 异步管线版）
	 *
	 * 旧实现全程压在渲染线程：回读（diff 时两次）→ JPEG/PNG 编码（1080p PNG
	 * 可达 50–200ms）→ 降级重编码每轮再回读+再编 → 组包发送。这是
	 * "分享窗口特别卡"的稳态病灶，start all 首帧风暴时还要 ×N。
	 *
	 * 新管线（三条线程职责清晰，帧序由"每窗在途 1 帧"保证）：
	 *   渲染线程：帧率闸 → 编码在途闸 → GPU blit 缩放 + PBO 回读进窗口自有
	 *             **双缓冲 raw slot**（一次回读；diff 检测与编码共用同一份数据，
	 *             旧 diff 路径的第二次全量回读就此消除）→ meta 快照 → 提交编码任务
	 *   编码线程：JPEG/PNG 编码 + 大小保护降级重编码（纯 CPU，不碰 GL/MC 状态）
	 *             → 释放 slot → Minecraft.execute 回渲染线程收尾
	 *   渲染线程：码率闸 / 自适应评估 / 组 payload / 发送 / 统计（与旧路径同样的
	 *             单线程所有权，计数器无需加锁）
	 *
	 * captureTs 语义变化（⑧）：由"发送时刻"改为"**捕获时刻**"——真内容时间，
	 * 编码排队延迟进 transit 下界观测（SharePlayoutController 共享时钟自然吸收），
	 * 不再进入排期环节；音频轨 poll 回推的 captureTs 同为内容时刻，两轨同锚。
	 *
	 * 底线不变：scale 永不动（大小保护只降 quality）、心跳帧语义、大小保护降级
	 * 强制 JPEG（v0.2.32 教训）、超限丢帧。
	 */
	private void updateSharedWindow(ShareState state) {
		// X11 窗口走独立抓帧路径（XGetImage，无 framebuffer/toplevel）
		if(state.source == ShareState.Source.X11) {
			updateSharedWindowX11(state);
			return;
		}

		ImageCapture.CaptureConfig effectiveConfig = state.getEffectiveConfig(captureConfig);
		
		// 帧率限制（maxFps<=0 无限制；带宽不足时按 adaptiveFpsFactor 动态下调）
		int effectiveFps = effectiveConfig.maxFps <= 0 ? 0
			: Math.max(1, Math.round(effectiveConfig.maxFps * adaptiveFpsFactor));
		if(!frameRateController.shouldUpdate(state.windowHandle, effectiveFps)) {
			return;
		}
		
		// ⑧ 每窗口至多一帧编码在途（编码跟不上时自然丢新帧，不堆队列不压渲染线程）
		if(state.encodeBusy) {
			state.encodeDroppedFrames++;
			return;
		}
		
		// 获取本地窗口
		WLCToplevel toplevel = getLocalWindow(state.windowHandle);
		if(toplevel == null || !toplevel.isMapped() || toplevel.framebuffer == null) {
			return;
		}
		
		// scale 锁死为配置值（不再动态缩放 —— 远程跟本地渲染一致是底线）
		float effectiveScale = effectiveConfig.scale;
		
		// === ⑧ 一次捕获：GPU 缩放 blit + PBO 异步回读进窗口 slot（bottom-up RGBA） ===
		// slot 归还编码任务释放；双槽交替保证读到的始终是最新完成帧。
		ImageCapture.RawFrame raw = ImageCapture.captureRawFrame(state.windowHandle, toplevel.framebuffer, effectiveScale);
		if(raw == null) {
			return;
		}
		
		// === 像素差异检测（⑧ 直接比对 slot 数据，不再第二次回读） ===
		if(effectiveConfig.diffUpdate) {
			if(!ImageCapture.hasSignificantChange(state.windowHandle, raw.buffer, effectiveConfig.diffThreshold)) {
				// 无显著变化，跳过本帧
				state.skippedFrames++;
				// 心跳帧：长时间无变化时也强制发一帧，避免接收端纹理一直不刷新
				// （也兜底 diff 基准帧因某种原因失效导致的永久静默）
				long nowMs = System.currentTimeMillis();
				if(nowMs - state.lastFrameSentTime <= HEARTBEAT_INTERVAL_MS) {
					ImageCapture.releaseRawFrame(raw);
					return;
				}
				state.skippedFrames = 0;
			}
		}
		
		// === ⑧ meta 快照：MC/display/settings 状态只允许渲染线程读 ===
		EncodeJob job = new EncodeJob();
		job.state = state;
		job.raw = raw;
		job.config = effectiveConfig;
		job.captureTs = System.currentTimeMillis();
		job.frameSeq = (int) state.frameCount;
		// 使用 framebuffer 原始尺寸（非缩放），接收端根据这个尺寸计算世界大小。
		// 注意：必须是 framebuffer 尺寸（含 xoff/yoff 偏移的完整缓冲），
		// 而不是 geometry 尺寸 —— 否则接收端四边形尺寸与纹理内容对不上，窗口会偏移/缩小。
		job.originalW = toplevel.framebuffer.getWidth();
		job.originalH = toplevel.framebuffer.getHeight();
		// xoff/yoff 随帧传递，接收端用于 bufOffset 对齐（与本地 WindowDisplay.render 一致）
		job.xOff = toplevel.framebuffer.getXOff();
		job.yOff = toplevel.framebuffer.getYOff();
		job.geometryWidth = toplevel.geometry.width();
		job.geometryHeight = toplevel.geometry.height();
		// 从本地WindowDisplay获取窗口变换
		if(clientMod != null) {
			for(var display : clientMod.displays) {
				if(display.window.getHandle() == state.windowHandle) {
					Vec3 pivot = display.pivot;
					Vec3 normal = display.normal();
					Vec3 d = display.down();
					job.pivotX = pivot.x; job.pivotY = pivot.y; job.pivotZ = pivot.z;
					job.normalX = normal.x; job.normalY = normal.y; job.normalZ = normal.z;
					job.downX = d.x; job.downY = d.y; job.downZ = d.z;
					// 视觉缩放倍数必须同步，否则接收端尺寸与本地不一致
					job.viewScale = display.viewScale;
					break;
				}
			}
		}
		// 发送端自己的 pixelsPerBlock：接收端用它渲染，保证世界尺寸与本地一致
		// （接收端可能使用不同的 PPB 设置或 native 不可用导致 settings==null）
		if(WaylandCraft.instance != null && WaylandCraft.instance.settings != null) {
			job.senderPixelsPerBlock = WaylandCraft.instance.settings.getPixelsPerBlock();
		} else {
			job.senderPixelsPerBlock = 500;
		}
		
		state.encodeBusy = true;
		try {
			ensureEncodeExecutor().submit(() -> runEncode(job));
		} catch(RejectedExecutionException e) {
			state.encodeBusy = false;
			ImageCapture.releaseRawFrame(raw);
			job.raw = null;
		}
	}
	
	/** 惰性创建编码线程池（daemon；断线不销毁——任务自校验 shareStates） */
	private java.util.concurrent.ExecutorService ensureEncodeExecutor() {
		synchronized(encodeExecLock) {
			if(encodeExecutor == null || encodeExecutor.isShutdown() || encodeExecutor.isTerminated()) {
				java.util.concurrent.atomic.AtomicInteger idx = new java.util.concurrent.atomic.AtomicInteger();
				encodeExecutor = java.util.concurrent.Executors.newFixedThreadPool(ENCODE_THREADS, r -> {
					Thread t = new Thread(r, "waylandcraft-share-encode-" + idx.getAndIncrement());
					t.setDaemon(true);
					return t;
				});
			}
			return encodeExecutor;
		}
	}
	
	/**
	 * ⑧ 编码任务：渲染线程填好输入快照，编码线程只读这些字段；
	 * 结果经 Minecraft.execute 回渲染线程组包发送（netty 入队也留在主线程，
	 * 与旧路径同线程语义，规避 ClientPlayNetworking 线程约束的不确定性）。
	 */
	private static final class EncodeJob {
		ShareState state;
		ImageCapture.RawFrame raw;          // wayland 路径：窗口 slot（编码完释放）
		ImageCapture.CaptureConfig config;
		long captureTs;
		int frameSeq;
		int originalW, originalH, xOff, yOff, geometryWidth, geometryHeight;
		double pivotX = 0, pivotY = 0, pivotZ = 0;
		double normalX = 0, normalY = 0, normalZ = 1;
		double downX = 0, downY = -1, downZ = 0;
		double viewScale = 1.0;
		int senderPixelsPerBlock = 500;
		// X11 专用
		boolean x11;
		String x11Display;
		long x11Xid;
		java.nio.ByteBuffer x11Buffer;      // 编码线程本地（XGetImage 结果，无 slot 池）
		int x11SrcW, x11SrcH, x11TargetW, x11TargetH;
		// 编码结果
		byte[] result;
		int degradeRounds;
		boolean sizeDropped;
	}
	
	/**
	 * ⑧ 编码线程：JPEG/PNG 编码 + 大小保护（只降 quality，scale 冻结——单槽捕获天然保持）。
	 * 与旧 captureFrameWithSizeProtection 语义逐条对齐：降级轮强制 JPEG
	 * （v0.2.32 教训：PNG 无损路径 quality 无效，降级必须能生效）、超限丢帧、每轮 WARN。
	 */
	private void runEncode(EncodeJob job) {
		long frameStart = System.nanoTime();
		try {
			ImageCapture.CaptureConfig config = job.config;
			float quality = effectiveQuality(config); // ⑨ 自适应画质（编码线程读 volatile 快照）
			byte[] imageData;
			int w, h;
			if(job.x11) {
				// X11 抓帧（XGetImage/X 往返同样挪出渲染线程；无 GL 依赖，连接独立）
				dev.evvie.waylandcraft.utils.X11Capture.Frame frame =
					dev.evvie.waylandcraft.utils.X11Capture.captureRgba(job.x11Display, job.x11Xid);
				if(frame == null) {
					completeEncode(job, null, 0, false); // 窗口最小化/销毁/抓取失败：跳过本帧
					return;
				}
				int srcW = frame.width();
				int srcH = frame.height();
				if(srcW <= 0 || srcH <= 0) {
					completeEncode(job, null, 0, false);
					return;
				}
				float scale = config.scale;
				int targetW = Math.max(1, Math.round(srcW * scale));
				int targetH = Math.max(1, Math.round(srcH * scale));
				java.nio.ByteBuffer rgba = frame.rgba();
				if(targetW != srcW || targetH != srcH) {
					rgba = scaleRgbaNearest(rgba, srcW, srcH, targetW, targetH);
				}
				// 刷新几何信息（窗口移动/缩放后交互坐标要跟着更新；字段 volatile，主线程只读展示）
				job.state.x11Width = srcW;
				job.state.x11Height = srcH;
				job.state.x11RootX = frame.rootX();
				job.state.x11RootY = frame.rootY();
				job.x11SrcW = srcW; job.x11SrcH = srcH;
				job.x11TargetW = targetW; job.x11TargetH = targetH;
				job.x11Buffer = rgba;
				imageData = ImageCapture.compressToJpegDirect(rgba, targetW, targetH, quality, false, false);
				w = targetW; h = targetH;
			}
			else {
				imageData = ImageCapture.compressToJpegDirect(job.raw.buffer, job.raw.width, job.raw.height, quality, true, false);
				w = job.raw.width; h = job.raw.height;
			}
			
			int degradeRounds = 0;
			boolean sizeDropped = false;
			if(imageData == null) {
				sizeDropped = true; // 编码失败：与旧路径"返回 null 丢帧"同义
			}
			else if(config.maxJpegBytes > 0) {
				while(imageData.length > config.maxJpegBytes && degradeRounds < config.maxDegradeRounds) {
					long beforeSize = imageData.length;
					float nextQuality = nextLadderValue(config.jpegQualityLadder, quality);
					if(nextQuality < 0) {
						break; // quality 已到阶梯底，无可降 → 直接走最终超限判定（丢帧）
					}
					quality = nextQuality;
					byte[] reencoded = job.x11
						? ImageCapture.compressToJpegDirect(job.x11Buffer, w, h, quality, false, true)
						: ImageCapture.compressToJpegDirect(job.raw.buffer, w, h, quality, true, true);
					if(reencoded == null) {
						imageData = null;
						break;
					}
					degradeRounds++;
					LOGGER.warn("Window 0x{} JPEG over size limit ({} bytes): {} -> {} bytes, degrade round {}: quality={} scale frozen (async encode)",
						Long.toHexString(job.state.windowHandle), config.maxJpegBytes,
						beforeSize, reencoded.length, degradeRounds, String.format("%.2f", quality));
					imageData = reencoded;
				}
				if(imageData != null && imageData.length > config.maxJpegBytes) {
					sizeDropped = true; // quality 到底仍超限 → 丢帧（丢帧允许，尺寸绝不降）
				}
			}
			
			long frameNanos = System.nanoTime() - frameStart;
			if(WcLogging.verbose() && frameNanos > ENCODE_WARN_THRESHOLD_NANOS) {
				LOGGER.debug("Window 0x{} async encode took {} ms ({} bytes, {} degrade round(s))",
					Long.toHexString(job.state.windowHandle), String.format("%.1f", frameNanos / 1_000_000.0),
					imageData == null ? 0 : imageData.length, degradeRounds);
			}
			completeEncode(job, imageData, degradeRounds, sizeDropped);
		} finally {
			if(job.raw != null) {
				ImageCapture.releaseRawFrame(job.raw);
			}
		}
	}
	
	/** ⑧ 编码完回渲染线程收尾（execute 失败=MC 正在关闭，直接清在途标志） */
	private void completeEncode(EncodeJob job, byte[] data, int degradeRounds, boolean sizeDropped) {
		job.result = data;
		job.degradeRounds = degradeRounds;
		job.sizeDropped = sizeDropped;
		try {
			Minecraft.getInstance().execute(() -> finishEncodedFrame(job));
		} catch(Throwable t) {
			job.state.encodeBusy = false;
		}
	}
	
	/**
	 * ⑧ 渲染线程收尾：码率闸/自适应评估/diff 封装/payload 组包/发送/统计——
	 * 全部与旧路径同线程同所有权（计数器不加锁的既有约定保持不变）。
	 */
	private void finishEncodedFrame(EncodeJob job) {
		ShareState state = job.state;
		state.encodeBusy = false;
		// 停共享/切世界竞态：窗口已不在共享表则结果作废（slot 已在编码线程释放）
		if(clientMod == null || !sharingEnabled || !shareStates.containsKey(state.windowHandle)) {
			return;
		}
		byte[] imageData = job.result;
		if(imageData == null || job.sizeDropped) {
			if(job.sizeDropped) {
				state.sizeDroppedFrames++;
				LOGGER.error("Window 0x{} JPEG still over size limit after {} degrade round(s), DROPPING frame (quality floor reached, scale preserved)",
					Long.toHexString(state.windowHandle), job.degradeRounds);
			}
			return;
		}
		if(job.degradeRounds > 0) {
			state.degradedFrames++;
		}
		ImageCapture.CaptureConfig config = job.config;
		
		// === 码率限速 ===
		if(config.maxBitrate > 0) {
			long maxBytesPerSecond = (long)config.maxBitrate * 1000 / 8; // kbps → bytes/sec
			if(bytesSentThisSecond + imageData.length > maxBytesPerSecond) {
				// 超过码率限制，跳过本帧
				state.rateLimitedFrames++;
				lastFrameOverLimit = true;
				adaptiveOverLimitCount++;
				adaptiveUnderUtilCount = 0;
				return;
			}
			lastFrameOverLimit = false;
		}
		
		// === 自适应质量评估 ===
		adaptiveEvalCounter++;
		adaptiveFrameBytes += imageData.length;
		if(adaptiveEvalCounter >= ADAPTIVE_EVAL_INTERVAL) {
			evaluateAdaptiveQuality(config);
			adaptiveEvalCounter = 0;
			adaptiveFrameBytes = 0;
		}
		
		// 处理差分更新（当前已禁用，JPEG压缩数据diff无意义）。
		// ⑧ X11 路径保持旧行为不经 diff 封装（旧 updateSharedWindowX11 直接发 imageData）
		byte[] processedData = job.x11 ? imageData : diffUpdateManager.processFrame(state.windowHandle, imageData);
		if(processedData == null) return;
		
		long now = System.currentTimeMillis();
		SharedWindowImagePayload imagePayload = job.x11
			// X11 无本地 display：默认变换（原点、正对、竖直）。
			// 接收端用原始窗口尺寸（srcW/srcH）算世界四边形大小，图像是缩放后的
			// （targetW/targetH），纹理拉伸到原始尺寸。
			? new SharedWindowImagePayload(
				state.windowHandle, 0, 0, 0,
				job.x11TargetW, job.x11TargetH,
				processedData,
				0, 0, 0,
				0, 0, 1,
				0, -1, 0,
				1.0, job.x11SrcW, job.x11SrcH,
				job.senderPixelsPerBlock,
				job.captureTs, job.frameSeq)
			: new SharedWindowImagePayload(
				state.windowHandle, 0, job.xOff, job.yOff,
				job.originalW, job.originalH,
				processedData,
				job.pivotX, job.pivotY, job.pivotZ,
				job.normalX, job.normalY, job.normalZ,
				job.downX, job.downY, job.downZ,
				job.viewScale, job.geometryWidth, job.geometryHeight,
				job.senderPixelsPerBlock,
				job.captureTs, job.frameSeq);
		ClientPlayNetworking.send(imagePayload);
		
		// 更新统计
		bytesSentThisSecond += processedData.length;
		state.lastUpdateTime = now;
		state.lastFrameSentTime = now;
		state.frameCount++;
		state.totalBytes += processedData.length;
		state.currentFps = frameRateController.getCurrentFps(state.windowHandle);
		state.currentBitrate = bytesSentThisSecond * 8 / 1000; // kbps
	}
	
	/**
	 * 更新单个 X11 共享窗口（⑧ v1.4.8：整段挪进编码线程）。
	 *
	 * 与 wayland 路径的区别：
	 * - 没有 toplevel/framebuffer，直接问 X server 要像素（XGetImage 往返）；
	 * - 没有本地 WindowDisplay，用默认变换（原点正对）；
	 * - 不做 diff 检测（X11 抓帧已经是整帧开销，diff 收益低，先全帧发送）。
	 *
	 * ⑧ X11 抓帧是 X 连接往返 + 逐像素缩放 + 编码，全部无 GL 依赖、连接独立，
	 * 旧实现把它们压在渲染线程 = X11 窗口共享的稳态卡顿。现在 updateSharedWindowX11
	 * 只做闸门与意图快照，真实工作由 runEncode(job.x11) 分支完成；几何字段经
	 * volatile 写回供交互注入读"最近一帧"（抓帧失败仍按旧语义跳过本帧）。
	 */
	private void updateSharedWindowX11(ShareState state) {
		ImageCapture.CaptureConfig effectiveConfig = state.getEffectiveConfig(captureConfig);

		// 帧率限制（maxFps<=0 无限制；带宽不足时按 adaptiveFpsFactor 动态下调）
		int effectiveFps = effectiveConfig.maxFps <= 0 ? 0
			: Math.max(1, Math.round(effectiveConfig.maxFps * adaptiveFpsFactor));
		if(!frameRateController.shouldUpdate(state.windowHandle, effectiveFps)) {
			return;
		}
		if(state.encodeBusy) {
			state.encodeDroppedFrames++;
			return;
		}

		EncodeJob job = new EncodeJob();
		job.state = state;
		job.x11 = true;
		job.config = effectiveConfig;
		job.x11Display = state.x11Display;
		job.x11Xid = state.x11Xid;
		job.captureTs = System.currentTimeMillis();
		job.frameSeq = (int) state.frameCount;
		job.senderPixelsPerBlock = senderPixelsPerBlock();
		state.encodeBusy = true;
		try {
			ensureEncodeExecutor().submit(() -> runEncode(job));
		} catch(RejectedExecutionException e) {
			state.encodeBusy = false;
		}
	}

	/** 发送端自己的 pixelsPerBlock（与 wayland 路径一致） */
	private int senderPixelsPerBlock() {
		if(WaylandCraft.instance != null && WaylandCraft.instance.settings != null) {
			return WaylandCraft.instance.settings.getPixelsPerBlock();
		}
		return 500;
	}

	/** 最近邻 RGBA 缩放（top-down） */
	private static java.nio.ByteBuffer scaleRgbaNearest(java.nio.ByteBuffer src, int srcW, int srcH, int dstW, int dstH) {
		java.nio.ByteBuffer dst = java.nio.ByteBuffer.allocateDirect(dstW * dstH * 4).order(java.nio.ByteOrder.nativeOrder());
		for(int y = 0; y < dstH; y++) {
			int srcY = y * srcH / dstH;
			for(int x = 0; x < dstW; x++) {
				int srcX = x * srcW / dstW;
				int si = (srcY * srcW + srcX) * 4;
				int di = (y * dstW + x) * 4;
				dst.put(di, src.get(si));
				dst.put(di + 1, src.get(si + 1));
				dst.put(di + 2, src.get(si + 2));
				dst.put(di + 3, src.get(si + 3));
			}
		}
		return dst;
	}

	// ⑧ v1.4.8：旧 encodeJpegWithSizeProtection（渲染线程同步编码 + 每轮 rewind 重编码）
	// 由 runEncode 的 x11 分支取代——大小保护/降级阶梯/WARN 语义逐条保留，编码挪出渲染线程。

	// ⑧ v1.4.8：旧 captureFrameWithSizeProtection（渲染线程"回读→编码→每轮降级再回读再编码"）
	// 由管线取代：渲染线程只 captureRawFrame 一次进 slot（PBO 异步回读），runEncode 在编码
	// 线程对同一份 slot 数据做编码与降级重编码（forceJpeg 语义保留，scale 天然冻结——
	// 重编码不再需要重回读，比旧路径每轮少 1 次全帧回读）。
	
	/**
	 * 在降级阶梯中取严格小于 current 的最大档位；阶梯为 null 或无更小档位时返回 -1。
	 * ⑨ 包私有：ShareQualityLadderTest 直打（降/升档对偶语义的回归网）。
	 */
	static float nextLadderValue(float[] ladder, float current) {
		if(ladder == null) {
			return -1f;
		}
		float best = -1f;
		for(float v : ladder) {
			if(v < current - 1e-4f && v > best) {
				best = v;
			}
		}
		return best;
	}
	
	/**
	 * 评估自适应帧率
	 * 带宽超限 → 降 fps（保 scale=1.0 不变），低利用 → 恢复 fps
	 */
	private void evaluateAdaptiveQuality(ImageCapture.CaptureConfig config) {
		if(config.maxBitrate <= 0) return; // 无码率限制时不做自适应
		
		long maxBytesPerSecond = (long)config.maxBitrate * 1000 / 8;
		float utilization = maxBytesPerSecond > 0 ? (float)adaptiveFrameBytes / (maxBytesPerSecond * ADAPTIVE_EVAL_INTERVAL / 20) : 0;
		
		if(adaptiveOverLimitCount >= ADAPTIVE_EVAL_INTERVAL / 2) {
			// 超过一半帧数都超限 → 降 fps（绝不降 scale，避免缩放窗口 UI）
			adaptiveFpsFactor = Math.max(ADAPTIVE_FPS_MIN, adaptiveFpsFactor * ADAPTIVE_FPS_DOWN);
			// 分级=细节（INFO→debug）：自适应调速是周期控制环噪声，发布档
			// latest.log 默认不可见；调优时把 log4j2 的本 mod logger 提到
			// DEBUG 即可看回。分级表见 docs/LOGGING.md。
			LOGGER.debug("[ADAPTIVE] FPS factor decreased to {} (over limit: {} frames)", String.format("%.2f", adaptiveFpsFactor), adaptiveOverLimitCount);
			adaptiveOverLimitCount = 0;
			// ⑨ v1.4.9 第二维降载（用户裁定：画质按负载自动降低）：fps 已降到最低档
			// 仍持续超限 → 才动 quality（沿阶梯下一档）。fps 优先、画质兜底，
			// 保证不到万不得已不牺牲观感。
			if(adaptiveFpsFactor <= ADAPTIVE_FPS_MIN + 0.001f) {
				if(++adaptiveQualityDownStreak >= QUALITY_DOWN_STREAK) {
					lowerAdaptiveQuality(config);
				}
			}
			else {
				adaptiveQualityDownStreak = 0;
			}
		} else if(utilization < 0.5f && adaptiveFpsFactor < ADAPTIVE_FPS_MAX) {
			// 利用率低于50% → 恢复 fps（逐步回到目标帧率）
			adaptiveFpsFactor = Math.min(ADAPTIVE_FPS_MAX, adaptiveFpsFactor * ADAPTIVE_FPS_UP);
			LOGGER.debug("[ADAPTIVE] FPS factor increased to {} (utilization: {}%)", String.format("%.2f", adaptiveFpsFactor), String.format("%.1f", utilization * 100));
			adaptiveUnderUtilCount = 0;
		}
		else if(utilization < 0.4f && adaptiveFpsFactor >= ADAPTIVE_FPS_MAX - 0.001f) {
			// ⑨ v1.4.9 升载：fps 已满且带宽持续宽裕 → 沿阶梯升回画质
			//（升档更保守：连续 QUALITY_UP_STREAK 个周期 ≈ 15s 才升一档，防振荡）
			if(++adaptiveQualityUpStreak >= QUALITY_UP_STREAK) {
				raiseAdaptiveQuality(config);
			}
		}
		else {
			adaptiveQualityDownStreak = 0;
			adaptiveQualityUpStreak = 0;
		}
	}
	
	// ===== ⑨ v1.4.9 自适应画质（默认最高档，按负载自动降低/提高；scale 永不动） =====
	/** 当前自适应画质（-1 = 未启用自适应，用 config.quality）。编码线程跨线程读 → volatile */
	private volatile float adaptiveQuality = -1f;
	private int adaptiveQualityDownStreak = 0;
	private int adaptiveQualityUpStreak = 0;
	/** fps 到底仍连续这么多个评估周期超限才降画质（fps 优先、画质兜底） */
	static final int QUALITY_DOWN_STREAK = 2;
	/** fps 已满且连续这么多个评估周期低利用才升画质（≈15s/档，防振荡） */
	static final int QUALITY_UP_STREAK = 6;
	
	private float currentAdaptiveQuality(ImageCapture.CaptureConfig config) {
		float aq = adaptiveQuality;
		return aq < 0 ? config.quality : aq;
	}
	
	private void lowerAdaptiveQuality(ImageCapture.CaptureConfig config) {
		float next = nextLadderValue(config.jpegQualityLadder, currentAdaptiveQuality(config));
		adaptiveQualityDownStreak = 0;
		if(next < 0) return; // 阶梯底，无可再降
		adaptiveQuality = next;
		adaptiveQualityUpStreak = 0;
		LOGGER.info("[share] adaptive quality lowered to {} (bandwidth saturated even at min fps)", String.format("%.2f", next));
	}
	
	private void raiseAdaptiveQuality(ImageCapture.CaptureConfig config) {
		float next = nextLadderUpValue(config.jpegQualityLadder, currentAdaptiveQuality(config));
		adaptiveQualityUpStreak = 0;
		if(next < 0) return; // 阶梯顶（默认最高档），无可再升
		adaptiveQuality = next;
		adaptiveQualityDownStreak = 0;
		LOGGER.info("[share] adaptive quality raised to {} (sustained bandwidth headroom)", String.format("%.2f", next));
	}
	
	/** ⑨ 纯函数（可单测）：阶梯中取严格大于 current 的最小档；无更高档/阶梯空返回 -1 */
	static float nextLadderUpValue(float[] ladder, float current) {
		if(ladder == null) return -1f;
		float best = -1f;
		for(float v : ladder) {
			if(v > current + 1e-4f && (best < 0 || v < best)) {
				best = v;
			}
		}
		return best;
	}
	
	/** ⑨ 编码线程取生效画质：自适应未启用时用配置值（默认最高档恒定） */
	private float effectiveQuality(ImageCapture.CaptureConfig config) {
		return currentAdaptiveQuality(config);
	}
	
	/**
	 * P2-18 纯函数（可单测）：从仍在共享的窗口里选音频回落候选——
	 * 排除 excludeHandle 后取 startTime 最新者；无候选返回 null。
	 */
	static ShareState pickAudioFallback(Iterable<ShareState> states, long excludeHandle) {
		ShareState next = null;
		for(ShareState s : states) {
			if(s == null || s.windowHandle == excludeHandle) continue;
			if(next == null || s.startTime > next.startTime) next = s;
		}
		return next;
	}

	/**
	 * P2-18：音频跟随窗口停止后，回落到仍在共享的"最近启动"窗口继续捕获
	 * （与 startSharing 的"单例跟随最近窗口"语义一致）；无候选时保持无音频并提示。
	 */
	private void fallbackAudioFollow(long stoppedHandle) {
		ShareState next = pickAudioFallback(shareStates.values(), stoppedHandle);
		if(next == null) {
			LOGGER.info("Audio follow window 0x{} stopped; no other shared window to fall back to",
				Long.toHexString(stoppedHandle));
			return;
		}
		if(audioCapture == null) return;
		// ⑧ 只更新意图+提交 reconcile；真正的 start（PipeWire boot/join）在 executor 里跑。
		// 意图被更新的任务覆盖时旧任务自动 no-op，stop all 的 N 次停/起不再串行阻塞。
		audioWindowHandle = next.windowHandle;
		audioCapture.setActiveWindow(next.windowHandle);
		String appId;
		if(next.source == ShareState.Source.X11) {
			appId = next.x11AppId;
		} else {
			WLCToplevel t = getLocalWindow(next.windowHandle);
			appId = t != null ? t.appID : null;
		}
		requestAudioSync(next.windowHandle, 0, next.windowTitle, appId);
		LOGGER.info("Audio follow falls back to shared window 0x{} (async start scheduled)",
			Long.toHexString(next.windowHandle));
	}
	
	// ===== ⑧ v1.4.8 音频意图合并 reconcile（executor 单线程串行） =====
	
	/** 惰性创建/重建音频同步 executor（断线 shutdownNow 后再次共享可恢复） */
	private java.util.concurrent.ExecutorService ensureAudioSyncExecutor() {
		synchronized(audioExecLock) {
			if(audioSyncExecutor == null || audioSyncExecutor.isShutdown() || audioSyncExecutor.isTerminated()) {
				audioSyncExecutor = java.util.concurrent.Executors.newSingleThreadExecutor(r -> {
					Thread t = new Thread(r, "waylandcraft-share-audio");
					t.setDaemon(true);
					return t;
				});
			}
			return audioSyncExecutor;
		}
	}
	
	/**
	 * 提交一次音频意图 reconcile。target=0 表示"应无捕获"；
	 * prev 为该意图生效前的跟随目标（start 失败回滚语义，P2-18）。
	 */
	private void requestAudioSync(long target, long prev, @Nullable String title, @Nullable String appId) {
		if(audioCapture == null) return;
		try {
			ensureAudioSyncExecutor().submit(() -> syncAudioState(target, prev, title, appId));
		} catch(java.util.concurrent.RejectedExecutionException e) {
			// 关停风暴（断线竞态）：意图已入 volatile，下一次 start/stop 会再提交 reconcile
			LOGGER.debug("Audio sync executor rejected (target 0x{}), deferred to next sync",
				Long.toHexString(target));
		}
	}
	
	/** ⑧ reconcile 单任务动作（decideAudioSync 返回值） */
	enum AudioSyncAction { SKIP, STOP, START, ALREADY }
	
	/**
	 * ⑧ 纯函数（可单测）：executor 任务在执行时刻的裁决——
	 * 意图已被更新的任务接管 → SKIP（去抖核心）；
	 * 目标 0 且正在捕获 → STOP；目标 0 且无捕获 → ALREADY；
	 * 目标==已应用且仍在捕获 → ALREADY（幂等，start all 只 boot 一次）；
	 * 其余 → START。
	 */
	static AudioSyncAction decideAudioSync(long intentAtRun, long taskTarget, long applied, boolean capturing) {
		if(intentAtRun != taskTarget) return AudioSyncAction.SKIP;
		if(taskTarget == 0) return capturing ? AudioSyncAction.STOP : AudioSyncAction.ALREADY;
		if(taskTarget == applied && capturing) return AudioSyncAction.ALREADY;
		return AudioSyncAction.START;
	}
	
	/**
	 * executor 线程：把实际捕获会话对齐到**最新**意图。
	 * 去抖关键：audioWindowHandle != target 的任务直接 no-op——start all 连发 N 个
	 * 意图时只有最后一个真正执行 PipeWire 会话建立（旧实现 N 次全套同步 stop+boot，
	 * 每次最坏 25s 预算）；重复提交天然幂等（已应用且仍在运行则跳过）。
	 */
	private void syncAudioState(long target, long prev, @Nullable String title, @Nullable String appId) {
		AudioCaptureManager capture = audioCapture;
		if(capture == null) return;
		// 以执行时刻的意图为准（coalesce）——见 decideAudioSync 裁决表
		switch(decideAudioSync(audioWindowHandle, target, audioAppliedHandle, capture.isStarted())) {
			case SKIP:
			case ALREADY:
				return;
			case STOP: {
				capture.stop();
				audioAppliedHandle = 0;
				return;
			}
			case START: {
				boolean ok = capture.start(target, title, appId);
				if(ok) {
					audioAppliedHandle = target;
					LOGGER.info("Audio sharing enabled for 0x{}", Long.toHexString(target));
				}
				else {
					// P2-18 语义（start 失败回滚跟随目标）平移进 executor。
					// 旧实现回滚后并不重启捕获，语义保持：回滚仅修正归属，不做第二次 boot。
					if(audioWindowHandle == target) {
						audioWindowHandle = prev;
					}
					audioAppliedHandle = 0;
					LOGGER.info("Audio capture start failed for 0x{} (audio stays on 0x{})",
						Long.toHexString(target), Long.toHexString(prev));
				}
				return;
			}
		}
	}

	@Nullable
	private WLCToplevel getLocalWindow(long windowHandle) {
		if(clientMod == null || clientMod.bridge == null) {
			return null;
		}
		return clientMod.bridge.getToplevel(windowHandle);
	}
	
	private void handleDisconnect() {
		shareStates.clear();
		diffUpdateManager.clear();
		frameRateController.clear();
		adaptiveFpsFactor = 1.0f;
		bytesSentThisSecond = 0;
		ImageCapture.clearAllDiffCaches();
		ImageCapture.cleanupAllWindowResources();
		ImageCapture.cleanupAllRawSlots();
		audioWindowHandle = 0;
		if(audioCapture != null) {
			// ⑧ 断线时的 PipeWire 停会话同样走 executor（旧实现在客户端网络线程同步 join，
			// 卡顿断线界面）。audioCapture 不再置 null：置 null 会让 executor 里的
			// stop 任务读到 null 而跳过，PipeWire 会话泄漏——幂等 stop 由 isStarted 把关。
			requestAudioSync(0, 0, null, null);
		}
		LOGGER.info("Cleared all share states due to disconnect");
	}
	
	@Nullable
	public ShareState getShareState(long windowHandle) {
		return shareStates.get(windowHandle);
	}
	
	public Map<Long, ShareState> getAllShareStates() {
		return Map.copyOf(shareStates);
	}
	
	public void setCaptureConfig(ImageCapture.CaptureConfig config) {
		this.captureConfig = config;
		// ⑨ 手动画质/帧率变更即复位自适应画质（新配置档成为基准，避免环状态覆盖手动档）
		this.adaptiveQuality = -1f;
		this.adaptiveQualityDownStreak = 0;
		this.adaptiveQualityUpStreak = 0;
		LOGGER.info("Updated capture config: {}", config.getSummary());
	}
	
	public void setPerWindowConfig(long windowHandle, ImageCapture.CaptureConfig config) {
		ShareState state = shareStates.get(windowHandle);
		if(state == null) {
			LOGGER.warn("Cannot set per-window config: window 0x{} not shared", Long.toHexString(windowHandle));
			return;
		}
		state.perWindowConfig = config;
		LOGGER.info("Set per-window config for 0x{}: {}", Long.toHexString(windowHandle), config.getSummary());
	}
	
	public void clearPerWindowConfig(long windowHandle) {
		ShareState state = shareStates.get(windowHandle);
		if(state != null) {
			state.perWindowConfig = null;
			LOGGER.info("Cleared per-window config for 0x{}", Long.toHexString(windowHandle));
		}
	}
	
	public void setSharingEnabled(boolean enabled) {
		this.sharingEnabled = enabled;
		LOGGER.info("Sharing {}", enabled ? "enabled" : "disabled");
	}
	
	public boolean isSharingEnabled() {
		return sharingEnabled;
	}
	
	/**
	 * 获取自适应帧率因子（供stats命令使用；1.0 = 满目标帧率）
	 */
	public float getAdaptiveFpsFactor() {
		return adaptiveFpsFactor;
	}
	
	/**
	 * 获取当前码率利用率（0.0-1.0+）
	 */
	public float getBitrateUtilization() {
		if(captureConfig == null || captureConfig.maxBitrate <= 0) return 0;
		long maxBytesPerSecond = (long)captureConfig.maxBitrate * 1000 / 8;
		return maxBytesPerSecond > 0 ? (float)bytesSentThisSecond / maxBytesPerSecond : 0;
	}
	
	public String getStats() {
		long totalFrames = shareStates.values().stream().mapToLong(s -> s.frameCount).sum();
		long totalBytes = shareStates.values().stream().mapToLong(s -> s.totalBytes).sum();
		long totalSkipped = shareStates.values().stream().mapToLong(s -> s.skippedFrames).sum();
		long totalRateLimited = shareStates.values().stream().mapToLong(s -> s.rateLimitedFrames).sum();
		long totalDegraded = shareStates.values().stream().mapToLong(s -> s.degradedFrames).sum();
		long totalSizeDropped = shareStates.values().stream().mapToLong(s -> s.sizeDroppedFrames).sum();
		
		return String.format("Windows: %d, Frames: %d, Skipped: %d, RateLimited: %d, Degraded: %d, SizeDropped: %d, Bytes: %d, FpsFactor: %.2f, Quality: %s, Utilization: %.1f%%",
			shareStates.size(), totalFrames, totalSkipped, totalRateLimited, totalDegraded, totalSizeDropped, totalBytes,
			adaptiveFpsFactor, adaptiveQuality < 0 ? "config" : String.format("%.2f (adaptive)", adaptiveQuality), getBitrateUtilization() * 100);
	}
	
	/**
	 * 共享状态
	 */
	public static class ShareState {
		/** 窗口像素来源：wayland 窗口（framebuffer 抓帧）还是 X11 窗口（XGetImage 抓帧） */
		public enum Source { WAYLAND, X11 }

		public final long windowHandle;
		public final String windowTitle;
		// P2-18：非 final——音频回落候选按"最近启动"选择；单测需要可控注入该时刻
		public long startTime;

		public final Source source;

		// === X11 共享专用 ===
		public final String x11Display;   // X display（如 ":2"），仅 X11 源
		public final long x11Xid;         // X window id（== windowHandle），仅 X11 源
		public volatile int x11Width = 0;      // 最近一次抓帧的原始窗口宽
		public volatile int x11Height = 0;     // 最近一次抓帧的原始窗口高
		public volatile int x11RootX = 0;      // 窗口原点在根窗口的 X（交互注入用）
		public volatile int x11RootY = 0;      // 窗口原点在根窗口的 Y（交互注入用）
		public volatile String x11AppId = null;
		public volatile int x11Pid = 0;

		public long lastUpdateTime = 0;
		public long lastFrameSentTime = 0;   // 最近一次实际发送帧的时间（心跳帧用）
		public long frameCount = 0;
		public long totalBytes = 0;
		public long skippedFrames = 0;      // diff检测跳过的帧数
		/** ⑧ v1.4.8：编码在途帧占位时丢弃的新帧数（编码跟不上的自然背压计数，诊断用） */
		public long encodeDroppedFrames = 0;
		/** ⑧ 每窗口至多一帧在途编码（仅渲染线程读写） */
		volatile boolean encodeBusy = false;
		public long rateLimitedFrames = 0;   // 码率限制跳过的帧数
		public long degradedFrames = 0;      // 因超限降级后实际发送的帧数
		public long sizeDroppedFrames = 0;   // 降级后仍超限被丢弃的帧数
		public int currentFps = 0;           // 当前实际帧率
		public long currentBitrate = 0;      // 当前码率 (kbps)

		public ImageCapture.CaptureConfig perWindowConfig = null;

		public ShareState(long windowHandle, String windowTitle) {
			this(windowHandle, windowTitle, Source.WAYLAND, null, windowHandle);
		}

		public ShareState(long windowHandle, String windowTitle, Source source, String x11Display, long x11Xid) {
			this.windowHandle = windowHandle;
			this.windowTitle = windowTitle;
			this.source = source;
			this.x11Display = x11Display;
			this.x11Xid = x11Xid;
			this.startTime = System.currentTimeMillis();
		}

		public ImageCapture.CaptureConfig getEffectiveConfig(ImageCapture.CaptureConfig globalConfig) {
			return perWindowConfig != null ? perWindowConfig : globalConfig;
		}
	}
}
