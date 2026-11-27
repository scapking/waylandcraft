package dev.evvie.waylandcraft.network;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
import java.util.HashMap;
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
 * 共享窗口音频转发器（连续流：队列 + 尽力而为，积压时丢旧保新）。
 *
 * 与 {@link SharedWindowFrameRelay}（图像用"最新帧覆盖"）不同，音频是连续流，
 * 不能只留最新 —— 但音频丢包听感可接受，所以策略是：
 * - 每窗口一个队列，netty 线程只入队（绝不 send）；
 * - 服务端 tick 批量收集，转发线程池 send；
 * - 队列超过上限（积压，接收端跟不上）时丢最旧的包 —— 保实时不保完整。
 *
 * 带宽：PCM 48kHz stereo 16bit ≈ 192KB/s 每共享窗口。包 ≤ ~30KB。
 *
 * 服务端加固（server-polish）：
 * - P2-2：acceptAudio 入口校验（≤64KB、sampleRate/channels sanity），非法丢弃+计数；
 *   窗口注销/断连（tick 巡检发现窗口不存在）时移除队列条目，防泄漏与跨会话残留。
 * - P1-1/P1-2：canSend 过滤 + isWritable 背压跳包 + slow_viewer 统计。
 * - P1-3：转发线程池惰性重建（同帧中继）。
 */
public class AudioFrameRelay {

	/** 每窗口待转发音频队列（有界：超过上限丢最旧） */
	private final Map<Long, Deque<SharedWindowAudioPayload>> pending = new ConcurrentHashMap<>();

	/** 每窗口队列最大包数（30KB × 50 = 1.5MB 缓冲 ≈ 0.5 秒积压） */
	private static final int MAX_PENDING_PER_WINDOW = 50;

	/**
	 * 任务A-3：每窗口 FIFO 字节预算，与视频对齐（复用视频"单帧缓存"上限
	 * MAX_FRAME_BYTES = 1.9MB）。原实现只有包数上限：50 × 64KB 恶意包 = 3.2MB/窗，
	 * 与视频 1.9MB/窗不对称；字节上限后两者预算一致，谁先触发谁丢旧。
	 */
	static final int MAX_PENDING_BYTES_PER_WINDOW = SharedWindowFrameRelay.MAX_FRAME_BYTES;

	/** P2-2：单包 PCM 上限 64KB（合法 ~30KB，超限视为恶意/坏端） */
	static final int MAX_AUDIO_BYTES = 64 * 1024;

	/** P2-2：采样率合法区间（8kHz 电话~ 192kHz _hi-res_，覆盖一切合理捕获源） */
	static final int MIN_SAMPLE_RATE = 8000;
	static final int MAX_SAMPLE_RATE = 192000;

	/** 转发间隔：每 2 tick 转发一次（约 50ms） */
	private static final int RELAY_INTERVAL_TICKS = 2;

	/** 转发线程数：2..4（按窗口 hash 分片保序） */
	private static final int RELAY_THREADS = Math.max(2, Math.min(4, Runtime.getRuntime().availableProcessors() / 2));

	/** P1-3：惰性创建/重建（详见 SharedWindowFrameRelay 同机制注释） */
	private volatile ExecutorService[] relayExecutors;
	private final AtomicBoolean shutdownWarned = new AtomicBoolean(false);

	private int tickCounter = 0;

	/** P1-2：慢观察者追踪 */
	private final SlowViewerTracker slowTracker = new SlowViewerTracker();

	// ===== P3 + overload-guard：累计统计 =====
	private final AtomicLong totalPackets = new AtomicLong();
	private final AtomicLong totalBytes = new AtomicLong();
	private final AtomicLong totalSends = new AtomicLong();
	private final AtomicLong droppedInvalid = new AtomicLong();
	private final AtomicLong droppedNoCapability = new AtomicLong();
	private final AtomicLong skippedUnwritable = new AtomicLong();
	private final AtomicLong slowViewerEvents = new AtomicLong();
	private final AtomicLong skippedNotSubscribed = new AtomicLong();
	private final AtomicLong skippedLoadLevel = new AtomicLong();
	private final AtomicLong skippedOverLimit = new AtomicLong();
	private final AtomicLong droppedMemoryGuard = new AtomicLong();

	public AudioFrameRelay() {
		// 任务A-3：把音频 FIFO 字节数注册进全局内存守卫（求和来源，无增量计数器）
		RelayMemoryGuard.bindAudioSource(this::pendingAudioBytes);
	}

	/**
	 * P2-2：音频包合法性判定（纯函数，可单测）。
	 */
	public static boolean isAudioPayloadAcceptable(int sampleRate, int channels, int pcmLength) {
		return pcmLength > 0 && pcmLength <= MAX_AUDIO_BYTES
			&& sampleRate >= MIN_SAMPLE_RATE && sampleRate <= MAX_SAMPLE_RATE
			&& channels >= 1 && channels <= 2;
	}

	private synchronized void ensureExecutors() {
		if (relayExecutors != null) return;
		ExecutorService[] executors = new ExecutorService[RELAY_THREADS];
		for (int i = 0; i < RELAY_THREADS; i++) {
			final int idx = i;
			executors[i] = Executors.newSingleThreadExecutor(r -> {
				Thread t = new Thread(r, "waylandcraft-audio-relay-" + idx);
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

	public void register() {
		ServerTickEvents.END_SERVER_TICK.register(this::onServerTick);
		ServerLifecycleEvents.SERVER_STOPPING.register(server -> shutdownExecutors());
		// P2-5：会话停止清队列，不跨会话残留
		ServerLifecycleEvents.SERVER_STOPPED.register(server -> clear());
	}

	/**
	 * netty 线程调用：仅校验入队，这里绝不调用 ServerPlayNetworking.send。
	 * 由调用方（receiver）保证权限校验已通过。
	 *
	 * @return true = 已入队；false = 校验失败被丢弃
	 */
	public boolean acceptAudio(SharedWindowAudioPayload payload) {
		if (!isAudioPayloadAcceptable(payload.sampleRate(), payload.channels(), payload.pcmData().length)) {
			droppedInvalid.incrementAndGet();
			if (ServerWarnThrottle.allow("audio-reject:" + payload.windowHandle(), 5000)) {
				WaylandCraftCommon.LOGGER.warn("[SERVER] audio packet rejected at intake: handle=0x{} sampleRate={} channels={} bytes={}",
					Long.toHexString(payload.windowHandle()), payload.sampleRate(), payload.channels(), payload.pcmData().length);
			}
			return false;
		}
		Deque<SharedWindowAudioPayload> queue = pending.computeIfAbsent(payload.windowHandle(), k -> new ArrayDeque<>());
		// 任务A-3：全局总量守卫先于入队（不持任何队列锁，避免跨队列锁序死锁）：
		// 现有总量 + 新包 ≤ 上限才准入；不足则从"当前最重队列"逐包丢旧（全局公平，
		// 而非单窗口挨饿），全部队列清空仍放不下才拒新包（理论上不可达：单包
		// ≤64KB ≪ 全局上限，全空队列求和为 0 必然准入）。
		while (!RelayMemoryGuard.audioAcceptAllowed(pendingAudioBytes(), payload.pcmData().length)) {
			Deque<SharedWindowAudioPayload> victim = heaviestQueue();
			if (victim == null) {
				droppedMemoryGuard.incrementAndGet();
				if (ServerWarnThrottle.allow("audio-memory-guard:" + payload.windowHandle(), 5000)) {
					WaylandCraftCommon.LOGGER.warn("[SERVER] audio packet rejected by global memory guard: handle=0x{} bytes={}",
						Long.toHexString(payload.windowHandle()), payload.pcmData().length);
				}
				return false;
			}
			synchronized (victim) {
				if (!victim.isEmpty()) victim.removeFirst();
			}
		}
		synchronized (queue) {
			queue.addLast(payload);
			// 积压：丢最旧保实时。任务A-3：包数与字节双上限（字节与视频"单帧缓存"
			// 预算对齐），谁先触发谁丢旧。
			while (queue.size() > MAX_PENDING_PER_WINDOW || queueBytes(queue) > MAX_PENDING_BYTES_PER_WINDOW) {
				queue.removeFirst();
			}
		}
		return true;
	}

	/** 单个队列的缓冲字节（调用方持有该队列锁或仅在扫描时短暂加锁） */
	private static long queueBytes(Deque<SharedWindowAudioPayload> queue) {
		long total = 0;
		for (SharedWindowAudioPayload p : queue) {
			total += p.pcmData().length;
		}
		return total;
	}

	/** 当前音频 FIFO 缓冲总字节（全局内存守卫求和来源；逐队列短暂加锁，无跨队列持锁） */
	public long pendingAudioBytes() {
		long total = 0;
		for (Deque<SharedWindowAudioPayload> q : pending.values()) {
			synchronized (q) {
				total += queueBytes(q);
			}
		}
		return total;
	}

	/** 找到当前缓冲字节最多的非空队列（全局公平驱逐的受害者选择） */
	private Deque<SharedWindowAudioPayload> heaviestQueue() {
		Deque<SharedWindowAudioPayload> heaviest = null;
		long heaviestBytes = 0;
		for (Deque<SharedWindowAudioPayload> q : pending.values()) {
			synchronized (q) {
				long bytes = queueBytes(q);
				if (bytes > heaviestBytes) {
					heaviestBytes = bytes;
					heaviest = q;
				}
			}
		}
		return heaviest;
	}

	/** P2-2：窗口注销立即移除队列条目 */
	public void onWindowRemoved(long windowHandle) {
		pending.remove(windowHandle);
	}

	/**
	 * Server thread 回调：轻量收集 + 分片提交，绝不 send。
	 */
	private void onServerTick(MinecraftServer server) {
		tickCounter++;
		if (tickCounter < RELAY_INTERVAL_TICKS) return;
		tickCounter = 0;
		if (pending.isEmpty()) return;

		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		// P1-3：新会话首个 tick 惰性重建线程池
		if (relayExecutors == null) ensureExecutors();
		ExecutorService[] executors = relayExecutors;
		if (executors == null) return;

		List<ServerPlayer> players = new ArrayList<>(server.getPlayerList().getPlayers());

		// P1-1：本轮 canSend 过滤（能力≠权限，不撤销授权，仅不发送+计数）
		Map<UUID, Boolean> capabilityCache = new HashMap<>();
		List<ServerPlayer> capable = RelaySinks.selectCapable(
			players,
			ServerPlayer::getUUID,
			capabilityCache,
			player -> ServerPlayNetworking.canSend(player, SharedWindowAudioPayload.TYPE));
		long incapable = players.size() - capable.size();
		if (incapable > 0) droppedNoCapability.addAndGet(incapable);

		// P1-2：清掉已下线玩家的 episode，防泄漏
		Map<UUID, ServerPlayer> online = new HashMap<>();
		for (ServerPlayer player : players) online.put(player.getUUID(), player);
		slowTracker.retainOnly(online.keySet());
		// 任务A-2：分级状态同步清理（订阅登记由 SharedWindowServerHandler tick 清理）
		ViewerLoadController.retainOnly(online.keySet());

		// 收集待转发包；顺带 P2-2 清理：窗口已不存在（注销/断连）的队列条目直接移除
		List<SharedWindowAudioPayload> packets = new ArrayList<>();
		var it = pending.entrySet().iterator();
		while (it.hasNext()) {
			var entry = it.next();
			Deque<SharedWindowAudioPayload> queue = entry.getValue();
			boolean drained;
			synchronized (queue) {
				drained = queue.isEmpty();
				while (!queue.isEmpty()) {
					packets.add(queue.removeFirst());
				}
			}
			if (drained && manager.getWindow(entry.getKey()) == null) {
				it.remove();
			}
		}
		if (packets.isEmpty()) return;

		for (SharedWindowAudioPayload payload : packets) {
			int shard = (int) (payload.windowHandle() & 0x7fffffff) % RELAY_THREADS;
			try {
				executors[shard].execute(() -> relayAudio(server, capable, payload));
			} catch (RejectedExecutionException e) {
				handleRejected();
			}
		}
	}

	/** P1-3：线程池已随会话停止——一次性 WARN，下个 tick 惰性重建。 */
	private void handleRejected() {
		if (shutdownWarned.compareAndSet(false, true)) {
			WaylandCraftCommon.LOGGER.warn("[SERVER] audio relay executor rejected tasks (server session stopping); pool will be rebuilt lazily on next tick");
		}
		shutdownExecutors();
	}

	/**
	 * 转发线程：发送单个音频包。绝不阻塞 Server thread。
	 */
	private void relayAudio(MinecraftServer server, List<ServerPlayer> capable, SharedWindowAudioPayload payload) {
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		SharedWindowEntry entry = manager.getWindow(payload.windowHandle());
		if (entry == null) {
			return;
		}
		UUID senderUUID = entry.getOwnerUUID();

		int forwarded = 0;
		long now = System.currentTimeMillis();
		boolean overLimit = RelayMemoryGuard.isOverLimit();
		for (ServerPlayer player : capable) {
			UUID uuid = player.getUUID();
			if (uuid.equals(senderUUID)) continue;
			// ACL-3：同帧中继——VIEW 门走合成判定（黑白名单/过期即刻生效）
			if (!manager.effectivePermission(entry, uuid, now).hasPermission(WindowPermission.VIEW)) continue;

			// P1-2 背压 + 任务A-2 分级观测：可写性探测对所有 VIEW 观众执行（含被
			// 订阅/分级门跳过的观众），保证分级状态有持续恢复证据。
			// 音频丢包可听感无损，保实时不保完整。
			if (!RelaySinks.isChannelWritable(player)) {
				skippedUnwritable.incrementAndGet();
				boolean slowEvent = slowTracker.observeUnwritable(uuid, now);
				ViewerLoadController.observeUnwritable(uuid, now, slowEvent);
				if (slowEvent) {
					slowViewerEvents.incrementAndGet();
				}
				continue;
			}
			slowTracker.observeWritable(uuid);
			ViewerLoadController.observeWritable(uuid, now);

			// 任务A-1：订阅上限——未登记交付的窗口不转发（与列表广播一致，fail-open）
			if (!ViewerSubscriptions.isDelivered(uuid, payload.windowHandle())) {
				skippedNotSubscribed.incrementAndGet();
				continue;
			}
			// 任务A-2：分级推送（L0/L1 全量，L2 隔包减半，L3 暂停）
			if (!ViewerLoadController.trySendAudio(uuid)) {
				skippedLoadLevel.incrementAndGet();
				continue;
			}
			// 任务A-3：全局内存守卫联动——超限时先对已降级观众（level ≥ 1）跳包
			//（取舍见 SharedWindowFrameRelay.relayFrame 同款注释）
			if (overLimit && ViewerLoadController.levelOf(uuid) > ViewerLoadController.LEVEL_NORMAL) {
				skippedOverLimit.incrementAndGet();
				continue;
			}
			ServerPlayNetworking.send(player, payload);
			forwarded++;
		}
		totalPackets.incrementAndGet();
		totalBytes.addAndGet(payload.pcmData().length);
		totalSends.addAndGet(forwarded);
		WaylandCraftCommon.LOGGER.debug("[SERVER] forwarded audio from {} to {} players ({} bytes)",
			senderUUID, forwarded, payload.pcmData().length);
	}

	// ===== P3：统计读取接口 =====

	public long getTotalPackets() { return totalPackets.get(); }
	public long getTotalBytes() { return totalBytes.get(); }
	public long getTotalSends() { return totalSends.get(); }
	public long getDroppedInvalid() { return droppedInvalid.get(); }
	public long getDroppedNoCapability() { return droppedNoCapability.get(); }
	public long getSkippedUnwritable() { return skippedUnwritable.get(); }
	public long getSlowViewerEvents() { return slowViewerEvents.get(); }
	public long getSkippedNotSubscribed() { return skippedNotSubscribed.get(); }
	public long getSkippedLoadLevel() { return skippedLoadLevel.get(); }
	public long getSkippedOverLimit() { return skippedOverLimit.get(); }
	public long getDroppedMemoryGuard() { return droppedMemoryGuard.get(); }

	/** P2-5：SERVER_STOPPED 清队列与统计 */
	public void clear() {
		pending.clear();
		slowTracker.clear();
		tickCounter = 0;
		totalPackets.set(0);
		totalBytes.set(0);
		totalSends.set(0);
		droppedInvalid.set(0);
		droppedNoCapability.set(0);
		skippedUnwritable.set(0);
		slowViewerEvents.set(0);
		skippedNotSubscribed.set(0);
		skippedLoadLevel.set(0);
		skippedOverLimit.set(0);
		droppedMemoryGuard.set(0);
	}
}
