package dev.evvie.waylandcraft.network;

import java.util.ArrayList;
import java.util.List;
import java.util.UUID;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.WindowPermission;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

public record SharedWindowListPayload(List<WindowInfo> windows) implements CustomPacketPayload {
	
	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "shared_window_list");
	
	public static final CustomPacketPayload.Type<SharedWindowListPayload> TYPE = new CustomPacketPayload.Type<>(ID);
	
	public static final StreamCodec<RegistryFriendlyByteBuf, SharedWindowListPayload> CODEC = StreamCodec.of(
		(buf, payload) -> {
			buf.writeVarInt(payload.windows.size());
			for (WindowInfo info : payload.windows) {
				buf.writeLong(info.windowHandle());
				buf.writeUUID(info.ownerUUID());
				buf.writeUtf(info.windowTitle());
				buf.writeVarInt(info.permission().getLevel());
			}
		},
		buf -> {
			int count = buf.readVarInt();
			List<WindowInfo> windows = new ArrayList<>();
			for (int i = 0; i < count; i++) {
				windows.add(new WindowInfo(
					buf.readLong(),
					buf.readUUID(),
					buf.readUtf(),
					// P1-6：非法等级回退 NONE（拒绝），不再 values()[越界] AIOOBE 崩解码线程
					WindowPermission.fromLevel(buf.readVarInt())
				));
			}
			return new SharedWindowListPayload(windows);
		}
	);
	
	public record WindowInfo(long windowHandle, UUID ownerUUID, String windowTitle, WindowPermission permission) {}
	
	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
	
}
