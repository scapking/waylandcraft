package dev.evvie.waylandcraft.network;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.SharedWindowEntry;
import dev.evvie.waylandcraft.shared.SharedWindowManager;
import dev.evvie.waylandcraft.shared.WindowPermission;
import dev.evvie.waylandcraft.utils.ServerWarnThrottle;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerLifecycleEvents;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerTickEvents;
import net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;

/**
 * 共享窗口"最新帧缓存 + 多线程批量转发"器（v0.2.31：转发按窗口分片多线程）。
 *
 * 背景：
 * - v0.2.30 把 send 从 Server thread 挪到独立转发线程，但仍是单线程：
 *   多窗口同时共享时帧转发串行排队，一个慢接收端会拖慢其它窗口。
 * - v0.2.31 把转发线程池化：按 windowHandle hash 分片到 N 个线程
 *   （N = clamp(cpu/2, 2..4)）。同一窗口永远落在同一线程 → 帧序不丢；
 *   不同窗口并行转发 → 多窗口共享互不拖累。
 *
 * 方案：
 * - receiver（netty 线程）只做权限校验，把最新帧放入缓存，绝不 send。
 * - 服务端 tick 每 2 tick（约 50ms）只做"轻量收集"：取出待转发帧 + 复制
 *   players 快照（主线程上读取玩家列表绝对安全），然后按窗口 hash 分片提交。
 * - 实际 send 全部在 waylandcraft-frame-relay-N 线程执行，Server thread 零负担。
 * - 缓存按 windowHandle 覆盖旧帧 = 丢中间帧；迭代器逐个 remove 取出，
 *   不用 clear()，避免并发 put 的新帧被误删，保证不丢"最新帧"。
 *
 * 服务端加固（server-polish）：
 * - P1-1：每轮收集时对玩家快照做 ServerPlayNetworking.canSend 过滤（结果本轮缓存），
 *   无能力者不发送并计入统计（grantViewToNewPlayer 的授权语义不变：能力 ≠ 权限）。
 * - P1-2：发送前检查 netty channel isWritable，不可写跳帧；连续 >1s 记一次
 *   slow_viewer 事件（见 {@link SlowViewerTracker}）。
 * - P1-3：转发线程池惰性重建——SERVER_STOPPING 后（断线重进世界、退出单人存档
 *   再进）executor 已 shutdown，旧实现永久 RejectedExecutionException → 共享黑屏。
 * - P2-1：超大帧在 acceptFrame 入口即检即丢（不进缓存），限频告警。
 */
public class SharedWindowFrameRelay {

	/** 按 windowHandle 缓存每个窗口的最新帧；put 覆盖旧帧 = 丢中间帧 */
	private final Map<Long, SharedWindowImagePayload> latestFrames = new ConcurrentHashMap<>();

	/** MC CustomPacketPayload 约 2MB 包大小上限保护：超过该字节数入口即丢 */
	static final int MAX_FRAME_BYTES = 1_900_000;

	/** P2-3：帧尺寸合法上界（客户端 NativeImage 分配防御） */
	static final int MAX_FRAME_DIM = 8192;

	/** 转发间隔：每 2 tick 转发一次（约 50ms = 20fps 批次） */
	private static final int RELAY_INTERVAL_TICKS = 2;

	/**
	 * 转发线程数：2..4（CPU 一半，clamp）。按窗口 hash 分片：
	 * 同窗口同线程（保序），异窗口并行（多窗口共享互不拖累）。
	 */
	private static final int RELAY_THREADS = Math.max(2, Math.min(4, Runtime.getRuntime().availableProcessors() / 2));

	/**
	 * P1-3：分片转发线程池。惰性创建/重建——SERVER_STOPPING 置 null，
	 * 下一次 tick（新会话）重新创建，修复退出世界重进后转发线程池永久失效。
	 */
	private volatile ExecutorService[] relayExecutors;

	/** P1-3：线程池被拒的一次性 WARN 标志（每次重建复位） */
	private final AtomicBoolean shutdownWarned = new AtomicBoolean(false);

	/** 只在服务端线程读写 */
	private int tickCounter = 0;

	/** P1-2：慢观察者追踪（转发线程读写） */
	private final SlowViewerTracker slowTracker = new SlowViewerTracker();

	// ===== P3：累计统计（供 SharedWindowDebug 状态报告），跨会话在 clear() 归零 =====
	private final AtomicLong totalFrames = new AtomicLong();
	private final AtomicLong totalBytes = new AtomicLong();
	private final AtomicLong totalSends = new AtomicLong();
	private final AtomicLong droppedOversized = new AtomicLong();
	private final AtomicLong droppedBadDimensions = new AtomicLong();
	private final AtomicLong droppedNoCapability = new AtomicLong();
	private final AtomicLong skippedUnwritable = new AtomicLong();
	private final AtomicLong slowViewerEvents = new AtomicLong();

	/**
	 * P2-1/P2-3：入口帧校验（纯函数，可单测）：
	 * 尺寸 ∈ [1, 8192]、字节数 ∈ [0, MAX_FRAME_BYTES]。
	 */
	public static boolean isFrameAcceptable(int width, int height, int dataLength) {
		return width >= 1 && width <= MAX_FRAME_DIM
			&& height >= 1 && height <= MAX_FRAME_DIM
			&& dataLength >= 0 && dataLength <= MAX_FRAME_BYTES;
	}

	/** 只在服务端线程读写 */
	private boolean executorsReady() {
		return relayExecutors != null;
	}

	private synchronized void ensureExecutors() {
		if (relayExecutors != null) return;
		ExecutorService[] executors = new ExecutorService[RELAY_THREADS];
		for (int i = 0; i < RELAY_THREADS; i++) {
			final int idx = i;
			executors[i] = Executors.newSingleThreadExecutor(r -> {
				Thread t = new Thread(r, "waylandcraft-frame-relay-" + idx);
				t.setDaemon(true);
				return t;
			});
		}
		shutdownWarned.set(false);
		relayExecutors = executors;
	}

	private synchronized void shutdownExecutors() {
		ExecutorService[] executors = relayExecutors;
		relayExecutors = null;
		if (executors != null) {
			for (ExecutorService executor : executors) {
				executor.shutdownNow();
			}
		}
	}

	/**
	 * 注册服务端 tick 转发。必须在初始化流程里与 receiver 注册一起调用。
	 * P1-3：线程池改为 tick 时惰性创建，SERVER_STOPPING 只 shutdown，
	 * 新会话首个 tick 自动重建——断线重进世界不再永久黑屏。
	 */
	public void register() {
		ServerTickEvents.END_SERVER_TICK.register(this::onServerTick);
		ServerLifecycleEvents.SERVER_STOPPING.register(server -> shutdownExecutors());
		// P2-5：会话完全停止后清缓存与统计，不跨会话残留
		ServerLifecycleEvents.SERVER_STOPPED.register(server -> clear());
	}

	/**
	 * netty 线程调用：仅把最新帧放入缓存，这里绝不调用 ServerPlayNetworking.send。
	 * 由调用方（receiver）保证权限校验已通过。
	 * P2-1：超大/坏尺寸帧在入口即检即丢（不进缓存），限频告警。
	 *
	 * @return true = 已入缓存；false = 校验失败被丢弃
	 */
	public boolean acceptFrame(SharedWindowImagePayload payload) {
		int dataLength = payload.imageData().length;
		if (!isFrameAcceptable(payload.width(), payload.height(), dataLength)) {
			boolean oversized = dataLength > MAX_FRAME_BYTES || dataLength < 0;
			if (oversized) {
				droppedOversized.incrementAndGet();
			} else {
				droppedBadDimensions.incrementAndGet();
			}
			if (ServerWarnThrottle.allow("frame-reject:" + payload.windowHandle(), 5000)) {
				WaylandCraftCommon.LOGGER.warn("[SERVER] image frame rejected at intake: handle=0x{} w={} h={} bytes={} (oversized={})",
					Long.toHexString(payload.windowHandle()), payload.width(), payload.height(), dataLength, oversized);
			}
			return false;
		}
		latestFrames.put(payload.windowHandle(), payload);
		return true;
	}

	/** 窗口注销时立即清掉缓存帧（防旧帧泄露给下一个复用 handle 的窗口） */
	public void onWindowRemoved(long windowHandle) {
		latestFrames.remove(windowHandle);
	}

	/**
	 * Server thread 回调：只做轻量收集 + 分片提交，绝不 send。
	 */
	private void onServerTick(MinecraftServer server) {
		tickCounter++;
		if (tickCounter < RELAY_INTERVAL_TICKS) return;
		tickCounter = 0;
		if (latestFrames.isEmpty()) return;

		// P1-3：新会话（或首次）惰性创建转发线程池
		if (!executorsReady()) ensureExecutors();
		ExecutorService[] executors = relayExecutors;
		if (executors == null) return;

		// 主线程读取玩家列表并快照（主线程自身不会并发修改，安全；转发线程用快照避免 CME）
		List<ServerPlayer> players = new ArrayList<>(server.getPlayerList().getPlayers());

		// P1-1：本轮 canSend 过滤（服务端线程执行，结果缓存本轮复用）。
		// 能力≠权限：不因此撤销 VIEW 授权，只是对没注册接收器的连接不发送、计数观测。
		Map<UUID, Boolean> capabilityCache = new HashMap<>();
		List<ServerPlayer> capable = RelaySinks.selectCapable(
			players,
			ServerPlayer::getUUID,
			capabilityCache,
			player -> ServerPlayNetworking.canSend(player, SharedWindowImagePayload.TYPE));
		long incapable = players.size() - capable.size();
		if (incapable > 0) droppedNoCapability.addAndGet(incapable);

		// P1-2：清掉已下线玩家的慢观察者 episode，防 map 泄漏
		Map<UUID, ServerPlayer> online = new HashMap<>();
		for (ServerPlayer player : players) online.put(player.getUUID(), player);
		slowTracker.retainOnly(online.keySet());

		// 收集待转发帧（迭代器逐个 remove，不用 clear()：并发 put 的新帧不会被误删）
		List<SharedWindowImagePayload> frames = new ArrayList<>();
		Iterator<Map.Entry<Long, SharedWindowImagePayload>> it = latestFrames.entrySet().iterator();
		while (it.hasNext()) {
			Map.Entry<Long, SharedWindowImagePayload> frameEntry = it.next();
			it.remove();
			frames.add(frameEntry.getValue());
		}
		if (frames.isEmpty()) return;

		// 按窗口 hash 分片提交：同窗口同一线程（帧序不丢），异窗口并行。
		for (SharedWindowImagePayload payload : frames) {
			int shard = (int) (payload.windowHandle() & 0x7fffffff) % RELAY_THREADS;
			try {
				executors[shard].execute(() -> relayFrame(server, capable, payload));
			} catch (RejectedExecutionException e) {
				handleRejected();
			}
		}
	}

	/** P1-3：线程池已随会话停止——一次性 WARN，下个 tick 惰性重建。 */
	private void handleRejected() {
		if (shutdownWarned.compareAndSet(false, true)) {
			WaylandCraftCommon.LOGGER.warn("[SERVER] frame relay executor rejected tasks (server session stopping); pool will be rebuilt lazily on next tick");
		}
		shutdownExecutors();
	}

	/**
	 * 转发线程：发送单帧。绝不阻塞 Server thread。
	 * capable 列表已由 tick 收集阶段过滤 canSend；这里再做权限 + 背压判定。
	 */
	private void relayFrame(MinecraftServer server, List<ServerPlayer> capable, SharedWindowImagePayload payload) {
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		// 转发前再次确认窗口仍存在（可能已注销）
		SharedWindowEntry entry = manager.getWindow(payload.windowHandle());
		if (entry == null) {
			return;
		}
		UUID senderUUID = entry.getOwnerUUID();

		// 批量转发给所有有 VIEW 权限且本轮 canSend 通过的在线玩家（跳过发送者本人）
		int forwarded = 0;
		long now = System.currentTimeMillis();
		for (ServerPlayer player : capable) {
			UUID uuid = player.getUUID();
			if (uuid.equals(senderUUID)) continue;
			// ACL-3：VIEW 门走合成判定（owner 恒可、黑名单/过期即刻失效）。
			// 转发线程读 PermissionManager 的并发表（ConcurrentHashMap/volatile），无锁安全。
			if (!manager.effectivePermission(entry, uuid, now).hasPermission(WindowPermission.VIEW)) continue;

			// P1-2 背压：channel 不可写（发送队列超水位）跳帧，不让视频流拖垮连接/堆
			if (!RelaySinks.isChannelWritable(player)) {
				skippedUnwritable.incrementAndGet();
				if (slowTracker.observeUnwritable(uuid, now)) {
					slowViewerEvents.incrementAndGet();
					WaylandCraftCommon.LOGGER.info("[SERVER] slow viewer {} on window 0x{}: unwritable >1s, frames being skipped",
						uuid, Long.toHexString(payload.windowHandle()));
				}
				continue;
			}
			slowTracker.observeWritable(uuid);
			ServerPlayNetworking.send(player, payload);
			forwarded++;
		}
		// ⑥ 帧级日志降为 debug（24fps×多窗口下这是最大的日志洪水源）；
		// INFO 改为 1s 聚合摘要，保留吞吐可观测性
		WaylandCraftCommon.LOGGER.debug("[SERVER] forwarded image from {} to {} players ({} bytes)",
			senderUUID, forwarded, payload.imageData().length);
		totalFrames.incrementAndGet();
		totalBytes.addAndGet(payload.imageData().length);
		totalSends.addAndGet(forwarded);
		synchronized (statsLock) {
			statsFrames++;
			statsBytes += payload.imageData().length;
			statsReceivers += forwarded;
			long nowMs = System.currentTimeMillis();
			if (nowMs - statsWindowStartMs >= 1000) {
				if (statsFrames > 0) {
					WaylandCraftCommon.LOGGER.info("[SERVER] relay stats ({}ms): {} frames, {} KB/s, {} sends, skip_unwritable={} slow_viewers={}",
						nowMs - statsWindowStartMs, statsFrames, String.format("%.1f", statsBytes / 1024.0), statsReceivers,
						skippedUnwritable.get(), slowViewerEvents.get());
				}
				statsWindowStartMs = nowMs;
				statsFrames = 0; statsBytes = 0; statsReceivers = 0;
			}
		}
	}

	/** ⑥ 1 秒聚合转发统计 */
	private final Object statsLock = new Object();
	private long statsWindowStartMs = System.currentTimeMillis();
	private long statsFrames;
	private long statsBytes;
	private int statsReceivers;

	// ===== P3：统计读取接口（SharedWindowDebug 状态报告用） =====

	public long getTotalFrames() { return totalFrames.get(); }
	public long getTotalBytes() { return totalBytes.get(); }
	public long getTotalSends() { return totalSends.get(); }
	public long getDroppedOversized() { return droppedOversized.get(); }
	public long getDroppedBadDimensions() { return droppedBadDimensions.get(); }
	public long getDroppedNoCapability() { return droppedNoCapability.get(); }
	public long getSkippedUnwritable() { return skippedUnwritable.get(); }
	public long getSlowViewerEvents() { return slowViewerEvents.get(); }

	/** P2-5：SERVER_STOPPED 清缓存与统计，防止跨会话残留 */
	public void clear() {
		latestFrames.clear();
		slowTracker.clear();
		tickCounter = 0;
		totalFrames.set(0);
		totalBytes.set(0);
		totalSends.set(0);
		droppedOversized.set(0);
		droppedBadDimensions.set(0);
		droppedNoCapability.set(0);
		skippedUnwritable.set(0);
		slowViewerEvents.set(0);
		synchronized (statsLock) {
			statsFrames = 0; statsBytes = 0; statsReceivers = 0;
			statsWindowStartMs = System.currentTimeMillis();
		}
	}
}
