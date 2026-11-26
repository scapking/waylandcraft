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
import dev.evvie.waylandcraft.shared.ShareAcl;
import dev.evvie.waylandcraft.shared.ShareAclAudit;
import dev.evvie.waylandcraft.shared.SharedWindowEntry;
import dev.evvie.waylandcraft.shared.SharedWindowManager;
import dev.evvie.waylandcraft.shared.WindowGrant;
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

	/** ACL-1：到期扫描间隔（tick）与计数器——只在服务端线程读写 */
	static final int EXPIRY_SWEEP_INTERVAL_TICKS = 20; // 1s
	private static int expiryTickCounter = 0;

	/** ACL-2：邀请提示冷却（key = handle + ":" + requester → 上次发出时刻）。
	 *  防恶意观众反复 join/重连刷爆 owner 聊天；仅服务端线程读写。 */
	private static final Map<String, Long> inviteCooldowns = new ConcurrentHashMap<>();
	static final long INVITE_COOLDOWN_MS = 60_000;
	static final long INVITE_COOLDOWN_RETIRE_MS = 15 * 60_000L;
	/** 单次 collect/send 调用的邀请上限（防注册风暴 × 全员 = 提示洪泛） */
	static final int MAX_INVITES_PER_BATCH = 16;

	/**
	 * 与 receiver 注册一起调用：tick 合并广播 + 速率窗过期清理 + ACL 到期扫描 + 跨会话状态清零。
	 */
	public static void register() {
		ServerTickEvents.END_SERVER_TICK.register(server -> {
			if (windowListDirty.compareAndSet(true, false)) {
				broadcastWindowListToAll(WaylandCraftCommon.instance.sharedWindowManager);
			}
			// 清理 30s 无注册的速率窗，防 map 随玩家数无界增长
			long now = System.currentTimeMillis();
			registerRates.entrySet().removeIf(e -> now - e.getValue()[0] > 30_000);
			inviteCooldowns.entrySet().removeIf(e -> now - e.getValue() > INVITE_COOLDOWN_RETIRE_MS);

			// ACL-1：限时授权到期扫描（每 20 tick，服务端线程——满足主线程约束）
			if (++expiryTickCounter >= EXPIRY_SWEEP_INTERVAL_TICKS) {
				expiryTickCounter = 0;
				sweepExpiredGrants(now);
			}
		});
		ServerLifecycleEvents.SERVER_STOPPED.register(server -> reset());
	}

	/** P2-5：跨会话清空限流/脏标记/邀请冷却状态 */
	public static void reset() {
		registerRates.clear();
		windowListDirty.set(false);
		expiryTickCounter = 0;
		inviteCooldowns.clear();
	}

	/**
	 * ACL-1：到期记录回收 + 即时传播（服务端线程，tick 调用；测试接缝 allow 直接调用）。
	 * 对每个到期：S2C 权限移除信号（SharedWindowPermissionPayload NONE，复用既有
	 * removeWindow 传播路径）、relay 缓存清理、列表标脏、审计日志/计数。
	 */
	static void sweepExpiredGrants(long nowMs) {
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		var expired = manager.expireGrants(nowMs);
		for (SharedWindowManager.GrantExpired e : expired) {
			// 观众端立即销毁该窗口显示与缓冲（同 revoke 路径）
			notifyPermissionChange(e.windowHandle(), e.playerUUID(), WindowPermission.NONE);
			if (ServerWarnThrottle.allow("grant-expired:" + e.windowHandle() + ":" + e.playerUUID(), 5000)) {
				LOGGER.info("ACL grant expired: window 0x{} player {} (was {})",
					Long.toHexString(e.windowHandle()), e.playerUUID(), e.grant().level());
			}
			// 到期后该玩家对该窗口若无其他记录 → 清该 handle 的 relay 缓存
			// （窗口对他人仍共享，只清这个观众的视角数据不可行，
			// relay 缓存按窗口粒度：仅当再无任何 VIEW 持有者时整窗清理）
			if (!hasAnyViewer(manager, e.windowHandle())) {
				WaylandCraftNetworking.onWindowRemoved(e.windowHandle());
			}
			requestWindowListBroadcast();
		}
	}

	/** 窗口是否仍有至少一个非 owner 的 VIEW+ 观众（relay 缓存整窗清理判定用）。 */
	static boolean hasAnyViewer(SharedWindowManager manager, long windowHandle) {
		SharedWindowEntry entry = manager.getWindow(windowHandle);
		if (entry == null) return false;
		var server = WaylandCraftCommon.instance.server;
		if (server == null) return false;
		long now = System.currentTimeMillis();
		for (ServerPlayer player : server.getPlayerList().getPlayers()) {
			UUID uuid = player.getUUID();
			if (uuid.equals(entry.getOwnerUUID())) continue;
			if (manager.effectivePermission(entry, uuid, now).hasPermission(WindowPermission.VIEW)) return true;
		}
		return false;
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
		// ACL-6：同 owner 重注册（窗口销毁重建、handle/xid 复用）→ 解析完 title/target
		// 后强制替换为全新 entry（旧授权/订阅清空）。
		// 不同 owner 抢占同一 handle → 拒绝（原行为），并计审计。
		SharedWindowEntry existing = manager.getWindow(payload.windowHandle());
		boolean reRegister = existing != null;
		if(reRegister) {
			if (!existing.getOwnerUUID().equals(playerUUID)) {
				if (ServerWarnThrottle.allow("register-dup:" + playerUUID, 5000)) {
					LOGGER.warn("Player {} attempted to hijack window 0x{} owned by {}",
						playerUUID, Long.toHexString(payload.windowHandle()), existing.getOwnerUUID());
				}
				ShareAclAudit.reregisterRejected.incrementAndGet();
				return;
			}
		}

		// P1-7：per-UUID 窗口数上限，防单玩家无限注册（registry 膨胀 + 全服广播放大）
		// ACL-6：同 owner 重注册（reRegister）不新增名额——该句柄本就计入 owned，
		// 满额玩家的窗口重建若被拒会出现"旧 entry 卡住无法更新共享"的死区。
		int owned = 0;
		for (SharedWindowEntry entry : manager.getAllWindows()) {
			if (entry.getOwnerUUID().equals(playerUUID)) owned++;
		}
		if (!reRegister && owned >= MAX_WINDOWS_PER_PLAYER) {
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
		SharedWindowEntry entry;
		if (reRegister) {
			// ACL-6：同 owner 重建——替换 entry（旧 ACL + 旧订阅清空），并即时
			// 向旧授权持有者发"权限移除"信号 + 清转发缓存，再走正常策略物化。
			SharedWindowEntry old = manager.replaceWindowForSameOwner(
				payload.windowHandle(), playerUUID, title, targetUUID);
			if (old != null) {
				for (UUID v : old.grantsSnapshot().keySet()) {
					if (v.equals(playerUUID)) continue;
					notifyPermissionChange(payload.windowHandle(), v, WindowPermission.NONE);
				}
				ShareAclAudit.log("REREGISTER", payload.windowHandle(), playerUUID, playerUUID,
					WindowPermission.CONTROL, null, WindowGrant.NEVER_EXPIRES);
				ShareAclAudit.reregisterCleared.incrementAndGet();
				LOGGER.info("Player {} re-registered window 0x{} (same owner): old ACL cleared",
					playerUUID, Long.toHexString(payload.windowHandle()));
			}
			entry = manager.getWindow(payload.windowHandle());
			WaylandCraftNetworking.onWindowRemoved(payload.windowHandle());
		} else {
			entry = manager.registerWindow(payload.windowHandle(), playerUUID, title, targetUUID);
		}

		LOGGER.debug("Player {} registered window 0x{}: {} (targeted={})",
			playerUUID, Long.toHexString(entry.getWindowHandle()), title, targetUUID != null);

		// ACL-2：公开窗口对在线观众应用默认策略（ASK → 邀请请求交网络层）
		if (targetUUID == null) {
			sendInviteRequests(manager.applyPolicyForAudience(entry));
		}

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

		// ACL-4：注销前抓观众名单，随后逐个即时发"权限移除"信号
		// （列表合并广播最快也要下个 tick；信号 + 客户端销毁路径保证显示/音频立刻停）
		var viewers = entry.grantsSnapshot().keySet();
		// 注销窗口
		manager.unregisterWindow(windowHandle);
		for (UUID v : viewers) {
			if (v.equals(playerUUID)) continue;
			notifyPermissionChange(windowHandle, v, WindowPermission.NONE);
		}
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

		// ACL-3：state 权限走合成判定（owner 恒 CONTROL；黑白名单压过记录）
		if(!manager.effectivePermission(entry, playerUUID, System.currentTimeMillis()).hasPermission(WindowPermission.CONTROL)) {
			if (ServerWarnThrottle.allow("state-denied:" + playerUUID + ":" + payload.windowHandle(), 5000)) {
				LOGGER.warn("Player {} denied state update for window 0x{}",
					playerUUID, Long.toHexString(payload.windowHandle()));
				ShareAclAudit.logDeny("state", payload.windowHandle(), playerUUID);
			}
			ShareAclAudit.deniedState.incrementAndGet();
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
			case PermissionCommandPayload.ACTION_SET_NEW_VIEWER_POLICY -> {
				// ACL-2：op 设置新观众默认策略。非法名回退当前值（不隐式改严也不隐式放松）。
				ShareAcl.NewViewerPolicy before = pm.getNewViewerPolicy();
				ShareAcl.NewViewerPolicy next = ShareAcl.NewViewerPolicy.fromNameOr(payload.targetName(), before);
				pm.setNewViewerPolicy(next);
				sendPermResponse(player, "New viewer policy: " + next.name().toLowerCase()
					+ (next == ShareAcl.NewViewerPolicy.ALLOW ? " (re-applying DEFAULT grants to online viewers)" : ""));
				if (next == ShareAcl.NewViewerPolicy.ALLOW) {
					// 切回 allow：对全体公开窗口×在线观众回放一次补授（显式记录/墓碑不动）
					WaylandCraftCommon.instance.sharedWindowManager.reapplyAllowPolicyAll();
				}
				if (next != before && next != ShareAcl.NewViewerPolicy.ALLOW) {
					// 切到 ASK/DENY 不影响既有授权（只改变之后的 join/register 行为）——文档注明
					LOGGER.info("New viewer policy {} -> {} (existing grants unchanged; tightening applies to future joins)",
						before, next);
				}
				requestWindowListBroadcast();
			}
			case PermissionCommandPayload.ACTION_LIST -> {
				StringBuilder sb = new StringBuilder();
				sb.append("=== Permissions ===\n");
				sb.append("Default: ").append(pm.getDefaultPermission().name()).append("\n");
				sb.append("New viewer policy: ").append(pm.getNewViewerPolicy().name().toLowerCase()).append("\n");

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
	 * 处理窗口所有者（owner）的授权管理命令（授权/撤销/列授权/ACL 报表）。
	 * GRANT/REVOKE/LIST 按窗口走 owner 权威校验：非 owner 一律拒绝；
	 * ACL 报表例外——非 owner/非 op 也可查，但服务端只返回其本人相关行。
	 */
	public static void handleWindowPermCommand(SharedWindowPermCommandPayload payload, ServerPlayer player) {
		UUID playerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;

		// ACL-5：报表走独立鉴权（可见行由 isOwner/isOp 过滤），不进 owner 校验门
		if (payload.action() == SharedWindowPermCommandPayload.ACTION_ACL) {
			boolean isOp = player.permissions().hasPermission(Permissions.COMMANDS_GAMEMASTER);
			for (String line : buildAclReport(manager, player, payload.windowHandle(), isOp)) {
				player.sendSystemMessage(Component.literal(line));
			}
			return;
		}

		SharedWindowEntry entry = manager.getWindow(payload.windowHandle());

		if(entry == null) {
			sendPermResponse(player, "Window not found: 0x" + Long.toHexString(payload.windowHandle()));
			return;
		}
		// ACL-6：owner 权威门（ShareAcl 纯函数——观众/管理员伪造 grant/revoke/list 包统一在此拒）
		if(!ShareAcl.canManageWindow(entry.getOwnerUUID(), playerUUID)) {
			if (ServerWarnThrottle.allow("permcmd-denied:" + playerUUID + ":" + payload.windowHandle(), 5000)) {
				LOGGER.warn("Player {} denied perm command on window 0x{} (not owner)", playerUUID, Long.toHexString(payload.windowHandle()));
				ShareAclAudit.logDeny("permcmd-not-owner", payload.windowHandle(), playerUUID);
			}
			ShareAclAudit.deniedNotOwner.incrementAndGet();
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
				// ACL-1：入站等级查表 + 保守钳位（非法/越界 → VIEW，绝不钳位提权到 CONTROL），
				// 时长 durationSeconds>0 = 限时授权，0 = 永久。
				WindowPermission requested = ShareAcl.clampGrantedLevel(
					WindowPermission.fromLevel(payload.permissionLevel()));
				long now = System.currentTimeMillis();
				long expiresAt = payload.durationSeconds() > 0
					? now + payload.durationSeconds() * 1000L
					: WindowGrant.NEVER_EXPIRES;
				WindowGrant g = manager.grantTimed(payload.windowHandle(), target.getUUID(), requested,
					playerUUID, expiresAt, WindowGrant.GrantSource.MANUAL, now);
				if (g == null) {
					sendPermResponse(player, "Grant rejected (owner check failed)");
					return;
				}
				ShareAclAudit.log("GRANT", payload.windowHandle(), target.getUUID(), playerUUID,
					requested, WindowGrant.GrantSource.MANUAL, expiresAt);
				ShareAclAudit.countGrant(WindowGrant.GrantSource.MANUAL);
				sendPermResponse(player, "Granted " + requested.name() + " to " + targetName + " on 0x"
					+ Long.toHexString(payload.windowHandle())
					+ (expiresAt != WindowGrant.NEVER_EXPIRES ? " for " + payload.durationSeconds() + "s" : ""));
				notifyPermissionChange(payload.windowHandle(), target.getUUID(), requested);
				requestWindowListBroadcast();
			}
			case SharedWindowPermCommandPayload.ACTION_REVOKE -> {
				String targetName = payload.targetName();
				ServerPlayer target = WaylandCraftCommon.instance.server.getPlayerList().getPlayerByName(targetName);
				if(target == null) {
					sendPermResponse(player, "Player not found: " + targetName);
					return;
				}
				SharedWindowEntry e = manager.updatePermissionAndGetEntry(payload.windowHandle(),
					target.getUUID(), WindowPermission.NONE, playerUUID);
				if (e == null) {
					sendPermResponse(player, "Revoke rejected (window gone / not owner / targets owner)");
					return;
				}
				ShareAclAudit.log("REVOKE", payload.windowHandle(), target.getUUID(), playerUUID,
					WindowPermission.NONE, WindowGrant.GrantSource.MANUAL, WindowGrant.NEVER_EXPIRES);
				ShareAclAudit.revoked.incrementAndGet();
				sendPermResponse(player, "Revoked access for " + targetName + " on 0x" + Long.toHexString(payload.windowHandle()));
				notifyPermissionChange(payload.windowHandle(), target.getUUID(), WindowPermission.NONE);
				requestWindowListBroadcast();
			}
			case SharedWindowPermCommandPayload.ACTION_LIST -> {
				// ACL-5：单窗口授权明细（含来源/到期）
				long now = System.currentTimeMillis();
				StringBuilder sb = new StringBuilder("=== Window 0x" + Long.toHexString(payload.windowHandle()) + " permissions ===\n");
				sb.append("Owner: ").append(resolveName(player, entry.getOwnerUUID())).append(" (CONTROL)\n");
				sb.append("Mode: ").append(entry.isTargeted() ? "targeted" : "public").append("\n");
				sb.append("Policy default: ").append(manager.currentPolicy()).append("\n");
				boolean any = false;
				for(Map.Entry<UUID, WindowGrant> e : entry.grantsSnapshot().entrySet()) {
					if(e.getKey().equals(entry.getOwnerUUID())) continue;
					sb.append("  ").append(resolveName(player, e.getKey())).append(": ")
						.append(ShareAcl.describeGrant(e.getValue(), now)).append("\n");
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

	// ===== ACL-5：/wl share acl 报表 =====

	/**
	 * ACL 报表可见性门（纯函数，可单测）：owner 看自己窗口全量、op 看全服全量，
	 * 其余玩家只看"与自己相关"的行（自己是观众的那些窗口的自己的权限行）。
	 */
	static boolean aclRowVisible(boolean isOp, boolean rowWindowOwner, boolean rowPlayerIsRequester) {
		if (isOp || rowWindowOwner) return true;
		return rowPlayerIsRequester;
	}

	/** 构建 ACL 一屏报表（handleFilter=0 全部窗口）。 */
	static List<String> buildAclReport(SharedWindowManager manager, ServerPlayer requester, long handleFilter, boolean isOp) {
		UUID requesterUUID = requester.getUUID();
		long now = System.currentTimeMillis();
		List<String> lines = new ArrayList<>();
		lines.add("=== Shared window ACL (policy=" + manager.currentPolicy()
			+ ", showing rows relevant to you) ===");
		int rowCount = 0;
		for (SharedWindowEntry entry : manager.getAllWindows()) {
			if (handleFilter != 0 && entry.getWindowHandle() != handleFilter) continue;
			boolean owns = entry.getOwnerUUID().equals(requesterUUID);
			boolean anyRow = false;
			StringBuilder sb = new StringBuilder();
			sb.append("0x").append(Long.toHexString(entry.getWindowHandle()))
				.append(" \"").append(entry.getWindowTitle()).append("\" owner=")
				.append(resolveName(requester, entry.getOwnerUUID()))
				.append(owns ? " [you]" : "").append(owns ? " (CONTROL always)" : "");
			for (Map.Entry<UUID, WindowGrant> e : entry.grantsSnapshot().entrySet()) {
				if (!aclRowVisible(isOp, owns, e.getKey().equals(requesterUUID))) continue;
				if (e.getKey().equals(entry.getOwnerUUID())) continue; // owner 恒全权已在行首
				anyRow = true;
				sb.append("\n  ").append(resolveName(requester, e.getKey()));
				if (e.getKey().equals(requesterUUID)) sb.append(" [you]");
				sb.append(": ").append(ShareAcl.describeGrant(e.getValue(), now));
			}
			if (owns || anyRow || isOp) {
				if (!anyRow && !owns) sb.append("\n  (no rows visible to you)");
				lines.add(sb.toString());
				rowCount++;
			}
		}
		if (rowCount == 0) lines.add("  (no shared windows)");
		// 全局表摘要（op 才有意义，非 op 只读无害）
		return lines;
	}

	// ===== ACL-2：邀请流程 =====

	/**
	 * 发送邀请请求给各窗口 owner（服务端线程）。
	 * - 同 (窗口, 观众) 冷却 INVITE_COOLDOWN_MS 内不重复发（防 join/register 风暴刷提示）；
	 * - 单批最多 MAX_INVITES_PER_BATCH 条（超出计审计后静默丢弃，owner 可手动 grant）；
	 * - owner 旧客户端未注册 payload（canSend=false）→ 跳过并计数：观众保持
	 *   无授权（fail-closed），不会出现"旧客户端静默放行"。
	 */
	static void sendInviteRequests(List<SharedWindowManager.InviteRequest> invites) {
		if (invites.isEmpty()) return;
		var server = WaylandCraftCommon.instance.server;
		if (server == null) return;
		long now = System.currentTimeMillis();
		int sent = 0;
		for (SharedWindowManager.InviteRequest req : invites) {
			String key = req.windowHandle() + ":" + req.requesterUUID();
			Long last = inviteCooldowns.get(key);
			if (last != null && now - last < INVITE_COOLDOWN_MS) continue;
			if (sent >= MAX_INVITES_PER_BATCH) {
				LOGGER.info("Invite batch cap {} reached, remaining prompts skipped (grant manually via /wl share grant)",
					MAX_INVITES_PER_BATCH);
				break;
			}
			ServerPlayer owner = server.getPlayerList().getPlayer(req.ownerUUID());
			ServerPlayer viewer = server.getPlayerList().getPlayer(req.requesterUUID());
			if (owner == null || viewer == null) continue; // owner 离线 = 无人可批（fail-closed）
			if (!ServerPlayNetworking.canSend(owner, ShareInviteRequestPayload.TYPE)) {
				if (ServerWarnThrottle.allow("invite-nocap:" + req.ownerUUID(), 10_000)) {
					LOGGER.info("Invite to owner {} skipped: client lacks share_invite_request receiver (viewer stays unauthorized)",
						req.ownerUUID());
				}
				continue;
			}
			inviteCooldowns.put(key, now);
			ServerPlayNetworking.send(owner, new ShareInviteRequestPayload(
				req.windowHandle(), req.requesterUUID(), viewer.getName().getString()));
			ShareAclAudit.inviteSent.incrementAndGet();
			ShareAclAudit.log("INVITE_SEND", req.windowHandle(), req.requesterUUID(), req.ownerUUID(),
				WindowPermission.VIEW, null, WindowGrant.NEVER_EXPIRES);
			sent++;
		}
	}

	/**
	 * 处理 owner 对邀请的应答（C2S share_invite_response，服务端线程）。
	 * 鉴权：窗口必须存在且发送者仍是当前 owner（ShareAcl.canRespondToInvite）——
	 * 窗口已注销或 handle 已换主时旧应答作废（防"邀请后窗口转手仍授权"竞态）。
	 */
	public static void handleShareInviteResponse(ShareInviteResponsePayload payload, ServerPlayer player) {
		UUID ownerUUID = player.getUUID();
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		SharedWindowEntry entry = manager.getWindow(payload.windowHandle());

		if (!ShareAcl.canRespondToInvite(entry != null, entry != null && ShareAcl.canManageWindow(entry.getOwnerUUID(), ownerUUID))) {
			if (ServerWarnThrottle.allow("invite-resp:" + ownerUUID, 5000)) {
				LOGGER.warn("Player {} invite response denied for window 0x{} (not owner or window gone)",
					ownerUUID, Long.toHexString(payload.windowHandle()));
			}
			ShareAclAudit.logDeny("invite-response", payload.windowHandle(), ownerUUID);
			ShareAclAudit.deniedInviteResponse.incrementAndGet();
			sendPermResponse(player, "Invite response rejected: window gone or you are not the owner");
			return;
		}
		if (payload.requesterUUID().equals(ownerUUID)) {
			sendPermResponse(player, "Cannot answer invite for your own window");
			return;
		}

		long now = System.currentTimeMillis();
		if (payload.decision() == ShareAcl.INVITE_DENY) {
			// 写吊销墓碑：压过后续默认策略回放，避免同一人反复触发 ASK 提示
			entry.revoke(payload.requesterUUID(), ownerUUID, now);
			manager.unsubscribePlayer(payload.requesterUUID(), payload.windowHandle());
			notifyPermissionChange(payload.windowHandle(), payload.requesterUUID(), WindowPermission.NONE);
			ShareAclAudit.log("REVOKE", payload.windowHandle(), payload.requesterUUID(), ownerUUID,
				WindowPermission.NONE, WindowGrant.GrantSource.INVITE, WindowGrant.NEVER_EXPIRES);
			ShareAclAudit.revoked.incrementAndGet();
			ShareAclAudit.inviteDenied.incrementAndGet();
			sendPermResponse(player, "Invitation declined");
			requestWindowListBroadcast();
			return;
		}

		WindowPermission level = ShareAcl.clampGrantedLevel(WindowPermission.fromLevel(payload.permissionLevel()));
		long durationSeconds = payload.decision() == ShareAcl.INVITE_ALLOW_TIMED
			? Math.max(1, Math.min(ShareInviteResponsePayload.MAX_DURATION_SECONDS, payload.durationSeconds()))
			: 0;
		long expiresAt = durationSeconds > 0 ? now + durationSeconds * 1000L : WindowGrant.NEVER_EXPIRES;
		WindowGrant g = manager.grantTimed(payload.windowHandle(), payload.requesterUUID(), level,
			ownerUUID, expiresAt, WindowGrant.GrantSource.INVITE, now);
		if (g == null) {
			sendPermResponse(player, "Invite grant failed (owner check)");
			return;
		}
		ShareAclAudit.log("GRANT", payload.windowHandle(), payload.requesterUUID(), ownerUUID,
			level, WindowGrant.GrantSource.INVITE, expiresAt);
		ShareAclAudit.countGrant(WindowGrant.GrantSource.INVITE);
		ShareAclAudit.inviteAccepted.incrementAndGet();
		sendPermResponse(player, "Granted " + level.name() + (expiresAt == WindowGrant.NEVER_EXPIRES ? " (always)" : " for " + durationSeconds + "s")
			+ " to invite requester");
		notifyPermissionChange(payload.windowHandle(), payload.requesterUUID(), level);
		requestWindowListBroadcast();
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
	 * ACL-4：向指定在线玩家即时发"权限移除"信号（窗口注销/owner 退出场景，
	 * 观众端据此销毁显示与音频，对齐 removeWindow 路径）。服务端线程调用。
	 */
	public static void notifyPermissionRemoved(long windowHandle, ServerPlayer viewer) {
		if (ServerPlayNetworking.canSend(viewer, SharedWindowPermissionPayload.TYPE)) {
			ServerPlayNetworking.send(viewer, new SharedWindowPermissionPayload(
				windowHandle, viewer.getUUID(), WindowPermission.NONE));
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
		long nowMs = System.currentTimeMillis();
		List<SharedWindowListPayload.WindowInfo> visible = new ArrayList<>();

		for(SharedWindowEntry entry : manager.getAllWindows()) {
			// ACL-3：列表广播走合成判定（owner 恒 CONTROL、黑白名单压过记录、过期即隐）
			WindowPermission perm = manager.effectivePermission(entry, playerUUID, nowMs);
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
		SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
		long nowMs = System.currentTimeMillis();
		for(ServerPlayer player : server.getPlayerList().getPlayers()) {
			UUID playerUUID = player.getUUID();
			// ACL-3：state 广播同样走合成判定（吊销/过期即时生效）
			if(manager.effectivePermission(entry, playerUUID, nowMs).hasPermission(WindowPermission.VIEW)
				&& ServerPlayNetworking.canSend(player, SharedWindowUpdatePayload.TYPE)) {
				ServerPlayNetworking.send(player, payload);
			}
		}
		LOGGER.debug("Broadcasting state update for window 0x{}", Long.toHexString(entry.getWindowHandle()));
	}
}
