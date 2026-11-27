package dev.evvie.waylandcraft.shared;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.List;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.bridge.WaylandCraftBridge;
import dev.evvie.waylandcraft.network.SharedWindowAudioPayload;
import dev.evvie.waylandcraft.utils.X11WindowLister;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking;

/**
 * 共享窗口音频捕获管理器（发送端）。
 *
 * 流程：
 * 1. start(handle, title, appId)：解析窗口所属进程 PID（SO_PEERCRED /
 *    X11 _NET_WM_PID），交给 native。native 优先按 PID 直连该应用自己的
 *    PipeWire 播放流节点捕获（mode=per-app，"只要窗口的声音"）；应用没有
 *    播放流节点或 PID 未知时降级整机 sink monitor（mode=monitor-fallback，
 *    声音是整机会话）；两者都不行才算启动失败。
 * 2. tick()：周期 poll native 累积的 PCM，分包成 SharedWindowAudioPayload
 *    发送；并周期同步 native 状态 JSON（watchdog 可能随时重连/降级/升级，
 *    mode 会变）。
 * 3. stop()：停止 native 捕获。
 *
 * 实际捕获模式/目标节点以 native 状态 JSON 为准（audio_capture.rs），
 * getStatusSummary 透出 mode / target node / fallback reason。
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
	/** ⑧ v1.4.8：start/stop 挪到 waylandcraft-share-audio executor 线程，
	 *  tick()/isStarted() 仍在主线程读 —— 跨线程可见性必须 volatile */
	private volatile boolean started = false;
	private boolean firstAudioLogged = false;
	private long totalAudioBytes = 0;
	private long nextAudioLogBytes = 1_000_000;

	// 全链路状态追踪（供 /wl audio status 查询）
	private volatile int resolvedPid = -1;        // 来源：窗口 → PID
	private volatile String sourceStage = "idle"; // 来源解析方式（wayland / x11 / none）
	private volatile String lastError = null;     // 最近一次失败原因
	private volatile long sentPackets = 0;        // 接口：已发送的音频包数
	private volatile long startedAt = 0;          // 捕获启动时间戳

	// native 捕获模式（audio_capture.rs 状态 JSON 透出；watchdog 会动态改）：
	// mode = per-app | monitor-fallback | none，per-app 时显示选中的 node.name
	private volatile String nativeMode = "none";
	private volatile int nativeTargetNode = 0;
	private volatile String nativeTargetName = "";
	private volatile String nativeFallbackReason = null;
	private volatile long lastNativeSync = 0;

	/** native 状态同步节流（per-app/monitor 切换要能在 /wl audio status 看到） */
	private static final long NATIVE_SYNC_INTERVAL_MS = 2000;
	
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
		
		// ⑦ per-app 捕获下 pid 是捕获目标本身（native 按 pid 直连该应用的
		// PipeWire 播放流节点）。PID 解析失败不再放弃启动：native 会以
		// pid=0 走 monitor-fallback 整机捕获（此前"没声音"的根因之一），
		// 但声音内容退化为整机会话——mode 会如实透出。
		int pid = findPidForWindow(windowHandle, title, appId);
		if(pid <= 0) {
			LOGGER.warn("Audio capture: cannot resolve PID for window '{}' (appId={}) — native will use monitor fallback (sound is the whole session, not the window's app)",
				title, appId);
			sourceStage = "monitor(no-pid)";
		}
		resolvedPid = Math.max(0, pid);

		try {
			clientMod.bridge.audioCaptureStart(resolvedPid);
		} catch(Throwable t) {
			lastError = "audioCaptureStart failed: " + t.toString();
			LOGGER.error("Audio capture start failed: {}", t.toString());
			nativeMode = "none";
			return false;
		}

		// 立即同步 native 实际选择的模式（per-app 直连成功？还是 monitor 降级？）
		syncNativeStatus(true);
		if("per-app".equals(nativeMode)) {
			LOGGER.info("Audio capture mode=per-app: direct link to app stream node {} \"{}\" (window's own audio)",
				nativeTargetNode, nativeTargetName);
		} else if("monitor-fallback".equals(nativeMode)) {
			LOGGER.warn("Audio capture mode=monitor-fallback: machine-wide audio, not the window's app — reason: {}",
				nativeFallbackReason);
		}
		
		started = true;
		startedAt = System.currentTimeMillis();
		seqCounter = 0;
		lastPollTime = 0;
		firstAudioLogged = false;
		totalAudioBytes = 0;
		sentPackets = 0;
		nextAudioLogBytes = 1_000_000;
		lastError = null;
		LOGGER.info("Audio capture started for window '{}' (pid={}, source={})", title, pid, sourceStage);
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

		// 周期同步 native 模式：watchdog 重连/降级/升级都会反映到这里
		if(now - lastNativeSync >= NATIVE_SYNC_INTERVAL_MS) {
			syncNativeStatus(false);
		}
		
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
		
		// 分包发送。⑦ 每个 chunk 的 captureTs = 本次 poll 时刻回推该 chunk 在
		// PCM 时间线上的样本位置（poll 到点收集了 [now-span, now) 的音频），
		// 使接收端音视频共用同一 playout 时钟。
		int bytesPerSecond = sampleRate * channels * 2;
		for(int offset = 0; offset < pcm.length; offset += MAX_PACKET_BYTES) {
			int len = Math.min(MAX_PACKET_BYTES, pcm.length - offset);
			byte[] chunk = new byte[len];
			System.arraycopy(pcm, offset, chunk, 0, len);
			long chunkCaptureTs = now - (long)(pcm.length - offset) * 1000 / Math.max(1, bytesPerSecond);
			
			SharedWindowAudioPayload payload = new SharedWindowAudioPayload(
				windowHandle(), seqCounter++, sampleRate, channels, chunk, chunkCaptureTs);
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
		if(clientMod != null && clientMod.bridge != null) {
			try {
				clientMod.bridge.audioCaptureStop();
			} catch(Throwable t) {
				LOGGER.warn("Audio capture stop failed", t);
			}
		}
		nativeMode = "none";
		nativeTargetNode = 0;
		nativeTargetName = "";
		nativeFallbackReason = null;
		LOGGER.info("Audio capture stopped (total {} bytes, {} packets)", totalAudioBytes, sentPackets);
	}

	/**
	 * 读取并解析 native 状态 JSON（audio_capture.rs get_audio_capture_status）。
	 * 失败不影响捕获本身，只是状态过期。force=true 忽略节流。
	 */
	private void syncNativeStatus(boolean force) {
		if(clientMod == null || clientMod.bridge == null) return;
		long now = System.currentTimeMillis();
		if(!force && now - lastNativeSync < NATIVE_SYNC_INTERVAL_MS) return;
		try {
			String json = clientMod.bridge.audioCaptureStatus();
			if(json == null || json.isEmpty()) return;
			JsonObject o = JsonParser.parseString(json).getAsJsonObject();
			String prev = nativeMode;
			nativeMode = strOr(o, "mode", "none");
			nativeTargetNode = o.has("target_node") && !o.get("target_node").isJsonNull()
				? o.get("target_node").getAsInt() : 0;
			nativeTargetName = strOr(o, "target_name", "");
			nativeFallbackReason = o.has("fallback_reason") && !o.get("fallback_reason").isJsonNull()
				? o.get("fallback_reason").getAsString() : null;
			lastNativeSync = now;
			if(started && !nativeMode.equals(prev)) {
				// 模式切换（watchdog 重连/降级/升级）值得单独一行日志
				LOGGER.info("Audio capture mode changed: {} -> {} (node {} \"{}\"){}",
					prev, nativeMode, nativeTargetNode, nativeTargetName,
					nativeFallbackReason != null ? " reason: " + nativeFallbackReason : "");
			}
		} catch(Throwable t) {
			LOGGER.debug("Audio capture: native status sync failed", t);
		}
	}

	private static String strOr(JsonObject o, String key, String fallback) {
		return o.has(key) && !o.get(key).isJsonNull() ? o.get(key).getAsString() : fallback;
	}
	
	public boolean isStarted() {
		return started;
	}
	
	/**
	 * 发送端全链路状态（供 /wl audio status 展示）。
	 * 覆盖：来源(PID) → 捕获(是否已启动 native) → 接口(已发字节/包数) + native 侧状态。
	 */
	public String getStatusSummary() {
		StringBuilder sb = new StringBuilder();
		sb.append("发送端 (capture):\n");
		sb.append("  started: ").append(started).append("\n");
		sb.append("  pid: ").append(resolvedPid > 0 ? resolvedPid : "N/A").append("\n");
		sb.append("  pid source: ").append(sourceStage).append("\n");
		if(System.currentTimeMillis() - lastNativeSync > NATIVE_SYNC_INTERVAL_MS) {
			syncNativeStatus(false);
		}
		sb.append("  capture mode: ").append(started ? nativeMode : "none").append("\n");
		if(started && nativeTargetNode > 0) {
			sb.append("  capture node: ").append(nativeTargetNode)
				.append(" \"").append(nativeTargetName).append("\"\n");
		}
		if(started && nativeFallbackReason != null) {
			sb.append("  fallback reason: ").append(nativeFallbackReason).append("\n");
		}
		sb.append("  window: 0x").append(Long.toHexString(activeWindowHandle)).append("\n");
		sb.append("  bytes streamed: ").append(totalAudioBytes).append("\n");
		sb.append("  packets sent: ").append(sentPackets).append("\n");
		if(startedAt > 0) {
			sb.append("  uptime: ").append((System.currentTimeMillis() - startedAt) / 1000).append("s\n");
		}
		if(lastError != null) {
			sb.append("  last error: ").append(lastError).append("\n");
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
