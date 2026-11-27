package dev.evvie.waylandcraft.shared;

import java.lang.management.ManagementFactory;
import java.lang.management.MemoryMXBean;
import java.lang.management.ThreadMXBean;
import java.util.Map;
import java.util.UUID;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;

/**
 * 调试工具
 * 提供共享窗口功能的调试和监控功能
 */
public class SharedWindowDebug {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-debug");
	
	// 性能监控
	private static long lastFpsCheckTime = 0;
	private static long frameCount = 0;
	private static float currentFps = 0;
	
	// 内存监控
	private static long lastMemoryCheckTime = 0;
	private static long usedMemory = 0;
	private static long maxMemory = 0;
	
	/**
	 * 初始化调试工具
	 */
	public static void init() {
		LOGGER.info("SharedWindowDebug initialized");
	}
	
	/**
	 * 更新性能统计
	 */
	public static void update() {
		frameCount++;
		
		long now = System.currentTimeMillis();
		if(now - lastFpsCheckTime >= 1000) {
			currentFps = frameCount * 1000.0f / (now - lastFpsCheckTime);
			frameCount = 0;
			lastFpsCheckTime = now;
		}
		
		if(now - lastMemoryCheckTime >= 5000) {
			updateMemoryStats();
			lastMemoryCheckTime = now;
		}
	}
	
	/**
	 * 更新内存统计
	 */
	private static void updateMemoryStats() {
		MemoryMXBean memoryBean = ManagementFactory.getMemoryMXBean();
		usedMemory = memoryBean.getHeapMemoryUsage().getUsed() / 1024 / 1024; // MB
		maxMemory = memoryBean.getHeapMemoryUsage().getMax() / 1024 / 1024; // MB
	}
	
	/**
	 * 获取性能报告
	 */
	public static String getPerformanceReport() {
		return String.format(
			"FPS: %.1f, Memory: %d/%d MB, Threads: %d",
			currentFps, usedMemory, maxMemory,
			ManagementFactory.getThreadMXBean().getThreadCount()
		);
	}
	
	/**
	 * 获取客户端状态报告
	 */
	public static String getClientStatusReport() {
		WaylandCraft client = WaylandCraft.instance;
		if(client == null) {
			return "Client not initialized";
		}
		
		StringBuilder sb = new StringBuilder();
		sb.append("=== Client Status ===\n");
		sb.append("Bridge: ").append(client.bridge != null ? "Connected" : "Disconnected").append("\n");
		sb.append("Socket: ").append(client.waylandSocket).append("\n");
		sb.append("Local Windows: ").append(client.displays.size()).append("\n");
		sb.append("Shared Windows: ").append(client.sharedDisplays.size()).append("\n");
		
		if(client.remoteWindowRenderer != null) {
			sb.append("Remote Renderer: Active\n");
		}
		
		return sb.toString();
	}
	
	/**
	 * 获取服务器状态报告
	 */
	public static String getServerStatusReport() {
		WaylandCraftCommon server = WaylandCraftCommon.instance;
		if(server == null) {
			return "Server not initialized";
		}
		
		StringBuilder sb = new StringBuilder();
		sb.append("=== Server Status ===\n");
		sb.append("Shared Windows: ").append(server.sharedWindowManager.getWindowCount()).append("\n");
		
		// 权限统计
		Map<UUID, WindowPermission> whitelist = server.permissionManager.getWhitelist();
		sb.append("Whitelist: ").append(whitelist.size()).append(" players\n");

		java.util.Set<UUID> blacklist = server.permissionManager.getBlacklist();
		sb.append("Blacklist: ").append(blacklist.size()).append(" players\n");

		// ACL-2/5：新观众策略 + 授权生命周期计数（进状态报告）
		sb.append("Share default: policy=").append(server.permissionManager.getNewViewerPolicy().name().toLowerCase())
			.append(" level=").append(server.permissionManager.getDefaultPermission().name()).append("\n");
		sb.append(ShareAclAudit.reportBlock());
		
		// P3：帧/音频转发计数（frames/bytes/slow_viewer），排障时直接看出
		// 是"没帧上来"还是"发不出去（背压/无能力）"
		var frameRelay = dev.evvie.waylandcraft.network.WaylandCraftNetworking.getFrameRelay();
		sb.append("Video relay: frames=").append(frameRelay.getTotalFrames())
			.append(" bytes=").append(frameRelay.getTotalBytes() / 1024).append("KB")
			.append(" sends=").append(frameRelay.getTotalSends()).append("\n");
		sb.append("Video drops: oversized=").append(frameRelay.getDroppedOversized())
			.append(" bad_dim=").append(frameRelay.getDroppedBadDimensions())
			.append(" no_capability=").append(frameRelay.getDroppedNoCapability())
			.append(" skip_unwritable=").append(frameRelay.getSkippedUnwritable())
			.append(" slow_viewers=").append(frameRelay.getSlowViewerEvents())
			.append(" unsubscribed=").append(frameRelay.getSkippedNotSubscribed())
			.append(" load_level=").append(frameRelay.getSkippedLoadLevel())
			.append(" over_limit=").append(frameRelay.getSkippedOverLimit())
			.append(" mem_guard=").append(frameRelay.getDroppedMemoryGuard()).append("\n");
		
		var audioRelay = dev.evvie.waylandcraft.network.WaylandCraftNetworking.getAudioRelay();
		sb.append("Audio relay: packets=").append(audioRelay.getTotalPackets())
			.append(" bytes=").append(audioRelay.getTotalBytes() / 1024).append("KB")
			.append(" sends=").append(audioRelay.getTotalSends()).append("\n");
		sb.append("Audio drops: invalid=").append(audioRelay.getDroppedInvalid())
			.append(" no_capability=").append(audioRelay.getDroppedNoCapability())
			.append(" skip_unwritable=").append(audioRelay.getSkippedUnwritable())
			.append(" slow_viewers=").append(audioRelay.getSlowViewerEvents())
			.append(" unsubscribed=").append(audioRelay.getSkippedNotSubscribed())
			.append(" load_level=").append(audioRelay.getSkippedLoadLevel())
			.append(" over_limit=").append(audioRelay.getSkippedOverLimit())
			.append(" mem_guard=").append(audioRelay.getDroppedMemoryGuard()).append("\n");

		// 任务A-2：观众分级状态（L0 normal / L1 half / L2 keyframe / L3 paused）。
		// 汇总 + 逐观众行（封顶 32 行防刷屏），排障时直接看出谁被降载、降到哪级。
		Map<UUID, Integer> viewerLevels = dev.evvie.waylandcraft.network.ViewerLoadController.snapshot();
		int[] levelCounts = new int[4];
		for (int level : viewerLevels.values()) {
			if (level >= 0 && level < levelCounts.length) levelCounts[level]++;
		}
		sb.append("Viewer load: L0=").append(levelCounts[0])
			.append(" L1=").append(levelCounts[1])
			.append(" L2=").append(levelCounts[2])
			.append(" L3=").append(levelCounts[3]).append("\n");
		int shownViewers = 0;
		for (Map.Entry<UUID, Integer> e : viewerLevels.entrySet()) {
			if (shownViewers++ >= 32) {
				sb.append("  … +").append(viewerLevels.size() - 32).append(" more viewers\n");
				break;
			}
			sb.append("  viewer ").append(e.getKey().toString(), 0, 8)
				.append(": L").append(e.getValue())
				.append(" (").append(dev.evvie.waylandcraft.network.ViewerLoadController.levelName(e.getValue())).append(")\n");
		}

		// 任务A-1：订阅上限状态
		sb.append("Viewer caps: tracked=").append(dev.evvie.waylandcraft.network.ViewerSubscriptions.trackedViewers())
			.append(" cap=").append(dev.evvie.waylandcraft.network.ViewerSubscriptions.MAX_WINDOWS_PER_VIEWER)
			.append("/viewer denied=").append(dev.evvie.waylandcraft.network.ViewerSubscriptions.totalDenied()).append("\n");

		// 任务A-3：全局内存守卫水位
		long videoBytes = dev.evvie.waylandcraft.network.RelayMemoryGuard.videoBufferedBytes();
		long audioBytes = dev.evvie.waylandcraft.network.RelayMemoryGuard.audioBufferedBytes();
		sb.append("Memory guard: video=").append(videoBytes / 1024).append("KB")
			.append(" audio=").append(audioBytes / 1024).append("KB")
			.append(" / limit=").append(dev.evvie.waylandcraft.network.RelayMemoryGuard.GLOBAL_BUFFER_BYTES_LIMIT / (1024 * 1024)).append("MB")
			.append(" over_limit=").append(dev.evvie.waylandcraft.network.RelayMemoryGuard.isOverLimit()).append("\n");

		return sb.toString();
	}
	
	/**
	 * 获取窗口详情
	 */
	public static String getWindowDetails(long windowHandle) {
		WaylandCraftCommon server = WaylandCraftCommon.instance;
		if(server == null) {
			return "Server not initialized";
		}
		
		SharedWindowEntry entry = server.sharedWindowManager.getWindow(windowHandle);
		if(entry == null) {
			return "Window not found: 0x" + Long.toHexString(windowHandle);
		}
		
		StringBuilder sb = new StringBuilder();
		sb.append("=== Window Details ===\n");
		sb.append("Handle: 0x").append(Long.toHexString(windowHandle)).append("\n");
		sb.append("Title: ").append(entry.getWindowTitle()).append("\n");
		sb.append("Owner: ").append(entry.getOwnerUUID()).append("\n");
		sb.append("Position: ").append(entry.getX()).append(", ").append(entry.getY()).append("\n");
		sb.append("Size: ").append(entry.getWidth()).append("x").append(entry.getHeight()).append("\n");
		sb.append("Visible: ").append(entry.isVisible()).append("\n");
		sb.append("Created: ").append(entry.getCreatedAt()).append("\n");
		
		// 权限列表
		sb.append("\nPermissions:\n");
		Map<UUID, WindowPermission> permissions = entry.getAllPermissions();
		for(Map.Entry<UUID, WindowPermission> perm : permissions.entrySet()) {
			sb.append("  ").append(perm.getKey()).append(": ").append(perm.getValue()).append("\n");
		}
		
		return sb.toString();
	}
	
	/**
	 * 打印调试信息到日志
	 */
	public static void printDebugInfo() {
		LOGGER.info("=== Debug Info ===");
		LOGGER.info("Performance: {}", getPerformanceReport());
		LOGGER.info("Client: {}", getClientStatusReport());
		LOGGER.info("Server: {}", getServerStatusReport());
	}
	
	/**
	 * 验证共享窗口状态
	 * @return 如果有问题返回错误信息，否则返回null
	 */
	public static String validateState() {
		WaylandCraft client = WaylandCraft.instance;
		WaylandCraftCommon server = WaylandCraftCommon.instance;
		
		if(client == null && server == null) {
			return "Neither client nor server initialized";
		}
		
		if(client != null) {
			// 检查客户端状态
			if(client.bridge == null) {
				return "Client bridge not initialized";
			}
			
			if(client.remoteWindowRenderer == null) {
				return "Remote renderer not initialized";
			}
		}
		
		if(server != null) {
			// 检查服务器状态
			if(server.sharedWindowManager == null) {
				return "Shared window manager not initialized";
			}
			
			if(server.permissionManager == null) {
				return "Permission manager not initialized";
			}
		}
		
		return null; // 没有问题
	}
	
	/**
	 * 运行自检
	 * @return 自检结果
	 */
	public static String selfTest() {
		StringBuilder sb = new StringBuilder();
		sb.append("=== Self Test ===\n");
		
		// 检查Java版本
		sb.append("Java Version: ").append(System.getProperty("java.version")).append("\n");
		
		// 检查内存
		Runtime runtime = Runtime.getRuntime();
		long totalMemory = runtime.totalMemory() / 1024 / 1024;
		long freeMemory = runtime.freeMemory() / 1024 / 1024;
		sb.append("Memory: ").append(freeMemory).append("MB free of ").append(totalMemory).append("MB\n");
		
		// 检查线程
		ThreadMXBean threadBean = ManagementFactory.getThreadMXBean();
		sb.append("Threads: ").append(threadBean.getThreadCount()).append("\n");
		
		// 检查共享窗口状态
		String stateError = validateState();
		if(stateError != null) {
			sb.append("State Error: ").append(stateError).append("\n");
		} else {
			sb.append("State: OK\n");
		}
		
		return sb.toString();
	}
	
	/**
	 * 重置统计信息
	 */
	public static void resetStats() {
		frameCount = 0;
		currentFps = 0;
		lastFpsCheckTime = System.currentTimeMillis();
		lastMemoryCheckTime = System.currentTimeMillis();
		LOGGER.info("Debug stats reset");
	}
}
