package dev.evvie.waylandcraft.shared;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.List;

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
 * 匹配不到 PID（原生 Wayland 窗口）时不启动捕获 —— 无声但共享画面不受影响。
 */
public class AudioCaptureManager {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-audio-capture");
	
	/** 单包 PCM 上限（30KB，远低于协议包上限，避免大包卡服务器） */
	private static final int MAX_PACKET_BYTES = 30_000;
	
	/** poll 间隔：每 100ms 拉一次 PCM 并发送 */
	private static final long POLL_INTERVAL_MS = 100;
	
	private final WaylandCraft clientMod;
	
	/** handle -> 发送序号 */
	private long lastPollTime = 0;
	private int seqCounter = 0;
	private boolean started = false;
	private boolean firstAudioLogged = false;
	private long totalAudioBytes = 0;
	private long nextAudioLogBytes = 1_000_000;
	
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
			LOGGER.warn("Audio capture: cannot resolve PID for window '{}' (appId={}) — audio sharing unavailable for this window",
				title, appId);
			return false;
		}
		
		try {
			clientMod.bridge.audioCaptureStart(pid);
		} catch(Throwable t) {
			LOGGER.error("Audio capture start failed: {}", t.toString());
			return false;
		}
		
		started = true;
		seqCounter = 0;
		lastPollTime = 0;
		firstAudioLogged = false;
		totalAudioBytes = 0;
		nextAudioLogBytes = 1_000_000;
		LOGGER.info("Audio capture started for window '{}' (pid={})", title, pid);
		return true;
	}
	
	/**
	 * 每帧调用（由 WindowShareManager.update 驱动）：周期 poll PCM 并发送。
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
		
		// 分包发送
		for(int offset = 0; offset < pcm.length; offset += MAX_PACKET_BYTES) {
			int len = Math.min(MAX_PACKET_BYTES, pcm.length - offset);
			byte[] chunk = new byte[len];
			System.arraycopy(pcm, offset, chunk, 0, len);
			
			SharedWindowAudioPayload payload = new SharedWindowAudioPayload(
				windowHandle(), seqCounter++, sampleRate, channels, chunk);
			ClientPlayNetworking.send(payload);
		}
	}
	
	/**
	 * 停止音频捕获。
	 */
	public void stop() {
		if(!started) return;
		started = false;
		if(clientMod != null && clientMod.bridge != null) {
			try {
				clientMod.bridge.audioCaptureStop();
			} catch(Throwable t) {
				LOGGER.warn("Audio capture stop failed", t);
			}
		}
		LOGGER.info("Audio capture stopped");
	}
	
	public boolean isStarted() {
		return started;
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
