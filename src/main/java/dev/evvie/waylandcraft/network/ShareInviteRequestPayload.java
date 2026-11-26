package dev.evvie.waylandcraft.network;

import java.util.UUID;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

/**
 * ACL-2 S2C：新观众在 default=ask 策略下请求查看某公开共享窗口时，
 * 服务端通知窗口 owner 处理（客户端以聊天点击命令形态呈现"允许10m/始终允许/拒绝"）。
 *
 * 旧客户端（未注册本 payload）：ServerPlayNetworking.canSend 为 false，
 * 服务端跳过发送并计数——观众保持无授权（fail-closed，不会静默放行）。
 */
public record ShareInviteRequestPayload(long windowHandle, UUID requesterUUID, String requesterName) implements CustomPacketPayload {

	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "share_invite_request");

	public static final CustomPacketPayload.Type<ShareInviteRequestPayload> TYPE = new CustomPacketPayload.Type<>(ID);

	static final int MAX_NAME_BYTES = 64;

	public static final StreamCodec<RegistryFriendlyByteBuf, ShareInviteRequestPayload> CODEC = StreamCodec.of(
		(buf, p) -> {
			buf.writeLong(p.windowHandle);
			buf.writeUUID(p.requesterUUID);
			buf.writeUtf(p.requesterName, MAX_NAME_BYTES);
		},
		buf -> new ShareInviteRequestPayload(
			buf.readLong(),
			buf.readUUID(),
			buf.readUtf(MAX_NAME_BYTES)
		)
	);

	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
}
