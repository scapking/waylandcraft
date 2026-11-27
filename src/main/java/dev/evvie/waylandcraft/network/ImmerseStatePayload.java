package dev.evvie.waylandcraft.network;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

/**
 * v1.5.0 沉浸模式 S2C 状态回执（服务端权威）。
 *
 * <p>协议契约（与客户端渲染侧共享，勿单方改动）：
 * <pre>
 * record ImmerseStatePayload(boolean active, String mode, boolean serverConfirmed)
 *   active          服务端最终状态（true=沉浸中 / false=已退出）
 *   mode            服务端实际采用的 mode（非法请求被拒时为 "still"）
 *   serverConfirmed 恒为 true——只有服务端真实落状态后才发本包；
 *                   客户端渲染侧以收到本包作为进入/退出渲染的唯一权威依据。
 * </pre>
 */
public record ImmerseStatePayload(boolean active, String mode, boolean serverConfirmed) implements CustomPacketPayload {

	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "immerse_state");

	public static final CustomPacketPayload.Type<ImmerseStatePayload> TYPE = new CustomPacketPayload.Type<>(ID);

	public static final StreamCodec<RegistryFriendlyByteBuf, ImmerseStatePayload> CODEC = StreamCodec.of(
		(buf, payload) -> {
			buf.writeBoolean(payload.active);
			buf.writeUtf(payload.mode == null ? "" : payload.mode, ImmerseTogglePayload.MAX_MODE_BYTES);
			buf.writeBoolean(payload.serverConfirmed);
		},
		buf -> new ImmerseStatePayload(buf.readBoolean(), buf.readUtf(ImmerseTogglePayload.MAX_MODE_BYTES), buf.readBoolean())
	);

	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}

}
