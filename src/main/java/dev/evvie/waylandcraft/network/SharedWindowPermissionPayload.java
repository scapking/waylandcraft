package dev.evvie.waylandcraft.network;

import java.util.UUID;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.WindowPermission;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

public record SharedWindowPermissionPayload(long windowHandle, UUID playerUUID, WindowPermission permission) implements CustomPacketPayload {
	
	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "shared_window_permission");
	
	public static final CustomPacketPayload.Type<SharedWindowPermissionPayload> TYPE = new CustomPacketPayload.Type<>(ID);
	
	public static final StreamCodec<RegistryFriendlyByteBuf, SharedWindowPermissionPayload> CODEC = StreamCodec.of(
		(buf, payload) -> {
			buf.writeLong(payload.windowHandle);
			buf.writeUUID(payload.playerUUID);
			buf.writeVarInt(payload.permission.getLevel());
		},
		buf -> new SharedWindowPermissionPayload(
			buf.readLong(),
			buf.readUUID(),
			// P1-6：非法等级回退 NONE（拒绝），不再 values()[越界] AIOOBE
			WindowPermission.fromLevel(buf.readVarInt())
		)
	);
	
	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
	
}
