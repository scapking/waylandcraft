package dev.evvie.waylandcraft.shared;

import java.util.Collection;
import java.util.Collections;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

import org.jetbrains.annotations.Nullable;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.server.level.ServerPlayer;

public class SharedWindowManager {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-shared");

	private final Map<Long, SharedWindowEntry> windowRegistry = new ConcurrentHashMap<>();
	private final Map<UUID, ConcurrentHashMap.KeySetView<Long, Boolean>> playerSubscriptions = new ConcurrentHashMap<>();

	public SharedWindowManager() {
		LOGGER.info("SharedWindowManager initialized");
	}

	public SharedWindowEntry registerWindow(long windowHandle, UUID ownerUUID, String windowTitle) {
		return registerWindow(windowHandle, ownerUUID, windowTitle, null);
	}

	/**
	 * 注册共享窗口。
	 *
	 * @param targetPlayerUUID 定向共享目标（null = 公开共享给所有人）
	 */
	public SharedWindowEntry registerWindow(long windowHandle, UUID ownerUUID, String windowTitle, @Nullable UUID targetPlayerUUID) {
		SharedWindowEntry entry = new SharedWindowEntry(windowHandle, ownerUUID, windowTitle, targetPlayerUUID);
		windowRegistry.put(windowHandle, entry);

		subscribePlayer(ownerUUID, windowHandle);
		if (targetPlayerUUID != null && !targetPlayerUUID.equals(ownerUUID)) {
			subscribePlayer(targetPlayerUUID, windowHandle);
		}

		LOGGER.info("Window registered: 0x{} by {} (targeted={})",
			Long.toHexString(windowHandle), ownerUUID, targetPlayerUUID != null);
		return entry;
	}

	public void unregisterWindow(long windowHandle) {
		SharedWindowEntry entry = windowRegistry.remove(windowHandle);
		if (entry != null) {
			playerSubscriptions.values().forEach(set -> set.remove(windowHandle));
			LOGGER.info("Window unregistered: 0x{}", Long.toHexString(windowHandle));
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

		if (!entry.hasPermission(playerUUID, WindowPermission.VIEW)) {
			LOGGER.warn("Player {} denied subscription to window 0x{}", playerUUID, Long.toHexString(windowHandle));
			return false;
		}

		playerSubscriptions.computeIfAbsent(playerUUID, k -> ConcurrentHashMap.newKeySet()).add(windowHandle);
		LOGGER.info("Player {} subscribed to window 0x{}", playerUUID, Long.toHexString(windowHandle));
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

	public void handlePlayerDisconnect(UUID playerUUID) {
		playerSubscriptions.remove(playerUUID);

		windowRegistry.entrySet().removeIf(entry -> {
			if (entry.getValue().getOwnerUUID().equals(playerUUID)) {
				LOGGER.info("Owner disconnected, unregistering window 0x{}", Long.toHexString(entry.getKey()));
				return true;
			}
			return false;
		});
	}

	public boolean updatePermission(long windowHandle, UUID targetUUID, WindowPermission permission, UUID requesterUUID) {
		SharedWindowEntry entry = windowRegistry.get(windowHandle);
		if (entry == null) return false;

		// owner 权威：只有窗口所有者能改权限（管理员/其他 CONTROL 持有者一律拒绝）。
		// 原实现按 hasPermission(CONTROL) 判断，任何被授予 CONTROL 的玩家都能越权改权限，
		// 违背"谁共享谁管理"的安全模型。
		if (!entry.getOwnerUUID().equals(requesterUUID)) {
			LOGGER.warn("Player {} denied permission update for window 0x{} (not owner)", requesterUUID, Long.toHexString(windowHandle));
			return false;
		}

		entry.setPermission(targetUUID, permission);
		LOGGER.info("Permission updated for player {} on window 0x{}: {}", targetUUID, Long.toHexString(windowHandle), permission);

		if (permission == WindowPermission.NONE) {
			unsubscribePlayer(targetUUID, windowHandle);
		} else {
			subscribePlayer(targetUUID, windowHandle);
		}

		return true;
	}

	public boolean updateWindowState(long windowHandle, int x, int y, int width, int height, boolean visible, UUID requesterUUID) {
		SharedWindowEntry entry = windowRegistry.get(windowHandle);
		if (entry == null) return false;

		if (!entry.hasPermission(requesterUUID, WindowPermission.CONTROL)) {
			return false;
		}

		entry.updatePosition(x, y);
		entry.updateSize(width, height);
		entry.setVisible(visible);

		return true;
	}

	public boolean canInteract(UUID playerUUID, long windowHandle) {
		SharedWindowEntry entry = windowRegistry.get(windowHandle);
		if (entry == null) return false;
		return entry.hasPermission(playerUUID, WindowPermission.INTERACT);
	}

	/**
	 * 给新加入的玩家授予"公开共享"窗口的 VIEW 权限。
	 * 定向共享的窗口（targetPlayerUUID != null）不自动授权 —— 只有被 owner 指定的玩家可见。
	 */
	public void grantViewToNewPlayer(UUID playerUUID) {
		int granted = 0;
		for (SharedWindowEntry entry : windowRegistry.values()) {
			if (entry.isTargeted()) continue; // 定向共享：不自动授权
			if (!entry.getOwnerUUID().equals(playerUUID)) {
				entry.setPermission(playerUUID, WindowPermission.VIEW);
				granted++;
			}
		}
		LOGGER.info("Granted VIEW permission to new player {} for {} public windows", playerUUID, granted);
	}

	public int getWindowCount() {
		return windowRegistry.size();
	}

	public void clear() {
		windowRegistry.clear();
		playerSubscriptions.clear();
		LOGGER.info("SharedWindowManager cleared");
	}
}
