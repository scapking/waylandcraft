package dev.evvie.waylandcraft.network;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

/**
 * v1.5.0 沉浸模式 C2S 切换请求。
 *
 * <p>协议契约（与客户端渲染侧共享，勿单方改动）：
 * <pre>
 * record ImmerseTogglePayload(boolean enable, String mode)
 *   mode 仅接受 "still" | "ghost"；其他值（含 null/空/非法长度）由服务端
 *   resolveMode 拒绝并按 still 处理 + 回执（见 ImmerseServer）。
 * </pre>
 *
 * <p>写法与 SharedWindowInteractionPayload 同款：StreamCodec + CustomPacketPayload.Type；
 * mode 走 writeUtf 上限封顶，防恶意超长串分配（SharedWindowPermCommandPayload 同款防御）。
 */
public record ImmerseTogglePayload(boolean enable, String mode) implements CustomPacketPayload {

	/** mode 字段字节上限（wire 值最长 5 字节，16 足够；超限 readUtf 抛异常断线，同 house 防御策略） */
	public static final int MAX_MODE_BYTES = 16;

	public static final String MODE_STILL = "still";
	public static final String MODE_GHOST = "ghost";

	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "immerse_toggle");

	public static final CustomPacketPayload.Type<ImmerseTogglePayload> TYPE = new CustomPacketPayload.Type<>(ID);

	public static final StreamCodec<RegistryFriendlyByteBuf, ImmerseTogglePayload> CODEC = StreamCodec.of(
		(buf, payload) -> {
			buf.writeBoolean(payload.enable);
			// 防御：null 编不出线，落成空串由服务端 resolveMode 判拒绝
			buf.writeUtf(payload.mode == null ? "" : payload.mode, MAX_MODE_BYTES);
		},
		buf -> new ImmerseTogglePayload(buf.readBoolean(), buf.readUtf(MAX_MODE_BYTES))
	);

	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}

}
