package dev.evvie.waylandcraft.network;

import java.util.UUID;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.ShareAcl;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

/**
 * ACL-2 C2S：owner 对 {@link ShareInviteRequestPayload} 的应答。
 *
 * 服务端强制校验：发送者必须仍是该窗口的当前 owner（窗口已注销/换主则拒绝），
 * 请求者 UUID 原样回传（服务端不信任客户端的名字解析结果）。
 *
 * decision 见 {@link ShareAcl#INVITE_DENY} 等常量：
 * DENY → 写吊销墓碑；ALLOW_ALWAYS → 永久授权；ALLOW_TIMED → 限时（durationSeconds）。
 */
public record ShareInviteResponsePayload(long windowHandle, UUID requesterUUID, byte decision,
                                         byte permissionLevel, long durationSeconds) implements CustomPacketPayload {

	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "share_invite_response");

	public static final CustomPacketPayload.Type<ShareInviteResponsePayload> TYPE = new CustomPacketPayload.Type<>(ID);

	/** 时长秒上限（30 天，与 ShareAcl.parseDuration 封顶一致；越界钳到上限而非拒绝，防溢出） */
	static final long MAX_DURATION_SECONDS = 30L * 86400L;

	public static final StreamCodec<RegistryFriendlyByteBuf, ShareInviteResponsePayload> CODEC = StreamCodec.of(
		(buf, p) -> {
			buf.writeLong(p.windowHandle);
			buf.writeUUID(p.requesterUUID);
			buf.writeByte(p.decision);
			buf.writeByte(p.permissionLevel);
			buf.writeVarLong(p.durationSeconds);
		},
		buf -> {
			long handle = buf.readLong();
			UUID requester = buf.readUUID();
			// 非法 decision 字节钳到 DENY（保守：绝不当成"允许"）
			byte rawDecision = buf.readByte();
			byte decision = (rawDecision >= ShareAcl.INVITE_DENY && rawDecision <= ShareAcl.INVITE_ALLOW_TIMED)
				? rawDecision : ShareAcl.INVITE_DENY;
			byte level = buf.readByte();
			long duration = Math.max(0, Math.min(MAX_DURATION_SECONDS, buf.readVarLong()));
			return new ShareInviteResponsePayload(handle, requester, decision, level, duration);
		}
	);

	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
}
