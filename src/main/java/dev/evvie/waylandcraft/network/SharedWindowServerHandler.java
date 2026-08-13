package dev.evvie.waylandcraft.network;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.UUID;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.PermissionManager;
import dev.evvie.waylandcraft.shared.SharedWindowEntry;
import dev.evvie.waylandcraft.shared.SharedWindowManager;
import dev.evvie.waylandcraft.shared.WindowPermission;
import net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking;
import net.minecraft.network.chat.Component;
import net.minecraft.server.level.ServerPlayer;

/**
 * 服务器端窗口共享处理器
 * 处理窗口共享的业务逻辑
 */
public class SharedWindowServerHandler {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-server-handler");
	
	/**
	 * 处理窗口注册请求
	 */
	public static void handleWindowRegister(SharedWindowRegisterPayload payload, ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		
		// 检查是否已经注册过该窗口
		SharedWindowEntry existing = manager.getWindow(payload.windowHandle());
		if(existing != null) {
			LOGGER.warn("Player {} attempted to register already registered window 0x{}", 
				playerUUID, Long.toHexString(payload.windowHandle()));
			return;
		}
		
		// 定向共享：解析目标玩家名 → UUID（找不到或离线则回退为公开共享，避免静默失效）
		UUID targetUUID = null;
		String targetName = payload.targetPlayer();
		if(targetName != null && !targetName.isBlank()) {
			var server = WaylandCraftCommon.instance.server;
			if(server != null) {
				ServerPlayer target = server.getPlayerList().getPlayerByName(targetName);
				if(target != null) {
					targetUUID = target.getUUID();
				} else {
					LOGGER.warn("Player {} requested targeted share to unknown/offline player '{}', falling back to public",
						playerUUID, targetName);
				}
			}
		}
		
		// 注册窗口
		SharedWindowEntry entry = manager.registerWindow(
			payload.windowHandle(),
			playerUUID,
			payload.windowTitle(),
			targetUUID
		);
		
		LOGGER.info("Player {} registered window 0x{}: {} (targeted={})", 
			playerUUID, Long.toHexString(payload.windowHandle()), payload.windowTitle(), targetUUID != null);
		
		// 广播窗口列表给所有玩家
		broadcastWindowListToAll(manager);
	}
	
	/**
	 * 处理窗口注销请求
	 */
	public static void handleWindowUnregister(long windowHandle, ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		
		SharedWindowEntry entry = manager.getWindow(windowHandle);
		if(entry == null) {
			LOGGER.warn("Player {} attempted to unregister non-existent window 0x{}", 
				playerUUID, Long.toHexString(windowHandle));
			return;
		}
		
		// 检查是否是窗口所有者
		if(!entry.getOwnerUUID().equals(playerUUID)) {
			LOGGER.warn("Player {} attempted to unregister window 0x{} owned by {}", 
				playerUUID, Long.toHexString(windowHandle), entry.getOwnerUUID());
			return;
		}
		
		// 注销窗口
		manager.unregisterWindow(windowHandle);
		LOGGER.info("Player {} unregistered window 0x{}", playerUUID, Long.toHexString(windowHandle));
		
		// 广播窗口列表给所有玩家
		broadcastWindowListToAll(manager);
	}
	
	/**
	 * 处理窗口状态更新
	 */
	public static void handleWindowStateUpdate(SharedWindowUpdatePayload payload, ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		
		SharedWindowEntry entry = manager.getWindow(payload.windowHandle());
		if(entry == null) {
			return;
		}
		
		// 检查权限
		if(!entry.hasPermission(playerUUID, WindowPermission.CONTROL)) {
			LOGGER.warn("Player {} denied state update for window 0x{}", 
				playerUUID, Long.toHexString(payload.windowHandle()));
			return;
		}
		
		// 更新窗口状态
		manager.updateWindowState(
			payload.windowHandle(),
			payload.x(), payload.y(),
			payload.width(), payload.height(),
			payload.visible(),
			playerUUID
		);
		
		// 广播状态更新给所有订阅者
		broadcastWindowState(entry, payload);
	}
	
	/**
	 * 处理客户端权限管理命令
	 */
	public static void handlePermissionCommand(PermissionCommandPayload payload, ServerPlayer player) {
		PermissionManager pm = WaylandCraftCommon.instance.permissionManager;
		byte action = payload.action();
		
		switch (action) {
			case PermissionCommandPayload.ACTION_SET_DEFAULT -> {
				WindowPermission perm = WindowPermission.values()[payload.permissionLevel()];
				pm.setDefaultPermission(perm);
				sendPermResponse(player, "Default permission set to " + perm.name());
			}
			case PermissionCommandPayload.ACTION_ALLOW -> {
				String targetName = payload.targetName();
				WindowPermission perm = WindowPermission.values()[payload.permissionLevel()];
				ServerPlayer target = WaylandCraftCommon.instance.server.getPlayerList().getPlayerByName(targetName);
				if (target != null) {
					pm.addToWhitelist(target.getUUID(), perm);
					sendPermResponse(player, "Added " + targetName + " to whitelist with " + perm.name());
				} else {
					sendPermResponse(player, "Player not found: " + targetName);
				}
			}
			case PermissionCommandPayload.ACTION_DENY -> {
				String targetName = payload.targetName();
				ServerPlayer target = WaylandCraftCommon.instance.server.getPlayerList().getPlayerByName(targetName);
				if (target != null) {
					pm.addToBlacklist(target.getUUID());
					sendPermResponse(player, "Added " + targetName + " to blacklist");
				} else {
					sendPermResponse(player, "Player not found: " + targetName);
				}
			}
			case PermissionCommandPayload.ACTION_REMOVE -> {
				String targetName = payload.targetName();
				ServerPlayer target = WaylandCraftCommon.instance.server.getPlayerList().getPlayerByName(targetName);
				if (target != null) {
					pm.removeFromWhitelist(target.getUUID());
					pm.removeFromBlacklist(target.getUUID());
					sendPermResponse(player, "Removed " + targetName + " from whitelist and blacklist");
				} else {
					sendPermResponse(player, "Player not found: " + targetName);
				}
			}
			case PermissionCommandPayload.ACTION_LIST -> {
				StringBuilder sb = new StringBuilder();
				sb.append("=== Permissions ===\n");
				sb.append("Default: ").append(pm.getDefaultPermission().name()).append("\n");
				
				Map<UUID, WindowPermission> whitelist = pm.getWhitelist();
				if (!whitelist.isEmpty()) {
					sb.append("Whitelist:\n");
					for (Map.Entry<UUID, WindowPermission> entry : whitelist.entrySet()) {
						String name = resolveName(player, entry.getKey());
						sb.append("  ").append(name).append(": ").append(entry.getValue().name()).append("\n");
					}
				}
				
				var blacklist = pm.getBlacklist();
				if (!blacklist.isEmpty()) {
					sb.append("Blacklist:\n");
					for (UUID uuid : blacklist) {
						sb.append("  ").append(resolveName(player, uuid)).append("\n");
					}
				}
				
				if (whitelist.isEmpty() && blacklist.isEmpty()) {
					sb.append("No player-specific permissions.");
				}
				
				// 分行发送
				for (String line : sb.toString().split("\\n")) {
					player.sendSystemMessage(Component.literal(line));
				}
			}
			default -> sendPermResponse(player, "Unknown action: " + action);
		}
	}
	
	private static void sendPermResponse(ServerPlayer player, String message) {
		player.sendSystemMessage(Component.literal("[WLC] " + message));
	}
	
	private static String resolveName(ServerPlayer player, UUID uuid) {
		ServerPlayer p = WaylandCraftCommon.instance.server.getPlayerList().getPlayer(uuid);
		return p != null ? p.getName().getString() : uuid.toString().substring(0, 8);
	}
	
	/**
	 * 处理窗口所有者（owner）的授权管理命令（定向授权 / 去除共享 / 列授权）。
	 * 按窗口走 owner 权威校验：非 owner 一律拒绝。
	 */
	public static void handleWindowPermCommand(SharedWindowPermCommandPayload payload, ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		SharedWindowEntry entry = manager.getWindow(payload.windowHandle());

		if(entry == null) {
			sendPermResponse(player, "Window not found: 0x" + Long.toHexString(payload.windowHandle()));
			return;
		}
		if(!entry.getOwnerUUID().equals(playerUUID)) {
			LOGGER.warn("Player {} denied perm command on window 0x{} (not owner)", playerUUID, Long.toHexString(payload.windowHandle()));
			sendPermResponse(player, "Only the window owner can manage its sharing");
			return;
		}

		switch(payload.action()) {
			case SharedWindowPermCommandPayload.ACTION_GRANT -> {
				String targetName = payload.targetName();
				ServerPlayer target = WaylandCraftCommon.instance.server.getPlayerList().getPlayerByName(targetName);
				if(target == null) {
					sendPermResponse(player, "Player not found: " + targetName);
					return;
				}
				if(target.getUUID().equals(entry.getOwnerUUID())) {
					sendPermResponse(player, "Owner already has full control");
					return;
				}
				manager.updatePermission(payload.windowHandle(), target.getUUID(), WindowPermission.VIEW, playerUUID);
				sendPermResponse(player, "Granted VIEW to " + targetName + " on 0x" + Long.toHexString(payload.windowHandle()));
				notifyPermissionChange(payload.windowHandle(), target.getUUID(), WindowPermission.VIEW);
				broadcastWindowListToAll(manager);
			}
			case SharedWindowPermCommandPayload.ACTION_REVOKE -> {
				String targetName = payload.targetName();
				ServerPlayer target = WaylandCraftCommon.instance.server.getPlayerList().getPlayerByName(targetName);
				if(target == null) {
					sendPermResponse(player, "Player not found: " + targetName);
					return;
				}
				manager.updatePermission(payload.windowHandle(), target.getUUID(), WindowPermission.NONE, playerUUID);
				sendPermResponse(player, "Revoked access for " + targetName + " on 0x" + Long.toHexString(payload.windowHandle()));
				notifyPermissionChange(payload.windowHandle(), target.getUUID(), WindowPermission.NONE);
				broadcastWindowListToAll(manager);
			}
			case SharedWindowPermCommandPayload.ACTION_LIST -> {
				StringBuilder sb = new StringBuilder("=== Window 0x" + Long.toHexString(payload.windowHandle()) + " permissions ===\n");
				sb.append("Owner: ").append(resolveName(player, entry.getOwnerUUID())).append(" (CONTROL)\n");
				sb.append("Mode: ").append(entry.isTargeted() ? "targeted" : "public").append("\n");
				boolean any = false;
				for(Map.Entry<UUID, WindowPermission> e : entry.getAllPermissions().entrySet()) {
					if(e.getKey().equals(entry.getOwnerUUID())) continue;
					sb.append("  ").append(resolveName(player, e.getKey())).append(": ").append(e.getValue().name()).append("\n");
					any = true;
				}
				if(!any) sb.append("  (no other players)\n");
				for(String line : sb.toString().split("\\n")) {
					player.sendSystemMessage(Component.literal(line));
				}
			}
			default -> sendPermResponse(player, "Unknown action: " + payload.action());
		}
	}

	/** 给目标玩家发送权限变更通知（用于客户端即时刷新其窗口列表/显示） */
	private static void notifyPermissionChange(long windowHandle, UUID targetUUID, WindowPermission permission) {
		ServerPlayer targetPlayer = WaylandCraftCommon.instance.server.getPlayerList().getPlayer(targetUUID);
		if(targetPlayer != null) {
			SharedWindowPermissionPayload permissionPayload = new SharedWindowPermissionPayload(windowHandle, targetUUID, permission);
			ServerPlayNetworking.send(targetPlayer, permissionPayload);
		}
	}

	/**
	 * 处理权限更新请求
	 */
	public static void handlePermissionUpdate(long windowHandle, UUID targetUUID, WindowPermission permission, ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		
		// 更新权限
		boolean success = manager.updatePermission(windowHandle, targetUUID, permission, playerUUID);
		if(!success) {
			LOGGER.warn("Player {} failed to update permission for window 0x{}", 
				playerUUID, Long.toHexString(windowHandle));
			return;
		}
		
		LOGGER.info("Player {} updated permission for player {} on window 0x{}: {}", 
			playerUUID, targetUUID, Long.toHexString(windowHandle), permission);
		
		// 发送权限更新给目标玩家
		ServerPlayer targetPlayer = player.level().getServer().getPlayerList().getPlayer(targetUUID);
		if(targetPlayer != null) {
			SharedWindowPermissionPayload permissionPayload = new SharedWindowPermissionPayload(
				windowHandle, playerUUID, permission
			);
			ServerPlayNetworking.send(targetPlayer, permissionPayload);
		}
	}
	
	/**
	 * 处理玩家断开连接
	 */
	public static void handlePlayerDisconnect(ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		
		// 清理玩家相关的窗口
		manager.handlePlayerDisconnect(playerUUID);
		
		LOGGER.info("Player {} disconnected, cleaned up shared windows", playerUUID);
		
		// 广播窗口列表给所有玩家
		broadcastWindowListToAll(manager);
	}
	
/**
 * 广播窗口列表给所有在线玩家
 * 
 * 修复：每个玩家看到的窗口列表不同——只有自己有权 VIEW 的窗口才出现在其列表里，
 * 且 perm 字段是该玩家对此窗口的真实权限，而非硬编码 VIEW。
 * 
 * 否则：黑名单/未授权玩家也能看到所有窗口的 handle/title，泄露元数据。
 */
private static void broadcastWindowListToAll(SharedWindowManager manager) {
	var server = WaylandCraftCommon.instance.server;
	if(server == null) {
		return;
	}
	
	int totalWindows = manager.getAllWindows().size();
	for(ServerPlayer player : server.getPlayerList().getPlayers()) {
		UUID playerUUID = player.getUUID();
		List<SharedWindowListPayload.WindowInfo> visible = new ArrayList<>();
		
		for(SharedWindowEntry entry : manager.getAllWindows()) {
			WindowPermission perm = entry.getPermission(playerUUID);
			// 跳过 NONE（黑名单/未授权玩家对该窗口完全不可见），防 handle/title 元数据泄露
			if(perm == WindowPermission.NONE) {
				continue;
			}
			visible.add(new SharedWindowListPayload.WindowInfo(
				entry.getWindowHandle(),
				entry.getOwnerUUID(),
				entry.getWindowTitle(),
				perm
			));
		}
		
		SharedWindowListPayload listPayload = new SharedWindowListPayload(visible);
		ServerPlayNetworking.send(player, listPayload);
	}
	
	LOGGER.info("Broadcasting window list: {} windows total", totalWindows);
}
	
	/**
	 * 广播窗口状态更新给订阅者
	 */
	private static void broadcastWindowState(SharedWindowEntry entry, SharedWindowUpdatePayload payload) {
		var server = WaylandCraftCommon.instance.server;
		if(server == null) return;
		for(ServerPlayer player : server.getPlayerList().getPlayers()) {
			UUID playerUUID = player.getUUID();
			if(entry.hasPermission(playerUUID, WindowPermission.VIEW)) {
				ServerPlayNetworking.send(player, payload);
			}
		}
		LOGGER.debug("Broadcasting state update for window 0x{}", Long.toHexString(entry.getWindowHandle()));
	}
}
