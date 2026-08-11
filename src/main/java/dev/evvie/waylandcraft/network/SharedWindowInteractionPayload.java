package dev.evvie.waylandcraft.network;

import java.util.UUID;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

public record SharedWindowInteractionPayload(long windowHandle, InteractionType interactionType, double x, double y, int button, int key, UUID senderUUID) implements CustomPacketPayload {
	
	public enum InteractionType {
		MOUSE_MOVE(0),
		MOUSE_CLICK(1),
		MOUSE_RELEASE(2),
		KEY_PRESS(3),
		KEY_RELEASE(4),
		SCROLL(5);
		
		private final int id;
		
		InteractionType(int id) {
			this.id = id;
		}
		
		public int getId() {
			return id;
		}
		
		public static InteractionType fromId(int id) {
			for (InteractionType type : values()) {
				if (type.id == id) return type;
			}
			return MOUSE_MOVE;
		}
	}

	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "shared_window_interaction");
	
	public static final CustomPacketPayload.Type<SharedWindowInteractionPayload> TYPE = new CustomPacketPayload.Type<>(ID);
	
	public static final StreamCodec<RegistryFriendlyByteBuf, SharedWindowInteractionPayload> CODEC = StreamCodec.of(
		(buf, payload) -> {
			buf.writeLong(payload.windowHandle);
			buf.writeVarInt(payload.interactionType.getId());
			buf.writeDouble(payload.x);
			buf.writeDouble(payload.y);
			buf.writeVarInt(payload.button);
			buf.writeVarInt(payload.key);
			buf.writeBoolean(payload.senderUUID != null);
			if (payload.senderUUID != null) {
				buf.writeUUID(payload.senderUUID);
			}
		},
		buf -> new SharedWindowInteractionPayload(
			buf.readLong(),
			InteractionType.fromId(buf.readVarInt()),
			buf.readDouble(),
			buf.readDouble(),
			buf.readVarInt(),
			buf.readVarInt(),
			buf.readBoolean() ? buf.readUUID() : null
		)
	);
	
	/**
	 * 创建带发送者UUID的副本
	 */
	public SharedWindowInteractionPayload withSender(UUID senderUUID) {
		return new SharedWindowInteractionPayload(windowHandle, interactionType, x, y, button, key, senderUUID);
	}
	
	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
	
}
