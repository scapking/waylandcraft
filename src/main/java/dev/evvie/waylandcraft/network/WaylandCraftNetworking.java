package dev.evvie.waylandcraft.network;

import java.util.HashSet;
import java.util.Set;
import java.util.UUID;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.SharedWindowEntry;
import dev.evvie.waylandcraft.shared.SharedWindowManager;
import dev.evvie.waylandcraft.utils.IMyServerPlayer;
import dev.evvie.waylandcraft.utils.ServerWarnThrottle;
import net.fabricmc.fabric.api.networking.v1.PayloadTypeRegistry;
import net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking;
import net.minecraft.server.level.ServerPlayer;

public class WaylandCraftNetworking {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-networking");

	/** 最新帧缓存 + tick 批量转发器（与 receiver 在同一初始化流程中注册） */
	private static final SharedWindowFrameRelay frameRelay = new SharedWindowFrameRelay();

	/** 共享窗口音频转发器（队列 + 尽力而为，积压丢旧保新） */
	private static final AudioFrameRelay audioRelay = new AudioFrameRelay();

	/** P1-5：单次 alive_windows 上报的句柄数上限（正常客户端远达不到） */
	static final int MAX_ALIVE_WINDOWS = 1024;

	public static SharedWindowFrameRelay getFrameRelay() {
		return frameRelay;
	}

	public static AudioFrameRelay getAudioRelay() {
		return audioRelay;
	}

	public static void register() {
		PayloadTypeRegistry.serverboundPlay().register(ServerboundGiveItemsPayload.TYPE, ServerboundGiveItemsPayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(ServerboundAliveWindowsPayload.TYPE, ServerboundAliveWindowsPayload.CODEC);

		// 注册多人显示功能的数据包
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowRegisterPayload.TYPE, SharedWindowRegisterPayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowUnregisterPayload.TYPE, SharedWindowUnregisterPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowUpdatePayload.TYPE, SharedWindowUpdatePayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowImagePayload.TYPE, SharedWindowImagePayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowImagePayload.TYPE, SharedWindowImagePayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowInteractionPayload.TYPE, SharedWindowInteractionPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowInteractionPayload.TYPE, SharedWindowInteractionPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowPermissionPayload.TYPE, SharedWindowPermissionPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowListPayload.TYPE, SharedWindowListPayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowAudioPayload.TYPE, SharedWindowAudioPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowAudioPayload.TYPE, SharedWindowAudioPayload.CODEC);

		// 权限管理命令
		PayloadTypeRegistry.serverboundPlay().register(PermissionCommandPayload.TYPE, PermissionCommandPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(PermissionResponsePayload.TYPE, PermissionResponsePayload.CODEC);

		// 窗口所有者（owner）授权管理命令
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowPermCommandPayload.TYPE, SharedWindowPermCommandPayload.CODEC);

		// P1-5：alive_windows 直接写玩家对象状态，原实现整段在 netty 线程执行：
		// 与服务器线程（give_items 读、物品 tick 读）竞争 ArrayList → CME/丢数据，
		// 进而误删玩家窗口物品。现在：payload 限长 → 服务端线程上原子替换集合内容。
		ServerPlayNetworking.registerGlobalReceiver(ServerboundAliveWindowsPayload.TYPE, (payload, ctx) -> {
			long[] handles = payload.handles();
			if (handles == null) return;
			if (!isAliveWindowsAcceptable(handles)) {
				// 超限整包丢弃（正常客户端不会到千级句柄；宁可保守勿撑爆集合）
				UUID sender = ctx.player().getUUID();
				if (ServerWarnThrottle.allow("alive-oversize:" + sender, 5000)) {
					LOGGER.warn("Player {} sent {} alive windows (>{}), payload dropped",
						sender, handles.length, MAX_ALIVE_WINDOWS);
				}
				return;
			}
			ServerPlayer player = ctx.player();
			ctx.server().execute(() -> {
				Set<Long> alive = ((IMyServerPlayer) player).getAliveWindows();
				Set<Long> replacement = new HashSet<>(Math.max(16, handles.length * 2));
				for (long handle : handles) {
					replacement.add(handle);
				}
				alive.clear();
				alive.addAll(replacement);
			});
		});

		// P0-2：give_items 直接改玩家库存（netty 线程改 = 与主线程库存操作竞争，
		// vanilla 库存/容器非线程安全）。移入服务端线程执行。
		ServerPlayNetworking.registerGlobalReceiver(ServerboundGiveItemsPayload.TYPE, (payload, ctx) -> {
			runSafely(ctx, "give_items",
				() -> WaylandCraftCommon.instance.serverItemManager.handleGiveItemsPayload(payload, ctx));
		});

		// 处理客户端请求注册共享窗口
		// v0.2.31：注册会触发窗口列表广播（遍历所有玩家 + send），切到服务端主线程执行，
		// 避免占 netty 线程；主线程上读玩家列表也绝对安全（无 CME）。
		// P0-1 同款外层 try/catch：单个任务抛异常不再炸穿 tick 崩溃服务器。
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowRegisterPayload.TYPE, (payload, ctx) ->
			runSafely(ctx, "window_register", () -> SharedWindowServerHandler.handleWindowRegister(payload, ctx.player())));

		// 处理客户端请求注销共享窗口
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowUnregisterPayload.TYPE, (payload, ctx) ->
			runSafely(ctx, "window_unregister", () -> SharedWindowServerHandler.handleWindowUnregister(payload.windowHandle(), ctx.player())));

		// 处理权限管理命令（P0-1：鉴权与钳位在 handler 内，这里保证任务体异常只记 WARN）
		ServerPlayNetworking.registerGlobalReceiver(PermissionCommandPayload.TYPE, (payload, ctx) ->
			runSafely(ctx, "permission_command", () -> SharedWindowServerHandler.handlePermissionCommand(payload, ctx.player())));

		// 处理窗口所有者授权管理命令
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowPermCommandPayload.TYPE, (payload, ctx) ->
			runSafely(ctx, "window_perm_command", () -> SharedWindowServerHandler.handleWindowPermCommand(payload, ctx.player())));

		// 处理客户端上传的窗口图像 - 只做权限校验并入最新帧缓存；
		// 实际转发由服务端 tick（SharedWindowFrameRelay，每 2 tick 批量转发）完成，
		// netty 线程内绝不调用 ServerPlayNetworking.send，避免堵死发送者连接。
		// P2-1：acceptFrame 入口即检即丢（超大/坏尺寸不进缓存），限频告警在 relay 内。
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowImagePayload.TYPE, (payload, ctx) -> {
			ServerPlayer sender = ctx.player();
			UUID senderUUID = sender.getUUID();

			// 检查窗口是否由该玩家共享（registry 是 ConcurrentHashMap，netty 线程读安全）
			SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
			SharedWindowEntry entry = manager.getWindow(payload.windowHandle());
			if(entry == null || !entry.getOwnerUUID().equals(senderUUID)) {
				if (ServerWarnThrottle.allow("image-reject-owner:" + senderUUID, 5000)) {
					WaylandCraftCommon.LOGGER.warn("[SERVER] image rejected: entry={}, owner match={}",
						entry != null, entry != null && entry.getOwnerUUID().equals(senderUUID));
				}
				return;
			}

			// 校验通过：只把最新帧存入缓存（按 windowHandle 覆盖旧帧 = 丢中间帧）
			frameRelay.acceptFrame(payload);
		});

		// tick 转发注册与 receiver 注册在同一初始化流程
		frameRelay.register();

		// 共享窗口音频：netty 线程只入队，tick 批量转发
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowAudioPayload.TYPE, (payload, ctx) -> {
			ServerPlayer sender = ctx.player();
			UUID senderUUID = sender.getUUID();

			SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
			SharedWindowEntry entry = manager.getWindow(payload.windowHandle());
			if(entry == null || !entry.getOwnerUUID().equals(senderUUID)) {
				return;
			}

			audioRelay.acceptAudio(payload);
		});
		audioRelay.register();

		// P1-6：交互转发链（读 manager、查在线玩家、send）整段移入服务端线程；
		// 越权 warn 的每玩家速率限制在 InteractionForwarder 内。
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowInteractionPayload.TYPE, (payload, ctx) ->
			runSafely(ctx, "window_interaction", () -> {
				ServerPlayer player = ctx.player();
				UUID playerUUID = player.getUUID();

				SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

				// 检查权限
				if (!manager.canInteract(playerUUID, payload.windowHandle())) {
					if (ServerWarnThrottle.allow("interact-denied:" + playerUUID, 5000)) {
						LOGGER.warn("Player {} failed interaction permission check for window 0x{} (spam-guarded)",
							playerUUID, Long.toHexString(payload.windowHandle()));
					}
					return;
				}

				// 转发交互给窗口所有者
				InteractionForwarder.forwardInteraction(payload, player);
			}));

		// P1-7：窗口列表广播的 tick 合并 + 限流状态清理
		SharedWindowServerHandler.register();
	}

	/**
	 * P1-5：纯函数（可单测）。null 视为合法（调用方已先行 return，避免告警路径 NPE）；
	 * 超过上限整包丢弃——防恶意客户端把 alive_windows 集合撑到任意大小。
	 */
	static boolean isAliveWindowsAcceptable(long[] handles) {
		return handles == null || handles.length <= MAX_ALIVE_WINDOWS;
	}

	/**
	 * P0-1/P1-6：把 receiver 逻辑调度到服务端线程，并包外层 try/catch——
	 * 单个恶意/异常包引发的任务异常只记 WARN，不再冒泡到 tick 崩溃服务器。
	 */
	private static void runSafely(ServerPlayNetworking.Context ctx, String what, Runnable body) {
		ctx.server().execute(() -> {
			try {
				body.run();
			} catch (Throwable t) {
				UUID player = ctx.player() != null ? ctx.player().getUUID() : null;
				LOGGER.warn("[SERVER] {} handler task failed for player {} (kept server alive)", what, player, t);
			}
		});
	}

	/**
	 * 窗口注销/owner 断连时清转发缓存（P2-2；断连场景另有 tick 巡检兜底）。
	 */
	public static void onWindowRemoved(long windowHandle) {
		frameRelay.onWindowRemoved(windowHandle);
		audioRelay.onWindowRemoved(windowHandle);
	}

	/**
	 * P2-5：SERVER_STOPPED 清 relay 缓存与统计（sharedWindowManager/permissionManager
	 * 由 WaylandCraftCommon 侧清理，见其 onInitialize 注册）。
	 */
	public static void clearSessionState() {
		frameRelay.clear();
		audioRelay.clear();
		SharedWindowServerHandler.reset();
		InteractionForwarder.reset();
		ServerWarnThrottle.reset();
	}

}
