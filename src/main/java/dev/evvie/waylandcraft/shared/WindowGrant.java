package dev.evvie.waylandcraft.shared;

import java.util.UUID;

import org.jetbrains.annotations.Nullable;

/**
 * ACL-1：单窗口单玩家的授权记录（不可变值对象）。
 *
 * 取代原来"player -> WindowPermission"的裸映射，携带审计与生命周期信息：
 * - grantedBy：谁授的权（owner UUID；DEFAULT 派生记录为 null）。
 * - grantedAtMs / expiresAtMs：墙钟毫秒；expiresAtMs == 0 = 永久。
 * - source：manual（owner 命令/邀请以外的授权）、invite（邀请流程）、
 *   default（公开共享按默认策略自动派生）。
 *
 * level == NONE 是显式"吊销墓碑"：区别于"从未授权（无记录）"，
 * 它必须压过 default 层的自动放行，否则 revoke 会被下一次策略回放/
 * join 重授悄悄复活（典型逻辑洞）。
 */
public record WindowGrant(UUID playerUUID, WindowPermission level, @Nullable UUID grantedBy,
                          long grantedAtMs, long expiresAtMs, GrantSource source) {

	public enum GrantSource { MANUAL, INVITE, DEFAULT }

	/** 永久授权哨兵（expiresAtMs = 0） */
	public static final long NEVER_EXPIRES = 0L;

	public WindowGrant(UUID playerUUID, WindowPermission level, @Nullable UUID grantedBy,
			long grantedAtMs, GrantSource source) {
		this(playerUUID, level, grantedBy, grantedAtMs, NEVER_EXPIRES, source);
	}

	public boolean isPermanent() {
		return expiresAtMs == NEVER_EXPIRES;
	}

	/** 到期判定（纯函数，注入时钟可单测）。墓碑 NONE 永不算过期。 */
	public boolean isExpired(long nowMs) {
		return expiresAtMs != NEVER_EXPIRES && level != WindowPermission.NONE && nowMs >= expiresAtMs;
	}

	/** 剩余毫秒；永久返回 -1。已过期为 0。 */
	public long remainingMs(long nowMs) {
		if (isPermanent()) return -1;
		return Math.max(0, expiresAtMs - nowMs);
	}

	public static WindowGrant permanent(UUID playerUUID, WindowPermission level, @Nullable UUID grantedBy,
			long nowMs, GrantSource source) {
		return new WindowGrant(playerUUID, level, grantedBy, nowMs, NEVER_EXPIRES, source);
	}

	/** 吊销墓碑：显式 NONE 记录，压过 default 层。 */
	public static WindowGrant tombstone(UUID playerUUID, @Nullable UUID grantedBy, long nowMs, GrantSource source) {
		return new WindowGrant(playerUUID, WindowPermission.NONE, grantedBy, nowMs, NEVER_EXPIRES, source);
	}
}
