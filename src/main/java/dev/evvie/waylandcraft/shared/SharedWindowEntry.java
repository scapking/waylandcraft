package dev.evvie.waylandcraft.shared;

import java.util.HashMap;
import java.util.Map;
import java.util.UUID;

import org.jetbrains.annotations.Nullable;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.server.level.ServerPlayer;

/**
 * 单个共享窗口的注册条目 + 逐玩家权限表。
 *
 * 共享模式（owner 权威）：
 * - 定向共享（targetPlayerUUID != null）：只给 owner 发 CONTROL、给目标玩家发 VIEW，
 *   其余玩家 NONE（完全不可见，防 handle/title 元数据泄露）。
 * - 公开共享（targetPlayerUUID == null）：给所有在线玩家发 VIEW。
 */
public class SharedWindowEntry {

	private final long windowHandle;
	private final UUID ownerUUID;
	private final String windowTitle;
	private final long createdAt;

	/** 定向共享目标（null = 公开共享给所有人） */
	@Nullable
	private final UUID targetPlayerUUID;

	private final Map<UUID, WindowPermission> permissions = new HashMap<>();

	private int x, y;
	private int width, height;
	private boolean visible = true;

	public SharedWindowEntry(long windowHandle, UUID ownerUUID, String windowTitle) {
		this(windowHandle, ownerUUID, windowTitle, null);
	}

	public SharedWindowEntry(long windowHandle, UUID ownerUUID, String windowTitle, @Nullable UUID targetPlayerUUID) {
		this.windowHandle = windowHandle;
		this.ownerUUID = ownerUUID;
		this.windowTitle = windowTitle;
		this.createdAt = System.currentTimeMillis();
		this.targetPlayerUUID = targetPlayerUUID;

		this.permissions.put(ownerUUID, WindowPermission.CONTROL);

		// 定向共享：只给目标玩家 VIEW；公开共享：给所有在线玩家 VIEW
		if (targetPlayerUUID != null) {
			if (!targetPlayerUUID.equals(ownerUUID)) {
				this.permissions.put(targetPlayerUUID, WindowPermission.VIEW);
			}
		} else {
			var server = WaylandCraftCommon.instance.server;
			if (server != null) {
				for (ServerPlayer player : server.getPlayerList().getPlayers()) {
					UUID uuid = player.getUUID();
					if (!uuid.equals(ownerUUID)) {
						this.permissions.put(uuid, WindowPermission.VIEW);
					}
				}
			}
		}
	}

	public long getWindowHandle() {
		return windowHandle;
	}

	public UUID getOwnerUUID() {
		return ownerUUID;
	}

	public String getWindowTitle() {
		return windowTitle;
	}

	public long getCreatedAt() {
		return createdAt;
	}

	/** 是否定向共享（false = 公开共享给所有人） */
	public boolean isTargeted() {
		return targetPlayerUUID != null;
	}

	@Nullable
	public UUID getTargetPlayerUUID() {
		return targetPlayerUUID;
	}

	public void setPermission(UUID playerUUID, WindowPermission permission) {
		if (permission == WindowPermission.NONE) {
			permissions.remove(playerUUID);
		} else {
			permissions.put(playerUUID, permission);
		}
	}

	public WindowPermission getPermission(UUID playerUUID) {
		return permissions.getOrDefault(playerUUID, WindowPermission.NONE);
	}

	public boolean hasPermission(UUID playerUUID, WindowPermission required) {
		return getPermission(playerUUID).hasPermission(required);
	}

	public Map<UUID, WindowPermission> getAllPermissions() {
		return new HashMap<>(permissions);
	}

	public void updatePosition(int x, int y) {
		this.x = x;
		this.y = y;
	}

	public void updateSize(int width, int height) {
		this.width = width;
		this.height = height;
	}

	public void setVisible(boolean visible) {
		this.visible = visible;
	}

	public int getX() { return x; }
	public int getY() { return y; }
	public int getWidth() { return width; }
	public int getHeight() { return height; }
	public boolean isVisible() { return visible; }

	@Override
	public boolean equals(Object obj) {
		if (this == obj) return true;
		if (!(obj instanceof SharedWindowEntry)) return false;
		SharedWindowEntry other = (SharedWindowEntry) obj;
		return windowHandle == other.windowHandle;
	}

	@Override
	public int hashCode() {
		return Long.hashCode(windowHandle);
	}
}
