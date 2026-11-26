package dev.evvie.waylandcraft.shared;

import org.jetbrains.annotations.Nullable;

/**
 * ACL-2：权限合成分判定的纯函数层（服务端权威收口，审计 P2-6"无消费者权威表"的落点）。
 *
 * 所有判定路径（帧/音频转发、交互转发、窗口列表广播、state 更新）统一走
 * {@link #compose}，优先级固定：
 *
 * <pre>
 * owner 恒 CONTROL  >  黑名单 NONE  >  白名单级别  >  per-window 授权记录（含过期与墓碑）  >  NONE
 * </pre>
 *
 * "默认策略"（allow/ask/deny）不在这里做环境默认：它体现在 join/register 时刻是否
 * 物化为 source=DEFAULT 的记录（见 SharedWindowManager.applyDefaultPolicy），
 * 保证 revoke 墓碑 / 限时过期语义清晰：
 * - 有记录且未过期 → 记录级别（NONE = 显式吊销，压过一切默认）。
 * - 记录过期被回收 → 无记录 → 公开窗口回到 DEFAULT 策略回放（由过期扫描重新派生），
 *   定向窗口回 NONE。
 *
 * 不依赖任何全局单例/MC 类型——可纯单测。
 */
public final class ShareAcl {

	/** 新观众默认策略（/wl share default ask|allow|deny）。 */
	public enum NewViewerPolicy {
		/** 公开窗口自动派生 DEFAULT 授权（现行为，向后兼容默认值）。 */
		ALLOW,
		/** 不自动授权：向 owner 发邀请，由 owner 决定（收紧档）。 */
		ASK,
		/** 不自动授权也不邀请：仅 owner 手动 grant 可见（最严档）。 */
		DENY;

		public static NewViewerPolicy fromNameOr(@Nullable String name, NewViewerPolicy fallback) {
			if (name == null) return fallback;
			switch (name.trim().toLowerCase()) {
				case "allow": return ALLOW;
				case "ask": return ASK;
				case "deny": return DENY;
				default: return fallback;
			}
		}
	}

	private ShareAcl() {}

	/**
	 * 合成判定核心（纯函数）。
	 *
	 * @param isOwner       请求者是否该窗口 owner
	 * @param blacklisted   PermissionManager 黑名单命中
	 * @param whitelist     PermissionManager 白名单级别（未命中传 null）
	 * @param record        该窗口的 per-window 授权记录（无记录传 null）
	 * @param recordExpired 记录是否已按注入时钟过期
	 * @param isTargeted    定向共享窗口（默认层回退必须为 NONE，不得放行公开默认）
	 * @param nowMs         注入时钟
	 */
	public static WindowPermission compose(boolean isOwner, boolean blacklisted,
			@Nullable WindowPermission whitelist, @Nullable WindowGrant record,
			boolean recordExpired, boolean isTargeted, long nowMs) {
		// owner 恒全权：即使记录被误删/墓碑化也不降级（显式测试锁定）
		if (isOwner) return WindowPermission.CONTROL;
		// 黑名单一票否决（运维显式禁令压过 owner 授权？——压过"默认"，
		// 但 owner 无法给被封禁的玩家开窗：黑名单先于 per-window，防绕过）
		if (blacklisted) return WindowPermission.NONE;
		if (whitelist != null) return whitelist;
		if (record != null && !recordExpired) return record.level();
		return WindowPermission.NONE;
	}

	/**
	 * 过期记录被回收后该玩家是否应重新获得 DEFAULT 派生授权
	 * （公开窗口 + ALLOW 策略；定向窗口永不；黑名单永不）。纯函数。
	 */
	public static boolean shouldReapplyDefault(boolean isTargeted, NewViewerPolicy policy,
			boolean blacklisted, boolean isOwner) {
		if (isOwner || isTargeted || blacklisted) return false;
		return policy == NewViewerPolicy.ALLOW;
	}

	/**
	 * 解析时长字面量：10m / 1h / 30s / 2d / 90（裸数=秒）。
	 * 非法或超界返回 -1（调用方回错误提示）。上限 30 天，防误输入"100000d"变相永久。
	 */
	public static long parseDurationMs(@Nullable String raw) {
		if (raw == null) return -1;
		String s = raw.trim().toLowerCase();
		if (s.isEmpty()) return -1;
		// 裸数字 = 秒
		int unitMult = 1000;
		char last = s.charAt(s.length() - 1);
		if (last >= 'a' && last <= 'z') {
			switch (last) {
				case 's' -> unitMult = 1_000;
				case 'm' -> unitMult = 60_000;
				case 'h' -> unitMult = 3_600_000;
				case 'd' -> unitMult = 86_400_000;
				default -> { return -1; }
			}
			s = s.substring(0, s.length() - 1);
		}
		long value;
		try {
			value = Long.parseLong(s);
		} catch (NumberFormatException e) {
			return -1;
		}
		if (value <= 0) return -1;
		long ms;
		try {
			ms = Math.multiplyExact(value, (long) unitMult);
		} catch (ArithmeticException e) {
			return -1;
		}
		long cap = 30L * 86_400_000L; // 30 天
		return ms > cap ? cap : ms;
	}

	/**
	 * 邀请时长字面量的秒数形态（payload 用秒传输，避免 varlong 兼容面）。
	 * 非法返回 -1；封顶后换算，保证 0 < s <= 30d。
	 */
	public static long parseDurationSeconds(@Nullable String raw) {
		long ms = parseDurationMs(raw);
		return ms < 0 ? -1 : Math.max(1, ms / 1000);
	}

	/**
	 * 邀请响应 decision 常量（payload 与 handler 共享的单一事实源）。
	 */
	public static final byte INVITE_DENY = 0;
	public static final byte INVITE_ALLOW_ALWAYS = 1;
	/** 限时允许：durationSeconds 由 payload 携带（UI 按钮默认 10m=600） */
	public static final byte INVITE_ALLOW_TIMED = 2;

	/** 邀请响应鉴权（纯函数）：只有该窗口 owner 能应答，且窗口必须仍在注册表。 */
	public static boolean canRespondToInvite(boolean windowExists, boolean senderIsOwner) {
		return windowExists && senderIsOwner;
	}

	/**
	 * 窗口管理鉴权（纯函数）：grant/revoke/list 一律 owner 权威——
	 * 管理员与其他 CONTROL 持有者都不越俎代庖（"谁共享谁管理"）。
	 */
	public static boolean canManageWindow(@Nullable java.util.UUID ownerUUID, @Nullable java.util.UUID requesterUUID) {
		return ownerUUID != null && ownerUUID.equals(requesterUUID);
	}

	/**
	 * grant 请求的等级钳制（纯函数）：owner 之间不互授（调用方拦），
	 * 对他人最高可授 CONTROL——但 NONE 不能作为"grant"表达（那是 revoke），
	 * 非法/越界一律落 VIEW（保守最小授权，绝不提权）。
	 */
	public static WindowPermission clampGrantedLevel(@Nullable WindowPermission requested) {
		if (requested == null) return WindowPermission.VIEW;
		return switch (requested) {
			case NONE -> WindowPermission.VIEW; // NONE 请求按最小可用授权处理
			case VIEW, INTERACT, CONTROL -> requested;
		};
	}

	/** 限时授权的绝对到期时刻（now + duration；永久 = NEVER_EXPIRES）。 */
	public static long expiryOf(long nowMs, long durationMs) {
		if (durationMs <= 0) return WindowGrant.NEVER_EXPIRES;
		return nowMs + durationMs;
	}

	/** ACL 行渲染（/wl share acl 与审计日志共用），纯函数。 */
	public static String describeGrant(@Nullable WindowGrant g, long nowMs) {
		if (g == null) return "none";
		StringBuilder sb = new StringBuilder();
		sb.append(g.level().name());
		sb.append(" (").append(g.source().name().toLowerCase());
		if (!g.isPermanent()) {
			long rem = g.remainingMs(nowMs);
			sb.append(g.isExpired(nowMs) ? ", expired" : ", ").append(rem / 1000).append("s left");
		} else if (g.level() == WindowPermission.NONE) {
			sb.append(", revoked");
		} else {
			sb.append(", forever");
		}
		sb.append(")");
		return sb.toString();
	}
}
