package dev.evvie.waylandcraft.network;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
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
 * 共享窗口音频转发器（连续流：队列 + 尽力而为，积压时丢旧保新）。
 * 
 * 与 {@link SharedWindowFrameRelay}（图像用"最新帧覆盖"）不同，音频是连续流，
 * 不能只留最新 —— 但音频丢包听感可接受，所以策略是：
 * - 每窗口一个队列，netty 线程只入队（绝不 send）；
 * - 服务端 tick 批量收集，转发线程池 send；
 * - 队列超过上限（积压，接收端跟不上）时丢最旧的包 —— 保实时不保完整。
 * 
 * 带宽：PCM 48kHz stereo 16bit ≈ 192KB/s 每共享窗口。包 ≤ ~30KB。
 */
public class AudioFrameRelay {

	/** 每窗口待转发音频队列（有界：超过上限丢最旧） */
	private final Map<Long, Deque<SharedWindowAudioPayload>> pending = new ConcurrentHashMap<>();

	/** 每窗口队列最大包数（30KB × 50 = 1.5MB 缓冲 ≈ 0.5 秒积压） */
	private static final int MAX_PENDING_PER_WINDOW = 50;

	/** 转发间隔：每 2 tick 转发一次（约 50ms） */
	private static final int RELAY_INTERVAL_TICKS = 2;

	/** 转发线程数：2..4（按窗口 hash 分片保序） */
	private static final int RELAY_THREADS = Math.max(2, Math.min(4, Runtime.getRuntime().availableProcessors() / 2));

	private final ExecutorService[] relayExecutors = new ExecutorService[RELAY_THREADS];
	{
		for (int i = 0; i < RELAY_THREADS; i++) {
			final int idx = i;
			relayExecutors[i] = Executors.newSingleThreadExecutor(r -> {
				Thread t = new Thread(r, "waylandcraft-audio-relay-" + idx);
				t.setDaemon(true);
				return t;
			});
		}
	}

	private int tickCounter = 0;

	public void register() {
		ServerTickEvents.END_SERVER_TICK.register(this::onServerTick);
		ServerLifecycleEvents.SERVER_STOPPING.register(server -> {
			for (ExecutorService executor : relayExecutors) {
				executor.shutdownNow();
			}
		});
	}

	/**
	 * netty 线程调用：仅入队，这里绝不调用 ServerPlayNetworking.send。
	 * 由调用方（receiver）保证权限校验已通过。
	 */
	public void acceptAudio(SharedWindowAudioPayload payload) {
		Deque<SharedWindowAudioPayload> queue = pending.computeIfAbsent(payload.windowHandle(), k -> new ArrayDeque<>());
		synchronized (queue) {
			queue.addLast(payload);
			while (queue.size() > MAX_PENDING_PER_WINDOW) {
				queue.removeFirst(); // 积压：丢最旧，保实时
			}
		}
	}

	/**
	 * Server thread 回调：轻量收集 + 分片提交，绝不 send。
	 */
	private void onServerTick(MinecraftServer server) {
		tickCounter++;
		if (tickCounter < RELAY_INTERVAL_TICKS) return;
		tickCounter = 0;
		if (pending.isEmpty()) return;

		List<ServerPlayer> players = new ArrayList<>(server.getPlayerList().getPlayers());

		// 收集待转发包
		List<SharedWindowAudioPayload> packets = new ArrayList<>();
		var it = pending.entrySet().iterator();
		while (it.hasNext()) {
			var entry = it.next();
			Deque<SharedWindowAudioPayload> queue = entry.getValue();
			synchronized (queue) {
				while (!queue.isEmpty()) {
					packets.add(queue.removeFirst());
				}
			}
		}
		if (packets.isEmpty()) return;

		for (SharedWindowAudioPayload payload : packets) {
			int shard = (int) (payload.windowHandle() & 0x7fffffff) % RELAY_THREADS;
			try {
				relayExecutors[shard].execute(() -> relayAudio(server, players, payload));
			} catch (RejectedExecutionException e) {
				// 服务端已停止，丢弃本包
			}
		}
	}

	/**
	 * 转发线程：发送单个音频包。绝不阻塞 Server thread。
	 */
	private void relayAudio(MinecraftServer server, List<ServerPlayer> players, SharedWindowAudioPayload payload) {
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		SharedWindowEntry entry = manager.getWindow(payload.windowHandle());
		if (entry == null) {
			return;
		}
		UUID senderUUID = entry.getOwnerUUID();

		for (ServerPlayer player : players) {
			if (player.getUUID().equals(senderUUID)) continue;
			if (entry.hasPermission(player.getUUID(), WindowPermission.VIEW)) {
				ServerPlayNetworking.send(player, payload);
			}
		}
	}

}
