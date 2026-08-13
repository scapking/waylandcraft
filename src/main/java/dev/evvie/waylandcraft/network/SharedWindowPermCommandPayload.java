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
 *   GRANT  — 给目标玩家授予 VIEW 权限（定向/追加授权）
 *   REVOKE — 收回目标玩家的查看权限（设为 NONE，等同"对该玩家去除共享"）
 *   LIST   — 列出该窗口当前的授权情况
 */
public record SharedWindowPermCommandPayload(long windowHandle, byte action, String targetName) implements CustomPacketPayload {

	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "shared_window_perm_command");

	public static final CustomPacketPayload.Type<SharedWindowPermCommandPayload> TYPE = new CustomPacketPayload.Type<>(ID);

	public static final StreamCodec<RegistryFriendlyByteBuf, SharedWindowPermCommandPayload> CODEC = StreamCodec.of(
		(buf, p) -> {
			buf.writeLong(p.windowHandle);
			buf.writeByte(p.action);
			buf.writeUtf(p.targetName, 64);
		},
		buf -> new SharedWindowPermCommandPayload(
			buf.readLong(),
			buf.readByte(),
			buf.readUtf(64)
		)
	);

	public static final byte ACTION_GRANT = 0;
	public static final byte ACTION_REVOKE = 1;
	public static final byte ACTION_LIST = 2;

	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
}
