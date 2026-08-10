package dev.evvie.waylandcraft.network;

import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.RejectedExecutionException;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.SharedWindowEntry;
import dev.evvie.waylandcraft.shared.SharedWindowManager;
import dev.evvie.waylandcraft.shared.WindowPermission;
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
 */
public class SharedWindowFrameRelay {

	/** 按 windowHandle 缓存每个窗口的最新帧；put 覆盖旧帧 = 丢中间帧 */
	private final Map<Long, SharedWindowImagePayload> latestFrames = new ConcurrentHashMap<>();

	/** MC CustomPacketPayload 约 2MB 包大小上限保护：超过该字节数告警并跳过 */
	private static final int MAX_FRAME_BYTES = 1_900_000;

	/** 转发间隔：每 2 tick 转发一次（约 50ms = 20fps 批次） */
	private static final int RELAY_INTERVAL_TICKS = 2;

	/**
	 * 转发线程数：2..4（CPU 一半，clamp）。按窗口 hash 分片：
	 * 同窗口同线程（保序），异窗口并行（多窗口共享互不拖累）。
	 */
	private static final int RELAY_THREADS = Math.max(2, Math.min(4, Runtime.getRuntime().availableProcessors() / 2));

	/** 分片转发线程池（daemon，服务端停止自动退出） */
	private final ExecutorService[] relayExecutors = new ExecutorService[RELAY_THREADS];
	{
		for (int i = 0; i < RELAY_THREADS; i++) {
			final int idx = i;
			relayExecutors[i] = Executors.newSingleThreadExecutor(r -> {
				Thread t = new Thread(r, "waylandcraft-frame-relay-" + idx);
				t.setDaemon(true);
				return t;
			});
		}
	}

	/** 只在服务端线程读写 */
	private int tickCounter = 0;

	/**
	 * 注册服务端 tick 转发。必须在初始化流程里与 receiver 注册一起调用。
	 */
	public void register() {
		ServerTickEvents.END_SERVER_TICK.register(this::onServerTick);
		ServerLifecycleEvents.SERVER_STOPPING.register(server -> {
			for (ExecutorService executor : relayExecutors) {
				executor.shutdownNow();
			}
		});
	}

	/**
	 * netty 线程调用：仅把最新帧放入缓存，这里绝不调用 ServerPlayNetworking.send。
	 * 由调用方（receiver）保证权限校验已通过。
	 */
	public void acceptFrame(SharedWindowImagePayload payload) {
		latestFrames.put(payload.windowHandle(), payload);
	}

	/**
	 * Server thread 回调：只做轻量收集 + 分片提交，绝不 send。
	 */
	private void onServerTick(MinecraftServer server) {
		tickCounter++;
		if (tickCounter < RELAY_INTERVAL_TICKS) return;
		tickCounter = 0;
		if (latestFrames.isEmpty()) return;

		// 主线程读取玩家列表并快照（主线程自身不会并发修改，安全；转发线程用快照避免 CME）
		List<ServerPlayer> players = new ArrayList<>(server.getPlayerList().getPlayers());

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
				relayExecutors[shard].execute(() -> relayFrame(server, players, payload));
			} catch (RejectedExecutionException e) {
				// 服务端已停止，丢弃本帧
			}
		}
	}

	/**
	 * 转发线程：发送单帧。绝不阻塞 Server thread。
	 */
	private void relayFrame(MinecraftServer server, List<ServerPlayer> players, SharedWindowImagePayload payload) {
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		// 包大小保护：超过上限告警并跳过该帧，避免协议崩
		if (payload.imageData().length > MAX_FRAME_BYTES) {
			WaylandCraftCommon.LOGGER.warn("[SERVER] image frame too large ({} bytes), skipping windowHandle={}",
				payload.imageData().length, payload.windowHandle());
			return;
		}

		// 转发前再次确认窗口仍存在（可能已注销）
		SharedWindowEntry entry = manager.getWindow(payload.windowHandle());
		if (entry == null) {
			return;
		}
		UUID senderUUID = entry.getOwnerUUID();

		// 批量转发给所有有 VIEW 权限的在线玩家（跳过发送者本人）
		int forwarded = 0;
		for (ServerPlayer player : players) {
			if (player.getUUID().equals(senderUUID)) continue;
			if (entry.hasPermission(player.getUUID(), WindowPermission.VIEW)) {
				ServerPlayNetworking.send(player, payload);
				forwarded++;
			}
		}
		WaylandCraftCommon.LOGGER.info("[SERVER] forwarded image from {} to {} players ({} bytes)",
			senderUUID, forwarded, payload.imageData().length);
	}

}
