package dev.evvie.waylandcraft.shared;

import java.util.UUID;
import java.util.concurrent.atomic.AtomicLong;

import org.jetbrains.annotations.Nullable;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * ACL-5：授权事件审计（结构化日志 + 服务端计数，进 SharedWindowDebug 状态报告）。
 *
 * 日志规范与子系统既有时间戳约定一致（slf4j 前缀），事件体单行竖线分字段，
 * 可被 grep/日志采集直接解析：
 * <pre>
 * ACL|op=GRANT win=0x1f2e player=uuid by=uuid level=VIEW src=MANUAL expires=1735300000000
 * </pre>
 *
 * 全部静态 AtomicLong：仅服务端写；clear() 挂 SERVER_STOPPED（P2-5 同模式），
 * 不跨会话残留。
 */
public final class ShareAclAudit {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-acl");

	/** grant 事件按来源计数 */
	public static final AtomicLong grantsManual = new AtomicLong();
	public static final AtomicLong grantsInvite = new AtomicLong();
	public static final AtomicLong grantsDefault = new AtomicLong();
	/** 到期自动回收数 */
	public static final AtomicLong expired = new AtomicLong();
	/** 显式 revoke（含邀请拒绝墓碑）数 */
	public static final AtomicLong revoked = new AtomicLong();
	/** 邀请发出 / 接受 / 拒绝 */
	public static final AtomicLong inviteSent = new AtomicLong();
	public static final AtomicLong inviteAccepted = new AtomicLong();
	public static final AtomicLong inviteDenied = new AtomicLong();
	/** 鉴权拒绝计数（越权包被丢弃的次数，按类别） */
	public static final AtomicLong deniedGrantForged = new AtomicLong();
	public static final AtomicLong deniedNotOwner = new AtomicLong();
	public static final AtomicLong deniedInviteResponse = new AtomicLong();
	public static final AtomicLong deniedInteract = new AtomicLong();
	public static final AtomicLong deniedState = new AtomicLong();
	/** handle 重注册清空旧 ACL 次数（正常同 owner 重建窗口；越权复用被拒也计） */
	public static final AtomicLong reregisterCleared = new AtomicLong();
	public static final AtomicLong reregisterRejected = new AtomicLong();

	private ShareAclAudit() {}

	/**
	 * 授权/吊销事件行。op ∈ GRANT/REVOKE/EXPIRE/INVITE_SEND/INVITE_DENY；
	 * expiresMs == WindowGrant.NEVER_EXPIRES 时打 forever。
	 */
	public static void log(String op, long windowHandle, UUID player, @Nullable UUID by,
			WindowPermission level, WindowGrant.GrantSource source, long expiresMs) {
		LOGGER.info("ACL|op={} win=0x{} player={} by={} level={} src={} expires={}",
			op, Long.toHexString(windowHandle), player, by != null ? by : "-",
			level, source != null ? source.name().toLowerCase() : "-",
			expiresMs == WindowGrant.NEVER_EXPIRES ? "forever" : Long.toString(expiresMs));
	}

	/** 事件计数（与 log 成对调用，避免遗漏） */
	public static void countGrant(WindowGrant.GrantSource source) {
		switch (source) {
			case MANUAL -> grantsManual.incrementAndGet();
			case INVITE -> grantsInvite.incrementAndGet();
			case DEFAULT -> grantsDefault.incrementAndGet();
		}
	}

	public static void logDeny(String kind, long windowHandle, UUID player) {
		LOGGER.info("ACL|op=DENY kind={} win=0x{} player={}", kind, Long.toHexString(windowHandle), player);
	}

	/** 状态报告块（SharedWindowDebug 调用） */
	public static String reportBlock() {
		StringBuilder sb = new StringBuilder();
		sb.append("ACL grants: manual=").append(grantsManual.get())
			.append(" invite=").append(grantsInvite.get())
			.append(" default=").append(grantsDefault.get()).append("\n");
		sb.append("ACL lifecycle: expired=").append(expired.get())
			.append(" revoked=").append(revoked.get()).append("\n");
		sb.append("ACL invites: sent=").append(inviteSent.get())
			.append(" accepted=").append(inviteAccepted.get())
			.append(" denied=").append(inviteDenied.get()).append("\n");
		sb.append("ACL denies: forged_grant=").append(deniedGrantForged.get())
			.append(" not_owner=").append(deniedNotOwner.get())
			.append(" invite_response=").append(deniedInviteResponse.get())
			.append(" interact=").append(deniedInteract.get())
			.append(" state=").append(deniedState.get()).append("\n");
		sb.append("ACL reregister: cleared=").append(reregisterCleared.get())
			.append(" rejected=").append(reregisterRejected.get()).append("\n");
		return sb.toString();
	}

	/** 会话停止清零 */
	public static void clear() {
		grantsManual.set(0);
		grantsInvite.set(0);
		grantsDefault.set(0);
		expired.set(0);
		revoked.set(0);
		inviteSent.set(0);
		inviteAccepted.set(0);
		inviteDenied.set(0);
		deniedGrantForged.set(0);
		deniedNotOwner.set(0);
		deniedInviteResponse.set(0);
		deniedInteract.set(0);
		deniedState.set(0);
		reregisterCleared.set(0);
		reregisterRejected.set(0);
	}
}
