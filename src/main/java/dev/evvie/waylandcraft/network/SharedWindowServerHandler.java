package dev.evvie.waylandcraft.network;

import java.util.ArrayList;

import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicBoolean;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.PermissionManager;
import dev.evvie.waylandcraft.shared.SharedWindowEntry;
import dev.evvie.waylandcraft.shared.SharedWindowManager;
import dev.evvie.waylandcraft.shared.WindowPermission;
import dev.evvie.waylandcraft.utils.ServerWarnThrottle;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerLifecycleEvents;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerTickEvents;
import net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking;
import net.minecraft.network.chat.Component;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.server.permissions.Permissions;

/**
 * 服务器端窗口共享处理器
 * 处理窗口共享的业务逻辑
 *
 * 服务端加固（server-polish）：
 * - P0-1：权限管理命令要求 op 权限等级 ≥2（原实现任何玩家可改全局默认权限），
 *   permissionLevel 钳位防 values()[越界] 崩服。
 * - P1-7：per-UUID 窗口数上限、注册速率门、title 截断；窗口列表广播改脏标记、
 *   每 tick 合并一次（防注册风暴 = 每包全服 O(players×windows) 广播）；
 *   注册路径 INFO 降 debug。
 * - P2-3：state 更新坐标/尺寸 clamp 后才入库并广播。
 *
 * 所有 handle* 方法只从 ctx.server().execute 任务内调用（服务端线程），
 * 但限流状态仍用并发容器防御未来调用点变化。
 */
public class SharedWindowServerHandler {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-server-handler");

	/** P1-7：单玩家最多同时共享的窗口数 */
	static final int MAX_WINDOWS_PER_PLAYER = 8;

	/** P1-7：单玩家注册速率门（每秒请求数上限） */
	static final int MAX_REGISTERS_PER_SECOND = 4;

	/** P1-7：窗口标题服务端截断长度 */
	static final int MAX_TITLE_LENGTH = 64;

	/** P1-7：per-UUID 注册速率窗状态（仅服务端线程读写，ConcurrentHashMap 防御性） */
	private static final Map<UUID, long[]> registerRates = new ConcurrentHashMap<>(); // [windowStartMs, count]

	/** P1-7：窗口列表广播脏标记——tick 合并一次 */
	private static final AtomicBoolean windowListDirty = new AtomicBoolean(false);

	/**
	 * 与 receiver 注册一起调用：tick 合并广播 + 速率窗过期清理 + 跨会话状态清零。
	 */
	public static void register() {
		ServerTickEvents.END_SERVER_TICK.register(server -> {
			if (windowListDirty.compareAndSet(true, false)) {
				broadcastWindowListToAll(WaylandCraftCommon.instance.sharedWindowManager);
			}
			// 清理 30s 无注册的速率窗，防 map 随玩家数无界增长
			long now = System.currentTimeMillis();
			registerRates.entrySet().removeIf(e -> now - e.getValue()[0] > 30_000);
		});
		ServerLifecycleEvents.SERVER_STOPPED.register(server -> reset());
	}

	/** P2-5：跨会话清空限流/脏标记状态 */
	public static void reset() {
		registerRates.clear();
		windowListDirty.set(false);
	}

	/**
	 * P1-7：纯函数，可单测——标题去控制字符并截断到 64。
	 */
	public static String sanitizeTitle(String title) {
		if (title == null || title.isEmpty()) return "";
		String sanitized = title.replace('\n', ' ').replace('\r', ' ').replace('\t', ' ');
		return sanitized.length() <= MAX_TITLE_LENGTH ? sanitized : sanitized.substring(0, MAX_TITLE_LENGTH);
	}

	/**
	 * P1-7：纯函数速率门——nowMs 时刻该 UUID 是否允许一次注册。
	 * interval 内计数 ≥ max 则拒绝。allow=false 时不消耗额度。
	 */
	static boolean allowRegister(Map<UUID, long[]> rates, UUID player, long nowMs, long windowMs, int max) {
		long[] state = rates.get(player);
		if (state == null || nowMs - state[0] >= windowMs) {
			rates.put(player, new long[]{nowMs, 1});
			return true;
		}
		if (state[1] >= max) {
			return false;
		}
		state[1]++;
		return true;
	}

	/**
	 * 处理窗口注册请求（服务端线程）
	 */
	public static void handleWindowRegister(SharedWindowRegisterPayload payload, ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		// P1-7：注册速率门（4/s），超限静默丢弃（限频 WARN 防日志洪泛）
		if (!allowRegister(registerRates, playerUUID, System.currentTimeMillis(), 1000, MAX_REGISTERS_PER_SECOND)) {
			if (ServerWarnThrottle.allow("register-rate:" + playerUUID, 5000)) {
				LOGGER.warn("Player {} exceeded register rate limit ({}/s), packet dropped", playerUUID, MAX_REGISTERS_PER_SECOND);
			}
			return;
		}

		// 检查是否已经注册过该窗口
		SharedWindowEntry existing = manager.getWindow(payload.windowHandle());
		if(existing != null) {
			if (ServerWarnThrottle.allow("register-dup:" + playerUUID, 5000)) {
				LOGGER.warn("Player {} attempted to register already registered window 0x{}",
					playerUUID, Long.toHexString(payload.windowHandle()));
			}
			return;
		}

		// P1-7：per-UUID 窗口数上限，防单玩家无限注册（registry 膨胀 + 全服广播放大）
		int owned = 0;
		for (SharedWindowEntry entry : manager.getAllWindows()) {
			if (entry.getOwnerUUID().equals(playerUUID)) owned++;
		}
		if (owned >= MAX_WINDOWS_PER_PLAYER) {
			if (ServerWarnThrottle.allow("register-cap:" + playerUUID, 5000)) {
				LOGGER.warn("Player {} hit shared window cap ({}), rejecting window 0x{}",
					playerUUID, MAX_WINDOWS_PER_PLAYER, Long.toHexString(payload.windowHandle()));
			}
			sendPermResponse(player, "Shared window limit reached (" + MAX_WINDOWS_PER_PLAYER + ")");
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

		// P1-7：title 截断，防超长恶意标题在列表广播/日志里放大
		String title = sanitizeTitle(payload.windowTitle());

		// 注册窗口
		SharedWindowEntry entry = manager.registerWindow(
			payload.windowHandle(),
			playerUUID,
			title,
			targetUUID
		);

		LOGGER.debug("Player {} registered window 0x{}: {} (targeted={})",
			playerUUID, Long.toHexString(entry.getWindowHandle()), title, targetUUID != null);

		// P1-7：标脏，tick 合并广播（原每注册立即全服 O(players×windows) 广播）
		requestWindowListBroadcast();
	}

	/**
	 * 处理窗口注销请求
	 */
	public static void handleWindowUnregister(long windowHandle, ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		SharedWindowEntry entry = manager.getWindow(windowHandle);
		if(entry == null) {
			if (ServerWarnThrottle.allow("unregister-ghost:" + playerUUID, 5000)) {
				LOGGER.warn("Player {} attempted to unregister non-existent window 0x{}",
					playerUUID, Long.toHexString(windowHandle));
			}
			return;
		}

		// 检查是否是窗口所有者
		if(!entry.getOwnerUUID().equals(playerUUID)) {
			if (ServerWarnThrottle.allow("unregister-foreign:" + playerUUID, 5000)) {
				LOGGER.warn("Player {} attempted to unregister window 0x{} owned by {}",
					playerUUID, Long.toHexString(windowHandle), entry.getOwnerUUID());
			}
			return;
		}

		// 注销窗口
		manager.unregisterWindow(windowHandle);
		// P2-2：窗口注销立即清转发缓存/队列，防旧帧泄露与内存残留
		WaylandCraftNetworking.onWindowRemoved(windowHandle);
		LOGGER.debug("Player {} unregistered window 0x{}", playerUUID, Long.toHexString(windowHandle));

		// P1-7：标脏，tick 合并广播
		requestWindowListBroadcast();
	}

	// ===== P2-3：state 更新 clamp（纯函数，可单测） =====

	/** 窗口状态坐标合法范围（方块世界，±1e7 足够覆盖任何地图） */
	static final int MAX_STATE_COORD = 10_000_000;
	/** 窗口状态尺寸合法范围 */
	static final int MAX_STATE_DIM = 8192;

	static int clampStateCoord(int value) {
		return Math.max(-MAX_STATE_COORD, Math.min(MAX_STATE_COORD, value));
	}

	static int clampStateDimension(int value) {
		return Math.max(0, Math.min(MAX_STATE_DIM, value));
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
			if (ServerWarnThrottle.allow("state-denied:" + playerUUID + ":" + payload.windowHandle(), 5000)) {
				LOGGER.warn("Player {} denied state update for window 0x{}",
					playerUUID, Long.toHexString(payload.windowHandle()));
			}
			return;
		}

		// P2-3：负值/超限坐标先 clamp，再入库与广播（防客户端把窗口甩到 2^31）
		int x = clampStateCoord(payload.x());
		int y = clampStateCoord(payload.y());
		int width = clampStateDimension(payload.width());
		int height = clampStateDimension(payload.height());
		SharedWindowUpdatePayload clamped =
			new SharedWindowUpdatePayload(payload.windowHandle(), x, y, width, height, payload.visible());

		// 更新窗口状态
		manager.updateWindowState(
			clamped.windowHandle(),
			clamped.x(), clamped.y(),
			clamped.width(), clamped.height(),
			clamped.visible(),
			playerUUID
		);

		// 广播状态更新给所有订阅者
		broadcastWindowState(entry, clamped);
	}

	/**
	 * P0-1：鉴权门（纯函数，可单测）。全局权限命令的所有动作——SET_DEFAULT、
	 * ALLOW、DENY、REMOVE，乃至 LIST（输出白/黑名单内容属敏感运维数据）——
	 * 一律要求 op 权限等级 ≥2，不允许任何 action 绕开门。
	 * hasGameMasterPermission 由调用点从 player.permissions() 注入。
	 */
	static boolean isPermissionCommandAuthorized(boolean hasGameMasterPermission) {
		return hasGameMasterPermission;
	}

	/**
	 * 处理客户端权限管理命令（服务端线程，由 receiver 包 execute + try/catch 调入）
	 *
	 * P0-1：这是全局权限模型的写入口（默认权限/白名单/黑名单），原实现零鉴权——
	 * 任何玩家发包即可把全局默认放开为 CONTROL。现在要求 op 等级 ≥2
	 * （Permissions.COMMANDS_GAMEMASTER，本映射版本无整数 hasPermissions(2)，
	 * 语义等价：旧 level 2 = gamemaster），非 op 拒绝并回提示。
	 */
	public static void handlePermissionCommand(PermissionCommandPayload payload, ServerPlayer player) {
		if (!isPermissionCommandAuthorized(player.permissions().hasPermission(Permissions.COMMANDS_GAMEMASTER))) {
			sendPermResponse(player, "Permission command denied: operator level 2 required");
			LOGGER.debug("Non-op player {} attempted permission command action={}", player.getUUID(), payload.action());
			return;
		}

		PermissionManager pm = WaylandCraftCommon.instance.permissionManager;
		byte action = payload.action();

		switch (action) {
			case PermissionCommandPayload.ACTION_SET_DEFAULT -> {
				// P0-1：钳位而非裸下标——恶意字节（负数/≥4）曾直接 AIOOBE 崩服
				WindowPermission perm = WindowPermission.fromLevelClamped(payload.permissionLevel());
				pm.setDefaultPermission(perm);
				sendPermResponse(player, "Default permission set to " + perm.name());
			}
			case PermissionCommandPayload.ACTION_ALLOW -> {
				String targetName = payload.targetName();
				WindowPermission perm = WindowPermission.fromLevelClamped(payload.permissionLevel());
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
			if (ServerWarnThrottle.allow("permcmd-denied:" + playerUUID + ":" + payload.windowHandle(), 5000)) {
				LOGGER.warn("Player {} denied perm command on window 0x{} (not owner)", playerUUID, Long.toHexString(payload.windowHandle()));
			}
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
				requestWindowListBroadcast();
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
				requestWindowListBroadcast();
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
		if(targetPlayer != null && ServerPlayNetworking.canSend(targetPlayer, SharedWindowPermissionPayload.TYPE)) {
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
		if(targetPlayer != null && ServerPlayNetworking.canSend(targetPlayer, SharedWindowPermissionPayload.TYPE)) {
			SharedWindowPermissionPayload permissionPayload = new SharedWindowPermissionPayload(
				windowHandle, playerUUID, permission
			);
			ServerPlayNetworking.send(targetPlayer, permissionPayload);
		}
	}

	/**
	 * P1-7：注册/注销/授权变化只标脏，实际广播在 END_SERVER_TICK 合并执行
	 * （风暴场景下 N 个注册包 = 1 次全服广播，而不是 N 次）。
	 */
	public static void requestWindowListBroadcast() {
		windowListDirty.set(true);
	}

/**
 * 广播窗口列表给所有在线玩家（tick 合并调用）
 *
 * 修复：每个玩家看到的窗口列表不同——只有自己有权 VIEW 的窗口才出现在其列表里，
 * 且 perm 字段是该玩家对此窗口的真实权限，而非硬编码 VIEW。
 *
 * 否则：黑名单/未授权玩家也能看到所有窗口的 handle/title，泄露元数据。
 *
 * P1-1：发送前 canSend 过滤（旧客户端没注册该 payload 时避免无谓写出）。
 */
private static void broadcastWindowListToAll(SharedWindowManager manager) {
	var server = WaylandCraftCommon.instance.server;
	if(server == null) {
		return;
	}

	int totalWindows = manager.getAllWindows().size();
	int sent = 0;
	for(ServerPlayer player : server.getPlayerList().getPlayers()) {
		if (!ServerPlayNetworking.canSend(player, SharedWindowListPayload.TYPE)) {
			continue;
		}
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
		sent++;
	}

	// P1-7：INFO 降 debug（注册风暴场景下逐次 INFO 是日志洪水源）
	LOGGER.debug("Broadcasting window list: {} windows total, {} players", totalWindows, sent);
}

	/**
	 * 广播窗口状态更新给订阅者
	 */
	private static void broadcastWindowState(SharedWindowEntry entry, SharedWindowUpdatePayload payload) {
		var server = WaylandCraftCommon.instance.server;
		if(server == null) return;
		for(ServerPlayer player : server.getPlayerList().getPlayers()) {
			UUID playerUUID = player.getUUID();
			if(entry.hasPermission(playerUUID, WindowPermission.VIEW)
				&& ServerPlayNetworking.canSend(player, SharedWindowUpdatePayload.TYPE)) {
				ServerPlayNetworking.send(player, payload);
			}
		}
		LOGGER.debug("Broadcasting state update for window 0x{}", Long.toHexString(entry.getWindowHandle()));
	}
}
