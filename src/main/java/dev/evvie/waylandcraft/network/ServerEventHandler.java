package dev.evvie.waylandcraft.network;

import java.util.UUID;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.SharedWindowManager;
import net.fabricmc.fabric.api.networking.v1.ServerPlayConnectionEvents;
import net.minecraft.server.level.ServerPlayer;

/**
 * 服务器事件处理器
 * 处理玩家连接/断开事件，清理共享窗口数据
 */
public class ServerEventHandler {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-server-events");
	
	/**
	 * 注册服务器事件处理器
	 */
	public static void register() {
		// 玩家加入服务器
		ServerPlayConnectionEvents.JOIN.register((handler, sender, server) -> {
			ServerPlayer player = handler.getPlayer();
			handlePlayerJoin(player);
		});
		
		// 玩家离开服务器
		ServerPlayConnectionEvents.DISCONNECT.register((handler, server) -> {
			ServerPlayer player = handler.getPlayer();
			handlePlayerDisconnect(player, server);
		});
		
		LOGGER.info("Server event handlers registered");
	}
	
	/**
	 * 处理玩家加入
	 */
	private static void handlePlayerJoin(ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		LOGGER.info("Player {} joined server", playerUUID);

		// ACL-2：按新观众默认策略处理公开窗口（allow=物化 DEFAULT 授权，保持既有
		// 行为兼容；ask=收集邀请请求发给各 owner；deny=不授权）。不再无条件放行。
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		var invites = manager.onPlayerJoin(playerUUID);
		if (!invites.isEmpty()) {
			SharedWindowServerHandler.sendInviteRequests(invites);
		}

		// 发送当前共享窗口列表给新加入的玩家
		sendWindowListToPlayer(player);
	}
	
	/**
	 * 处理玩家断开连接
	 */
	private static void handlePlayerDisconnect(ServerPlayer player, net.minecraft.server.MinecraftServer server) {
		UUID playerUUID = player.getUUID();
		LOGGER.info("Player {} disconnecting, cleaning up shared windows", playerUUID);
		
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		// 清理玩家相关的窗口。
		// ACL-4：拿回因 owner 退出而消失的句柄，逐个即时清转发缓存并向观众
		// 发"权限移除"信号（不等 tick 巡检/列表合并广播）。
		java.util.List<Long> removed = manager.handlePlayerDisconnect(playerUUID);
		for (long handle : removed) {
			WaylandCraftNetworking.onWindowRemoved(handle);
			for (ServerPlayer viewer : server.getPlayerList().getPlayers()) {
				if (viewer.getUUID().equals(playerUUID)) continue;
				SharedWindowServerHandler.notifyPermissionRemoved(handle, viewer);
			}
		}

		// 广播更新的窗口列表给所有剩余玩家
		// P1-7：改走 tick 合并广播（断线重连风暴下不再逐事件全服刷）
		SharedWindowServerHandler.requestWindowListBroadcast();

		LOGGER.info("Cleaned up {} shared window(s) for player {}", removed.size(), playerUUID);
	}
	
	/**
	 * 发送窗口列表给指定玩家
	 *
	 * 与 SharedWindowServerHandler.broadcastWindowListToAll 同步的两处修正：
	 * - P1-1：canSend 过滤（旧客户端未注册列表 payload 时不白写）；
	 * - NONE 权限窗口不发送（同列表广播的元数据泄露修复——原实现把全部窗口
	 *   连同 NONE 一起发给新玩家，泄露 handle/title；JOIN 流程里
	 *   grantViewToNewPlayer 先跑，合法窗口仍会出现在列表）。
	 */
	private static void sendWindowListToPlayer(ServerPlayer player) {
		if (!net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking.canSend(player, SharedWindowListPayload.TYPE)) {
			return;
		}
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		// 构建窗口列表
		var windowList = new java.util.ArrayList<SharedWindowListPayload.WindowInfo>();
		UUID playerUUID = player.getUUID();

		// ACL-3：新玩家的首屏列表同样走合成判定（黑名单/未授权不可见）
		long nowMs = System.currentTimeMillis();
		for(var entry : manager.getAllWindows()) {
			var perm = manager.effectivePermission(entry, playerUUID, nowMs);
			if (perm == dev.evvie.waylandcraft.shared.WindowPermission.NONE) {
				continue;
			}
			windowList.add(new SharedWindowListPayload.WindowInfo(
				entry.getWindowHandle(),
				entry.getOwnerUUID(),
				entry.getWindowTitle(),
				perm
			));
		}

		// 发送给玩家
		SharedWindowListPayload payload = new SharedWindowListPayload(windowList);
		net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking.send(player, payload);

		LOGGER.debug("Sent window list to player {}: {} windows", playerUUID, windowList.size());
	}
}
