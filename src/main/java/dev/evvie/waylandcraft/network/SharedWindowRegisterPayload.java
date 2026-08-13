package dev.evvie.waylandcraft.network;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

/**
 * C2S: 请求注册共享窗口。
 *
 * targetPlayer 为空字符串 = 共享给所有在线玩家（公开共享）；
 * 非空 = 只共享给该玩家（定向共享，其余玩家不可见）。
 */
public record SharedWindowRegisterPayload(long windowHandle, String windowTitle, String targetPlayer) implements CustomPacketPayload {
	
	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "shared_window_register");
	
	public static final CustomPacketPayload.Type<SharedWindowRegisterPayload> TYPE = new CustomPacketPayload.Type<>(ID);
	
	public static final StreamCodec<RegistryFriendlyByteBuf, SharedWindowRegisterPayload> CODEC = StreamCodec.of(
		(buf, payload) -> {
			buf.writeLong(payload.windowHandle);
			buf.writeUtf(payload.windowTitle);
			buf.writeUtf(payload.targetPlayer);
		},
		buf -> new SharedWindowRegisterPayload(buf.readLong(), buf.readUtf(), buf.readUtf())
	);
	
	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
	
}
