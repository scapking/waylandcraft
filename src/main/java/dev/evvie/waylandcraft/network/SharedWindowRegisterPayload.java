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

	/**
	 * 线上字段读端上限（写端保留原行为不受影响）。
	 * title 取 4096 而非 64：handler 有 64 截断，但读端必须容忍旧版写出的
	 * 合法长标题（readUtf 超限会抛 → 断线），同时封顶恶意分配的内存。
	 */
	static final int MAX_TITLE_BYTES = 4096;
	static final int MAX_TARGET_BYTES = 64;

	public static final StreamCodec<RegistryFriendlyByteBuf, SharedWindowRegisterPayload> CODEC = StreamCodec.of(
		// P2-4：写端保持原行为不变（与旧版客户端/服务端字节格式一致，兼容性由读端保证）
		(buf, payload) -> {
			buf.writeLong(payload.windowHandle);
			buf.writeUtf(payload.windowTitle);
			buf.writeUtf(payload.targetPlayer);
		},
		buf -> {
			long windowHandle = buf.readLong();
			// P2-3：读端限长，防恶意超长 title 撑内存（服务端还会截断到 64）
			String windowTitle = buf.readUtf(MAX_TITLE_BYTES);
			// P2-4：反向兼容——旧客户端只写 handle+title，缺 targetPlayer 字段时
			// 按空串（= 公开共享）处理，而不是 EOF 异常断开连接
			String targetPlayer = buf.readableBytes() > 0 ? buf.readUtf(MAX_TARGET_BYTES) : "";
			return new SharedWindowRegisterPayload(windowHandle, windowTitle, targetPlayer);
		}
	);

	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
	
}
