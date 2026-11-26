package dev.evvie.waylandcraft.shared;

import java.util.ArrayList;
import java.util.Collection;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

import org.jetbrains.annotations.Nullable;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.server.level.ServerPlayer;

/**
 * 共享窗口注册表 + 授权生命周期（ACL 加固后）。
 *
 * 变更点（feat/share-permission-hardening）：
 * - ACL-1 授权记录化：per-window 表存 WindowGrant（级别/授出人/到期/来源），
 *   setPermission 旧 API 保留为 MANUAL 永久授权/吊销墓碑的兼容入口。
 * - ACL-2 join/register 收口：grantViewToNewPlayer 的无条件放行替换为
 *   applyDefaultPolicy（allow=物化 DEFAULT 记录；ask=收集邀请请求交网络层；
 *   deny=不授权）。默认策略 ALLOW 保持既有行为兼容。
 * - ACL-3 权威合成：所有判定经 effectivePermission → ShareAcl.compose
 *   （owner 恒 CONTROL > 黑名单 > 白名单 > 记录）。
 * - ACL-1 过期扫描：expireGrants(now) 由 server tick 调用（纯扫描，不发消息），
 *   返回需要通知的到期事件，网络层负责传播。
 * - ACL-6 handle 重注册：同 owner 重建窗口 = 全新 entry（旧 ACL/订阅强制清空），
 *   不同 owner 抢占 = 拒绝。杜绝"窗口销毁重建 handle 复用 → 权限残留"。
 */
public class SharedWindowManager {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-shared");

	private final Map<Long, SharedWindowEntry> windowRegistry = new ConcurrentHashMap<>();
	private final Map<UUID, ConcurrentHashMap.KeySetView<Long, Boolean>> playerSubscriptions = new ConcurrentHashMap<>();

	/** ACL-3：权威合成判定需要的黑白名单/default 表。单人环境可为 null（表视为空）。 */
	@Nullable
	private final PermissionManager permissionManager;

	public SharedWindowManager() {
		this(null);
	}

	public SharedWindowManager(@Nullable PermissionManager permissionManager) {
		this.permissionManager = permissionManager;
		LOGGER.info("SharedWindowManager initialized (permission tables {})",
			permissionManager != null ? "bound" : "absent");
	}

	public SharedWindowEntry registerWindow(long windowHandle, UUID ownerUUID, String windowTitle) {
		return registerWindow(windowHandle, ownerUUID, windowTitle, null);
	}

	/**
	 * 注册共享窗口（公开共享时按新观众策略物化在线观众的 DEFAULT 授权）。
	 *
	 * @param targetPlayerUUID 定向共享目标（null = 公开共享）
	 */
	public SharedWindowEntry registerWindow(long windowHandle, UUID ownerUUID, String windowTitle, @Nullable UUID targetPlayerUUID) {
		SharedWindowEntry entry = new SharedWindowEntry(windowHandle, ownerUUID, windowTitle, targetPlayerUUID);
		windowRegistry.put(windowHandle, entry);

		subscribePlayer(ownerUUID, windowHandle);
		if (targetPlayerUUID != null && !targetPlayerUUID.equals(ownerUUID)) {
			subscribePlayer(targetPlayerUUID, windowHandle);
		}

		LOGGER.debug("Window registered: 0x{} by {} (targeted={})",
			Long.toHexString(windowHandle), ownerUUID, targetPlayerUUID != null);
		return entry;
	}

	/**
	 * ACL-2：注册公开窗口后对当前在线观众执行默认策略。
	 * 返回需要网络层发送的邀请列表（ASK）；ALLOW/DENY 返回空。
	 * 与 onPlayerJoin 分开暴露——网络副作用（发消息）留在 handler 层。
	 */
	public List<InviteRequest> applyPolicyForAudience(SharedWindowEntry entry) {
		return applyDefaultPolicy(entry, onlinePlayerUUIDs());
	}

	/**
	 * ACL-6：同 owner 的 handle 重注册（窗口销毁重建、xid 复用）——
	 * 必须彻底清掉旧 entry 的授权与订阅再替换，否则：
	 * - 旧观众自动看到"新窗口"的内容（越权泄露）；
	 * - 旧墓碑错误屏蔽新窗口的合法默认授权。
	 * 返回被替换的旧 entry（null = 全新注册）。调用方（服务端 handler）
	 * 负责 relay 缓存清理与列表广播。
	 */
	@Nullable
	public SharedWindowEntry replaceWindowForSameOwner(long windowHandle, UUID ownerUUID, String windowTitle,
			@Nullable UUID targetPlayerUUID) {
		SharedWindowEntry old = windowRegistry.get(windowHandle);
		if (old != null) {
			// 即时清转发缓存（同 unregister 路径语义）
			playerSubscriptions.values().forEach(set -> set.remove(windowHandle));
		}
		registerWindow(windowHandle, ownerUUID, windowTitle, targetPlayerUUID);
		return old;
	}

	public void unregisterWindow(long windowHandle) {
		SharedWindowEntry entry = windowRegistry.remove(windowHandle);
		if (entry != null) {
			playerSubscriptions.values().forEach(set -> set.remove(windowHandle));
			LOGGER.debug("Window unregistered: 0x{}", Long.toHexString(windowHandle));
		}
	}

	@Nullable
	public SharedWindowEntry getWindow(long windowHandle) {
		return windowRegistry.get(windowHandle);
	}

	public Collection<SharedWindowEntry> getAllWindows() {
		return Collections.unmodifiableCollection(windowRegistry.values());
	}

	public Collection<Long> getPlayerSubscriptions(UUID playerUUID) {
		return playerSubscriptions.getOrDefault(playerUUID, ConcurrentHashMap.newKeySet());
	}

	public boolean subscribePlayer(UUID playerUUID, long windowHandle) {
		SharedWindowEntry entry = windowRegistry.get(windowHandle);
		if (entry == null) return false;

		if (!effectivePermission(entry, playerUUID, System.currentTimeMillis()).hasPermission(WindowPermission.VIEW)) {
			LOGGER.warn("Player {} denied subscription to window 0x{}", playerUUID, Long.toHexString(windowHandle));
			return false;
		}

		playerSubscriptions.computeIfAbsent(playerUUID, k -> ConcurrentHashMap.newKeySet()).add(windowHandle);
		LOGGER.debug("Player {} subscribed to window 0x{}", playerUUID, Long.toHexString(windowHandle));
		return true;
	}

	public void unsubscribePlayer(UUID playerUUID, long windowHandle) {
		ConcurrentHashMap.KeySetView<Long, Boolean> set = playerSubscriptions.get(playerUUID);
		if (set != null) {
			set.remove(windowHandle);
			if (set.isEmpty()) {
				playerSubscriptions.remove(playerUUID);
			}
		}
	}

	/**
	 * 玩家断连：清理订阅 + 摘除其名下窗口。
	 * ACL-4：返回其名下（因 owner 退出而消失）的窗口句柄，调用方（网络层）
	 * 立即清 relay 缓存并通知观众，不等列表广播/tick 巡检。
	 */
	public List<Long> handlePlayerDisconnect(UUID playerUUID) {
		playerSubscriptions.remove(playerUUID);

		List<Long> removed = new ArrayList<>();
		windowRegistry.entrySet().removeIf(entry -> {
			if (entry.getValue().getOwnerUUID().equals(playerUUID)) {
				removed.add(entry.getKey());
				return true;
			}
			return false;
		});
		if (!removed.isEmpty()) {
			LOGGER.info("Owner disconnected, unregistering {} window(s)", removed.size());
		}
		return removed;
	}

	/**
	 * 权限更新（owner 权威）。permission == NONE 写吊销墓碑；
	 * 成功时返回写入的记录（供审计/通知），owner 自我改写与越权请求返回 null。
	 */
	@Nullable
	public SharedWindowEntry updatePermissionAndGetEntry(long windowHandle, UUID targetUUID, WindowPermission permission, UUID requesterUUID) {
		SharedWindowEntry entry = windowRegistry.get(windowHandle);
		if (entry == null) return null;

		// owner 权威：只有窗口所有者能改权限（管理员/其他 CONTROL 持有者一律拒绝）。
		if (!ShareAcl.canManageWindow(entry.getOwnerUUID(), requesterUUID)) {
			LOGGER.warn("Player {} denied permission update for window 0x{} (not owner)", requesterUUID, Long.toHexString(windowHandle));
			return null;
		}
		if (targetUUID.equals(entry.getOwnerUUID())) {
			return null; // owner 恒全权，不可自我改写
		}

		long now = System.currentTimeMillis();
		boolean ok = permission == WindowPermission.NONE
			? entry.revoke(targetUUID, requesterUUID, now)
			: entry.grant(targetUUID, permission, requesterUUID, WindowGrant.NEVER_EXPIRES, now, WindowGrant.GrantSource.MANUAL);
		if (!ok) return null;

		LOGGER.info("Permission updated for player {} on window 0x{}: {}", targetUUID, Long.toHexString(windowHandle), permission);

		if (permission == WindowPermission.NONE) {
			unsubscribePlayer(targetUUID, windowHandle);
		} else {
			subscribePlayer(targetUUID, windowHandle);
		}
		return entry;
	}

	/** 兼容旧签名的布尔入口（grant/revoke 走 updatePermissionAndGetEntry）。 */
	public boolean updatePermission(long windowHandle, UUID targetUUID, WindowPermission permission, UUID requesterUUID) {
		return updatePermissionAndGetEntry(windowHandle, targetUUID, permission, requesterUUID) != null;
	}

	/**
	 * ACL-1：限时授权写入口（grant <handle> <player> <level> [时长]）。
	 * 调用方已完成 owner 鉴权（handleWindowPermCommand）。
	 *
	 * @param expiresAtMs 绝对到期时刻；0 = 永久
	 * @return 写入的记录；owner 自我改写/窗口不存在返回 null
	 */
	@Nullable
	public WindowGrant grantTimed(long windowHandle, UUID targetUUID, WindowPermission level,
			UUID requesterUUID, long expiresAtMs, WindowGrant.GrantSource source, long nowMs) {
		SharedWindowEntry entry = windowRegistry.get(windowHandle);
		if (entry == null) return null;
		// ACL-6：伪造 grant 包在 owner 权威门前被拒（鉴权走 ShareAcl 纯函数，可单测锁定）
		if (!ShareAcl.canManageWindow(entry.getOwnerUUID(), requesterUUID) || targetUUID.equals(entry.getOwnerUUID())) {
			// 审计行走 5s/玩家节流（伪造 grant 包可高频重放，不能每包一条 INFO）；计数不节流
			if (dev.evvie.waylandcraft.utils.ServerWarnThrottle.allow("acl-grant-deny:" + requesterUUID, 5000)) {
				LOGGER.warn("Player {} denied timed grant on window 0x{} (not owner or targets owner)",
					requesterUUID, Long.toHexString(windowHandle));
				ShareAclAudit.logDeny("grant-not-owner", windowHandle, requesterUUID);
			}
			ShareAclAudit.deniedNotOwner.incrementAndGet();
			return null;
		}
		if (!entry.grant(targetUUID, level, requesterUUID, expiresAtMs, nowMs, source)) {
			return null;
		}
		subscribePlayer(targetUUID, windowHandle);
		return entry.getGrant(targetUUID);
	}

	public boolean updateWindowState(long windowHandle, int x, int y, int width, int height, boolean visible, UUID requesterUUID) {
		SharedWindowEntry entry = windowRegistry.get(windowHandle);
		if (entry == null) return false;

		if (!effectivePermission(entry, requesterUUID, System.currentTimeMillis()).hasPermission(WindowPermission.CONTROL)) {
			return false;
		}

		entry.updatePosition(x, y);
		entry.updateSize(width, height);
		entry.setVisible(visible);

		return true;
	}

	/** ACL-3：交互判定走合成链（黑名单/过期即刻生效，修复"记录未清但已吊销"竞态）。 */
	public boolean canInteract(UUID playerUUID, long windowHandle) {
		SharedWindowEntry entry = windowRegistry.get(windowHandle);
		if (entry == null) return false;
		return effectivePermission(entry, playerUUID, System.currentTimeMillis()).hasPermission(WindowPermission.INTERACT);
	}

	/**
	 * ACL-3：权威合成判定（转发/交互/列表广播/state 统一入口）。
	 * pm 未绑定时黑白名单视为空——owner/记录层语义不变。
	 */
	public WindowPermission effectivePermission(SharedWindowEntry entry, UUID playerUUID, long nowMs) {
		boolean isOwner = entry.getOwnerUUID().equals(playerUUID);
		boolean blacklisted = permissionManager != null && permissionManager.isBlacklisted(playerUUID);
		WindowPermission whitelist = permissionManager != null ? permissionManager.whitelistLevelOf(playerUUID) : null;
		WindowGrant record = entry.getGrant(playerUUID);
		boolean expired = record != null && record.isExpired(nowMs);
		return ShareAcl.compose(isOwner, blacklisted, whitelist, record, expired, entry.isTargeted(), nowMs);
	}

	/** 便捷重载：按句柄取 entry 并合成；窗口不存在返回 NONE。 */
	public WindowPermission effectivePermission(long windowHandle, UUID playerUUID, long nowMs) {
		SharedWindowEntry entry = windowRegistry.get(windowHandle);
		if (entry == null) return WindowPermission.NONE;
		return effectivePermission(entry, playerUUID, nowMs);
	}

	// ===== ACL-2：新观众默认策略 =====

	/**
	 * 玩家加入：按策略处理所有公开（非定向）窗口。
	 * - ALLOW：为每个窗口物化 DEFAULT 记录（级别 = PermissionManager.defaultPermission）。
	 * - ASK：不授权，返回需要向各 owner 发送的邀请目标列表（交网络层发送）。
	 * - DENY：什么都不做（观众对该窗口无记录 = 不可见）。
	 * 定向共享窗口任何策略下都不自动授权（owner 显式指定语义）。
	 */
	public List<InviteRequest> onPlayerJoin(UUID playerUUID) {
		List<InviteRequest> invites = new ArrayList<>();
		ShareAcl.NewViewerPolicy policy = currentPolicy();
		long now = System.currentTimeMillis();
		int affected = 0;
		for (SharedWindowEntry entry : windowRegistry.values()) {
			if (entry.isTargeted()) continue; // 定向共享：不自动授权
			if (entry.getOwnerUUID().equals(playerUUID)) continue;
			switch (policy) {
				case ALLOW -> {
					if (applyDefaultTo(entry, playerUUID, now)) affected++;
				}
				case ASK -> {
					if (entry.getGrant(playerUUID) == null) {
						invites.add(new InviteRequest(entry.getWindowHandle(), entry.getOwnerUUID(), playerUUID));
					}
				}
				case DENY -> { }
			}
		}
		if (policy == ShareAcl.NewViewerPolicy.ALLOW) {
			LOGGER.info("New player {}: default policy ALLOW applied to {} public windows", playerUUID, affected);
		} else {
			LOGGER.info("New player {}: default policy {} ({} invite prompt(s))", playerUUID, policy, invites.size());
		}
		return invites;
	}

	/**
	 * 注册公开窗口时对全体在线观众执行策略（ALLOW 物化 / ASK 返回邀请列表）。
	 * 旧 grantViewToNewPlayer 的替代入口，保留同名旧接口见下。
	 */
	public List<InviteRequest> applyDefaultPolicy(SharedWindowEntry entry, List<UUID> audience) {
		List<InviteRequest> invites = new ArrayList<>();
		if (entry.isTargeted()) return invites;
		ShareAcl.NewViewerPolicy policy = currentPolicy();
		if (policy == ShareAcl.NewViewerPolicy.DENY) return invites;
		long now = System.currentTimeMillis();
		WindowPermission defaultLevel = permissionManager != null ? permissionManager.getDefaultPermission() : WindowPermission.VIEW;
		for (UUID uuid : audience) {
			if (uuid.equals(entry.getOwnerUUID())) continue;
			if (policy == ShareAcl.NewViewerPolicy.ALLOW) {
				// 已有显式记录（含墓碑）不覆盖——owner 意志优先于默认回放
				WindowGrant existing = entry.getGrant(uuid);
				if (existing != null && !existing.isExpired(now)) continue;
				if (defaultLevel == WindowPermission.NONE) continue;
				if (entry.grant(uuid, defaultLevel, null, WindowGrant.NEVER_EXPIRES, now, WindowGrant.GrantSource.DEFAULT)) {
					ShareAclAudit.log("GRANT", entry.getWindowHandle(), uuid, null, defaultLevel, WindowGrant.GrantSource.DEFAULT, WindowGrant.NEVER_EXPIRES);
					ShareAclAudit.countGrant(WindowGrant.GrantSource.DEFAULT);
				}
			} else { // ASK
				if (entry.getGrant(uuid) == null) {
					invites.add(new InviteRequest(entry.getWindowHandle(), entry.getOwnerUUID(), uuid));
				}
			}
		}
		return invites;
	}

	private boolean applyDefaultTo(SharedWindowEntry entry, UUID playerUUID, long now) {
		WindowPermission defaultLevel = permissionManager != null ? permissionManager.getDefaultPermission() : WindowPermission.VIEW;
		if (defaultLevel == WindowPermission.NONE) return false;
		// 黑名单玩家不物化 DEFAULT 记录（表被消费的体现；compose 也会拒，双保险不留垃圾记录）
		if (permissionManager != null && permissionManager.isBlacklisted(playerUUID)) return false;
		if (entry.getOwnerUUID().equals(playerUUID)) return false;
		WindowGrant existing = entry.getGrant(playerUUID);
		if (existing != null && !existing.isExpired(now)) return false; // 显式记录（含墓碑）不被默认回放覆盖
		boolean ok = entry.grant(playerUUID, defaultLevel, null, WindowGrant.NEVER_EXPIRES, now, WindowGrant.GrantSource.DEFAULT);
		if (ok) {
			ShareAclAudit.log("GRANT", entry.getWindowHandle(), playerUUID, null, defaultLevel, WindowGrant.GrantSource.DEFAULT, WindowGrant.NEVER_EXPIRES);
			ShareAclAudit.countGrant(WindowGrant.GrantSource.DEFAULT);
		}
		return ok;
	}

	public ShareAcl.NewViewerPolicy currentPolicy() {
		return permissionManager != null ? permissionManager.getNewViewerPolicy() : ShareAcl.NewViewerPolicy.ALLOW;
	}

	/** 策略切回 ALLOW 时对全体公开窗口×在线玩家回放一次补授（显式记录/墓碑不动）。 */
	public void reapplyAllowPolicyAll() {
		List<UUID> audience = onlinePlayerUUIDs();
		for (SharedWindowEntry entry : windowRegistry.values()) {
			if (!entry.isTargeted()) applyDefaultPolicy(entry, audience);
		}
	}

	/**
	 * @deprecated ACL-2：使用 {@link #onPlayerJoin(UUID)}（受默认策略控制）。
	 * 保留旧入口以兼容既有调用点：ALLOW 语义与原"无条件放行"一致。
	 */
	@Deprecated
	public void grantViewToNewPlayer(UUID playerUUID) {
		onPlayerJoin(playerUUID);
	}

	// ===== ACL-1：过期扫描 =====

	/**
	 * 到期授权事件（服务端 tick 扫描产物；网络层据此通知观众+清缓存）。
	 */
	public record GrantExpired(long windowHandle, UUID playerUUID, WindowGrant grant) {}

	/**
	 * 扫描并回收所有到期记录（纯服务端线程调用；时钟注入可单测）。
	 * 过期的 MANUAL/INVITE 记录被摘除后，若公开窗口策略为 ALLOW 会重新物化
	 * DEFAULT 记录（"限时授权到期后回落到默认策略"语义）；定向窗口回 NONE。
	 */
	public List<GrantExpired> expireGrants(long nowMs) {
		List<GrantExpired> expired = new ArrayList<>();
		for (SharedWindowEntry entry : windowRegistry.values()) {
			for (Map.Entry<UUID, WindowGrant> e : entry.grantsSnapshot().entrySet()) {
				WindowGrant g = e.getValue();
				if (!g.isExpired(nowMs)) continue;
				// remove(key,value) 防与并发 grant 竞争
				ConcurrentHashMap<UUID, WindowGrant> map = entry.grantsForSweep();
				if (!map.remove(e.getKey(), g)) continue;
				expired.add(new GrantExpired(entry.getWindowHandle(), e.getKey(), g));
				unsubscribePlayer(e.getKey(), entry.getWindowHandle());
				ShareAclAudit.log("EXPIRE", entry.getWindowHandle(), e.getKey(), g.grantedBy(), g.level(), g.source(), g.expiresAtMs());
				ShareAclAudit.expired.incrementAndGet();
				// 回落默认策略
				if (ShareAcl.shouldReapplyDefault(entry.isTargeted(), currentPolicy(),
						permissionManager != null && permissionManager.isBlacklisted(e.getKey()),
						entry.getOwnerUUID().equals(e.getKey()))) {
					if (entry.getGrant(e.getKey()) == null) {
						applyDefaultTo(entry, e.getKey(), nowMs);
					}
				}
			}
		}
		return expired;
	}

	// ===== 杂项 =====

	@Nullable
	public PermissionManager getPermissionManager() {
		return permissionManager;
	}

	private List<UUID> onlinePlayerUUIDs() {
		List<UUID> ids = new ArrayList<>();
		var server = WaylandCraftCommon.instance != null ? WaylandCraftCommon.instance.server : null;
		if (server != null) {
			for (ServerPlayer player : server.getPlayerList().getPlayers()) {
				ids.add(player.getUUID());
			}
		}
		return ids;
	}

	public int getWindowCount() {
		return windowRegistry.size();
	}

	public void clear() {
		windowRegistry.clear();
		playerSubscriptions.clear();
		LOGGER.info("SharedWindowManager cleared");
	}

	/** ACL-2 邀请目标三元组（窗口, owner, 请求观众）。 */
	public record InviteRequest(long windowHandle, UUID ownerUUID, UUID requesterUUID) {}
}
