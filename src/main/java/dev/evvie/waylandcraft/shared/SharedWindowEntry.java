package dev.evvie.waylandcraft.shared;

import java.util.HashMap;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

import org.jetbrains.annotations.Nullable;

/**
 * 单个共享窗口的注册条目 + 逐玩家授权记录表（ACL-1：记录化改造）。
 *
 * 共享模式（owner 权威）：
 * - 定向共享（targetPlayerUUID != null）：只给 owner 记 CONTROL、给目标玩家记 VIEW，
 *   其余玩家无记录（不可见，防 handle/title 元数据泄露）。
 * - 公开共享（targetPlayerUUID == null）：在线观众的授权由
 *   SharedWindowManager 按新观众默认策略（allow/ask/deny）在注册/join 时物化，
 *   不再在构造器里无条件放行。
 *
 * 线程模型（P1-4 语义保留）：
 * - 帧/音频转发线程读（getPermission/hasPermission，含惰性过期检查），
 *   服务端线程写（grant/revoke/setPermission）——permissions 必须保持
 *   ConcurrentHashMap，裸 HashMap 在该模式下会丢条目甚至死循环。
 *
 * 判定收口：本类的 getPermission 是"记录快查"（转发热路径用）；
 * 权威判定必须走 SharedWindowManager.effectivePermission → ShareAcl.compose
 * （叠加 owner 恒 CONTROL / 黑白名单），二者对 owner 的结果一致。
 */
public class SharedWindowEntry {

	private final long windowHandle;
	private final UUID ownerUUID;
	private final String windowTitle;
	private final long createdAt;

	/** 定向共享目标（null = 公开共享给所有人） */
	@Nullable
	private final UUID targetPlayerUUID;

	/**
	 * ACL-1：player -> 授权记录（级别 + 来源 + 到期）。
	 * level=NONE 记录是显式吊销墓碑，压过默认策略；无记录 = 从未授权。
	 * 过期记录在读取时按"未授予"处理并惰性摘除（与 sweep 的竞态用
	 * remove(key, expected) 防御）。
	 */
	private final ConcurrentHashMap<UUID, WindowGrant> grants = new ConcurrentHashMap<>();

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
		long now = System.currentTimeMillis();
		this.createdAt = now;
		this.targetPlayerUUID = targetPlayerUUID;

		// owner 恒 CONTROL（永久、manual 来源、自授）
		this.grants.put(ownerUUID, WindowGrant.permanent(ownerUUID, WindowPermission.CONTROL, ownerUUID, now,
			WindowGrant.GrantSource.MANUAL));

		// 定向共享：只给目标玩家记 VIEW（owner 显式指定 = manual 来源）
		if (targetPlayerUUID != null && !targetPlayerUUID.equals(ownerUUID)) {
			this.grants.put(targetPlayerUUID, WindowGrant.permanent(targetPlayerUUID, WindowPermission.VIEW,
				ownerUUID, now, WindowGrant.GrantSource.MANUAL));
		}
		// 公开共享：不再在构造器遍历在线玩家无条件授权——由
		// SharedWindowManager.applyDefaultPolicy 按 NewViewerPolicy 处理（ACL-2），
		// 同时消除了构造器对 WaylandCraftCommon.instance 的隐式依赖。
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

	/**
	 * 旧 API 兼容写入口：等价于 MANUAL 来源的永久授权；NONE = 吊销墓碑。
	 * owner 自身拒绝改写（返回 false）——owner 恒全权，防自锁与提权记录混淆。
	 */
	public boolean setPermission(UUID playerUUID, WindowPermission permission) {
		if (playerUUID.equals(ownerUUID)) {
			return false;
		}
		long now = System.currentTimeMillis();
		if (permission == WindowPermission.NONE) {
			grants.put(playerUUID, WindowGrant.tombstone(playerUUID, null, now, WindowGrant.GrantSource.MANUAL));
		} else {
			grants.put(playerUUID, WindowGrant.permanent(playerUUID, permission, null, now, WindowGrant.GrantSource.MANUAL));
		}
		return true;
	}

	/**
	 * ACL-1 授权记录写入口（带来源/授出人/到期）。owner 拒绝改写。
	 *
	 * @param expiresAtMs 绝对到期时刻；{@link WindowGrant#NEVER_EXPIRES}=永久
	 */
	public boolean grant(UUID playerUUID, WindowPermission level, @Nullable UUID grantedBy,
			long expiresAtMs, long nowMs, WindowGrant.GrantSource source) {
		if (playerUUID.equals(ownerUUID)) {
			return false;
		}
		grants.put(playerUUID, new WindowGrant(playerUUID, level, grantedBy, nowMs, expiresAtMs, source));
		return true;
	}

	/**
	 * 显式吊销：写入 NONE 墓碑（压过后续默认策略回放）。owner 拒绝改写。
	 */
	public boolean revoke(UUID playerUUID, @Nullable UUID revokedBy, long nowMs) {
		if (playerUUID.equals(ownerUUID)) {
			return false;
		}
		grants.put(playerUUID, WindowGrant.tombstone(playerUUID, revokedBy, nowMs, WindowGrant.GrantSource.MANUAL));
		return true;
	}

	/** 取记录（不做过期过滤；过期判断交给调用方/compose/快查路径）。 */
	@Nullable
	public WindowGrant getGrant(UUID playerUUID) {
		return grants.get(playerUUID);
	}

	/**
	 * 记录快查（转发热路径）：过期视为无权限并惰性摘除该记录。
	 * 注意权威判定仍是 SharedWindowManager.effectivePermission（叠加黑白名单）；
	 * 本方法用于不需要黑白名单上下文的场景（如 owner 自身的 CONTROL 检查）。
	 */
	public WindowPermission getPermission(UUID playerUUID) {
		WindowGrant g = grants.get(playerUUID);
		if (g == null) return WindowPermission.NONE;
		long now = System.currentTimeMillis();
		if (g.isExpired(now)) {
			// 惰性摘除（仅当仍是同一条记录，防与并发 grant 竞争）
			grants.remove(playerUUID, g);
			return WindowPermission.NONE;
		}
		return g.level();
	}

	public boolean hasPermission(UUID playerUUID, WindowPermission required) {
		return getPermission(playerUUID).hasPermission(required);
	}

	/** 快照副本：调用方（含转发线程）遍历时不会与写入方产生竞争 */
	public Map<UUID, WindowPermission> getAllPermissions() {
		long now = System.currentTimeMillis();
		Map<UUID, WindowPermission> out = new HashMap<>();
		for (Map.Entry<UUID, WindowGrant> e : grants.entrySet()) {
			WindowGrant g = e.getValue();
			out.put(e.getKey(), g.isExpired(now) ? WindowPermission.NONE : g.level());
		}
		return out;
	}

	/** ACL-5：授权记录快照（含来源/到期），审计与 /wl share acl 用。 */
	public Map<UUID, WindowGrant> grantsSnapshot() {
		return new HashMap<>(grants);
	}

	/**
	 * 过期扫描专用：暴露并发映射本体，使 sweep 能用 remove(key, expectedValue)
	 * 原子摘除"仍是这一条"的过期记录（与并发 grant 竞争安全）。
	 * 仅供 SharedWindowManager.expireGrants 使用，勿在别处直接改。
	 */
	ConcurrentHashMap<UUID, WindowGrant> grantsForSweep() {
		return grants;
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
