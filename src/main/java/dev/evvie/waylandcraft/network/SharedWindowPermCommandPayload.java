package dev.evvie.waylandcraft.network;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

/**
 * C2S: 窗口所有者（owner）的授权管理命令，按窗口走 owner 权威校验。
 *
 * 与全局 {@link PermissionCommandPayload} 的区别：本 payload 针对单个共享窗口，
 * 服务端强制校验请求者 == 该窗口 owner，非 owner 一律拒绝。
 *
 * action:
 *   GRANT  — 授权（ACL-1 起带等级与时长：permissionLevel + durationSeconds）
 *   REVOKE — 收回目标玩家的权限（写吊销墓碑）
 *   LIST   — 列出该窗口当前的授权情况（owner 校验）
 *   ACL    — ACL-5：跨窗口 ACL 报表（handle=0 全部；owner/op 看全量行，
 *            非 owner/非 op 只看自己相关行。可见性由服务端过滤，无需 owner 校验）
 *
 * 兼容性：新字段追加在尾部，读端用 readableBytes 探测——旧客户端只写
 * handle/action/targetName 时按 level=VIEW、duration=0（永久）处理，
 * 与历史行为一致（register payload 同款宽容读端模式）。
 */
public record SharedWindowPermCommandPayload(long windowHandle, byte action, String targetName,
                                             byte permissionLevel, long durationSeconds) implements CustomPacketPayload {

	/** 兼容旧三字段构造点（= VIEW / 永久） */
	public SharedWindowPermCommandPayload(long windowHandle, byte action, String targetName) {
		this(windowHandle, action, targetName, (byte) dev.evvie.waylandcraft.shared.WindowPermission.VIEW.getLevel(), 0);
	}

	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "shared_window_perm_command");

	public static final CustomPacketPayload.Type<SharedWindowPermCommandPayload> TYPE = new CustomPacketPayload.Type<>(ID);

	static final int MAX_NAME_BYTES = 64;

	public static final StreamCodec<RegistryFriendlyByteBuf, SharedWindowPermCommandPayload> CODEC = StreamCodec.of(
		(buf, p) -> {
			buf.writeLong(p.windowHandle);
			buf.writeByte(p.action);
			buf.writeUtf(p.targetName, MAX_NAME_BYTES);
			buf.writeByte(p.permissionLevel);
			buf.writeVarLong(p.durationSeconds);
		},
		buf -> {
			long handle = buf.readLong();
			byte action = buf.readByte();
			String target = buf.readUtf(MAX_NAME_BYTES);
			byte level = buf.readableBytes() > 0 ? buf.readByte() : (byte) 1; // 缺省=VIEW（历史行为）
			long duration = buf.readableBytes() > 0 ? Math.max(0, buf.readVarLong()) : 0;
			return new SharedWindowPermCommandPayload(handle, action, target, level, duration);
		}
	);

	public static final byte ACTION_GRANT = 0;
	public static final byte ACTION_REVOKE = 1;
	public static final byte ACTION_LIST = 2;
	public static final byte ACTION_ACL = 3;

	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
}
