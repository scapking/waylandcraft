package dev.evvie.waylandcraft.shared;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.List;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.bridge.WaylandCraftBridge;
import dev.evvie.waylandcraft.network.SharedWindowAudioPayload;
import dev.evvie.waylandcraft.utils.X11WindowLister;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking;

/**
 * 共享窗口音频捕获管理器（发送端）。
 * 
 * 流程：
 * 1. start(handle, title, appId)：用 X11 窗口枚举匹配出窗口所属进程 PID
 *    （X11 _NET_WM_PID），交给 native 按 PID 捕获 PipeWire 音频
 *    （只捕获该进程的声音，不是整机）。
 * 2. tick()：周期 poll native 累积的 PCM，分包成 SharedWindowAudioPayload 发送。
 * 3. stop()：停止 native 捕获。
 * 
 * 缓冲策略：客户端预缓冲 4-8s + 服务端缓冲 4-8s，解决卡顿问题
 * （参考 Discord Go Live 架构）。
 * 
 * 匹配不到 PID（原生 Wayland 窗口）时不启动捕获 —— 无声但共享画面不受影响。
 */
public class AudioCaptureManager {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-audio-capture");
	
	/** 单包 PCM 上限（30KB，远低于协议包上限，避免大包卡服务器） */
	private static final int MAX_PACKET_BYTES = 30_000;
	
	/** 缓冲目标：6 秒（范围 4-8 秒） */
	private static final int TARGET_BUFFER_MS = 6000;
	private static final int MIN_BUFFER_MS = 4000;
	private static final int MAX_BUFFER_MS = 8000;
	
	/** poll 间隔：每 100ms 拉一次 PCM 并发送 */
	private static final long POLL_INTERVAL_MS = 100;
	
	private final WaylandCraft clientMod;
	
	/** 缓冲管理器 */
	private final AudioBufferManager bufferManager = new AudioBufferManager();
	
	/** Opus 编码器 */
	private OpusEncoderWrapper opusEncoder;
	
	/** 编码调度器 */
	private final ScheduledExecutorService encodeExecutor = Executors.newSingleThreadScheduledExecutor(r -> {
		Thread t = new Thread(r, "AudioEncode");
		t.setDaemon(true);
		return t;
	});
	
	/** handle -> 发送序号 */
	private long lastPollTime = 0;
	private int seqCounter = 0;
	private boolean started = false;
	private boolean firstAudioLogged = false;
	private long totalAudioBytes = 0;
	private long nextAudioLogBytes = 1_000_000;
	
	// 全链路状态追踪（供 /wl audio status 查询）
	private volatile int resolvedPid = -1;        // 来源：窗口 → PID
	private volatile String sourceStage = "idle"; // 来源解析方式（wayland / x11 / none）
	private volatile String lastError = null;     // 最近一次失败原因
	private volatile long sentPackets = 0;        // 接口：已发送的音频包数
	private volatile long startedAt = 0;          // 捕获启动时间戳
	
	public AudioCaptureManager(WaylandCraft clientMod) {
		this.clientMod = clientMod;
	}
	
	/**
	 * 启动音频捕获（窗口 → PID → native）。
	 * 
	 * @return true 表示成功启动（捕获到音频）；false 表示无法定位进程（无声）
	 */
	public boolean start(long windowHandle, String title, String appId) {
		if(clientMod == null || clientMod.bridge == null) {
			return false;
		}
		if(started) {
			stop();
		}
		
		int pid = findPidForWindow(windowHandle, title, appId);
		if(pid <= 0) {
			lastError = "cannot resolve PID for window (no wayland client pid & no X11 _NET_WM_PID match)";
			sourceStage = "none";
			LOGGER.warn("Audio capture: cannot resolve PID for window '{}' (appId={}) — audio sharing unavailable for this window",
				title, appId);
			return false;
		}
		resolvedPid = pid;
		
		try {
			clientMod.bridge.audioCaptureStart(pid);
		} catch(Throwable t) {
			lastError = "audioCaptureStart failed: " + t.toString();
			LOGGER.error("Audio capture start failed: {}", t.toString());
			return false;
		}
		
		// 初始化 Opus 编码器
		opusEncoder = new OpusEncoderWrapper();
		opusEncoder.configure(48000, 2, 64000); // 48kHz 立体声 64kbps
		
		started = true;
		startedAt = System.currentTimeMillis();
		seqCounter = 0;
		lastPollTime = 0;
		firstAudioLogged = false;
		totalAudioBytes = 0;
		sentPackets = 0;
		nextAudioLogBytes = 1_000_000;
		lastError = null;
		
		// 启动编码循环
		startEncodeLoop();
		
		LOGGER.info("Audio capture started for window '{}' (pid={}, source={})", title, pid, sourceStage);
		return true;
	}
	
	private void startEncodeLoop() {
		encodeExecutor.scheduleAtFixedRate(() -> {
			if (!started) return;
			
			AudioBufferManager.AudioFrame frame = bufferManager.pollFrame();
			if (frame == null) return;
			
			byte[] encoded = opusEncoder.encodeFrame(frame.data());
			if (encoded.length > 0) {
				AudioBufferManager.EncodedAudioPacket packet = new AudioBufferManager.EncodedAudioPacket(
					encoded, frame.timestampMs(), seqCounter++
				);
				bufferManager.enqueuePacket(packet);
			}
		}, 0, 20, TimeUnit.MILLISECONDS); // 20ms = 50fps
	}
	
	/**
	 * 每帧调用（由 WindowShareManager.update 驱动）：周期 poll PCM 并发送编码包。
	 */
	public void tick() {
		if(!started || clientMod == null || clientMod.bridge == null) return;
		
		long now = System.currentTimeMillis();
		if(now - lastPollTime < POLL_INTERVAL_MS) return;
		lastPollTime = now;
		
		byte[] data;
		try {
			data = clientMod.bridge.audioCapturePoll();
		} catch(Throwable t) {
			LOGGER.error("Audio capture poll failed", t);
			return;
		}
		if(data == null || data.length <= 8) return;
		
		ByteBuffer buf = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN);
		int sampleRate = buf.getInt();
		int channels = buf.getInt();
		byte[] pcm = new byte[data.length - 8];
		System.arraycopy(data, 8, pcm, 0, pcm.length);
		
		if(sampleRate <= 0 || channels <= 0 || pcm.length == 0) return;
		
		if(!firstAudioLogged) {
			LOGGER.info("Audio capture: first PCM received ({} bytes, {} Hz, {} ch) — capture pipeline LIVE",
				pcm.length, sampleRate, channels);
			firstAudioLogged = true;
		}
		totalAudioBytes += pcm.length;
		if(totalAudioBytes >= nextAudioLogBytes) {
			LOGGER.info("Audio capture: {} bytes streamed so far ({} Hz, {} ch)", totalAudioBytes, sampleRate, channels);
			nextAudioLogBytes += 1_000_000;
		}
		
		// 将 PCM 放入缓冲管理器（编码循环会异步处理）
		bufferManager.enqueueFrame(pcm, System.currentTimeMillis(), sampleRate, channels);
		
		// 发送编码后的音频包
		AudioBufferManager.EncodedAudioPacket packet;
		while ((packet = bufferManager.pollPacket()) != null) {
			SharedWindowAudioPayload payload = new SharedWindowAudioPayload(
				windowHandle(), packet.sequence(), 48000, 2, packet.data()
			);
			ClientPlayNetworking.send(payload);
			sentPackets++;
		}
	}
	
	/**
	 * 停止音频捕获。
	 */
	public void stop() {
		if(!started) return;
		started = false;
		
		// 关闭编码器
		if (opusEncoder != null) {
			opusEncoder.shutdown();
			opusEncoder = null;
		}
		
		// 关闭编码调度器
		encodeExecutor.shutdown();
		try {
			if (!encodeExecutor.awaitTermination(2, TimeUnit.SECONDS)) {
				encodeExecutor.shutdownNow();
			}
		} catch (InterruptedException e) {
			Thread.currentThread().interrupt();
			encodeExecutor.shutdownNow();
		}
		
		if(clientMod != null && clientMod.bridge != null) {
			try {
				clientMod.bridge.audioCaptureStop();
			} catch(Throwable t) {
				LOGGER.warn("Audio capture stop failed", t);
			}
		}
		bufferManager.reset();
		LOGGER.info("Audio capture stopped (total {} bytes, {} packets)", totalAudioBytes, sentPackets);
	}
	
	public boolean isStarted() {
		return started;
	}
	
	/**
	 * 发送端全链路状态（供 /wl audio status 展示）。
	 * 覆盖：来源(PID) → 捕获(是否已启动 native) → 缓冲/编码状态 + native 侧状态。
	 */
	public String getStatusSummary() {
		StringBuilder sb = new StringBuilder();
		sb.append("发送端 (capture):\n");
		sb.append("  started: ").append(started).append("\n");
		sb.append("  pid: ").append(resolvedPid > 0 ? resolvedPid : "N/A").append("\n");
		sb.append("  pid source: ").append(sourceStage).append("\n");
		sb.append("  window: 0x").append(Long.toHexString(activeWindowHandle)).append("\n");
		sb.append("  bytes streamed: ").append(totalAudioBytes).append("\n");
		sb.append("  packets sent: ").append(sentPackets).append("\n");
		if(startedAt > 0) {
			sb.append("  uptime: ").append((System.currentTimeMillis() - startedAt) / 1000).append("s\n");
		}
		if(lastError != null) {
			sb.append("  last error: ").append(lastError).append("\n");
		}
		// 缓冲状态
		AudioBufferManager.BufferState bufState = bufferManager.getState();
		sb.append("  buffer: ").append(bufState.bufferedMs).append("ms queued, ")
		  .append(bufState.queuedFrames).append(" frames, underrun=").append(bufState.underrun)
		  .append(", overrun=").append(bufState.overrun).append("\n");
		// 编码状态
		if (opusEncoder != null) {
			sb.append("  encoder: Opus ").append(opusEncoder.getSampleRate()).append("Hz ")
			  .append(opusEncoder.getChannels()).append("ch ").append(opusEncoder.getBitrate()).append("bps\n");
		}
		// native 侧链路状态（JSON）
		if(clientMod != null && clientMod.bridge != null) {
			try {
				String nativeStatus = clientMod.bridge.audioCaptureStatus();
				sb.append("  native: ").append(nativeStatus).append("\n");
			} catch(Throwable t) {
				sb.append("  native: unavailable (").append(t.toString()).append(")\n");
			}
		}
		return sb.toString();
	}
	
	/**
	 * 解析窗口所属进程 PID。两条路：
	 * 
	 * 1. 原生 wayland 窗口（Firefox 等）：xdg_toplevel 没有 X11 的 _NET_WM_PID，
	 *    compositor 直接通过 SO_PEERCRED（wl_client_get_credentials）拿到连
	 *    wayland socket 的客户端 PID —— 对原生 wayland 窗口这是唯一可靠的 PID 来源。
	 *    注意 windowHandle 是 xdg_toplevel 的 handle（wayland 共享）或 xid（X11 共享）；
	 *    传 xid 时 toplevelPid 查不到会返回 0，自动落到下面的 X11 枚举。
	 * 
	 * 2. X11 窗口枚举（_NET_WM_PID）：共享窗口运行在 waylandcraft 自己的
	 *    xwayland-satellite X display 上（由 native 启动，号是动态的，如 ":2"）。
	 *    必须显式连 satellite display 枚举，用 Minecraft 进程自己的 DISPLAY 会
	 *    枚举到空/宿主桌面 → PID 永远解析失败 → 无声。
	 * 
	 * @return PID；找不到返回 0
	 */
	private int findPidForWindow(long windowHandle, String title, String appId) {
		// 1. 原生 wayland 窗口：直接问 compositor（SO_PEERCRED）
		if(clientMod != null && clientMod.bridge != null && windowHandle != 0) {
			try {
				int pid = clientMod.bridge.toplevelPid(windowHandle);
				if(pid > 0) {
					sourceStage = "wayland(SO_PEERCRED)";
					LOGGER.info("Audio capture: wayland client pid={} for window '{}'", pid, title);
					return pid;
				}
			} catch(Throwable t) {
				LOGGER.debug("Audio capture: toplevelPid failed for window '{}'", title, t);
			}
		}
		
		String satelliteDisplay = null;
		if(clientMod != null && clientMod.bridge != null) {
			try {
				String d = clientMod.bridge.getSatelliteDisplay();
				if(d != null && !d.isEmpty()) satelliteDisplay = d;
			} catch(Throwable t) {
				LOGGER.debug("Failed to query satellite display", t);
			}
		}
		List<X11WindowLister.WindowInfo> windows = X11WindowLister.getDesktopWindows(satelliteDisplay);
		if(windows.isEmpty()) {
			LOGGER.debug("No X11 windows on display '{}'", satelliteDisplay);
			return 0;
		}
		
		X11WindowLister.WindowInfo best = null;
		int bestScore = 0;
		
		for(X11WindowLister.WindowInfo w : windows) {
			int score = 0;
			boolean titleMatch = title != null && !title.isEmpty() && title.equals(w.title);
			boolean appMatch = appId != null && !appId.isEmpty() && appId.equalsIgnoreCase(w.appId);
			
			if(titleMatch && appMatch) score = 3;
			else if(titleMatch) score = 2;
			else if(appMatch) score = 1;
			
			if(score > bestScore && w.pid > 0) {
				bestScore = score;
				best = w;
			}
		}
		
		if(best == null) {
			LOGGER.debug("No X11 window matches '{}' / '{}' ({} windows listed)", title, appId, windows.size());
			return 0;
		}
		sourceStage = "x11(_NET_WM_PID)";
		return best.pid;
	}
	
	// 由 WindowShareManager 在切换共享窗口时更新
	private long activeWindowHandle = 0;
	
	public void setActiveWindow(long windowHandle) {
		this.activeWindowHandle = windowHandle;
	}
	
	private long windowHandle() {
		return activeWindowHandle;
	}
}
