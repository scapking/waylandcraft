package dev.evvie.waylandcraft.network;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

/**
 * ⑦ v2：新增 captureTimestampMs / frameSeq。
 * 
 * - captureTimestampMs：发送端捕获瞬间的 System.currentTimeMillis()。接收端用它做
 *   playout 时钟（配合本地到达时间的偏移估计），实现"预缓冲换流畅"。跨机时钟偏移
 *   不影响平滑性——播放节奏由 ts 的相对差驱动，绝对偏移由 min-EWMA 估计吸收。
 * - frameSeq：同窗口单调递增序号（发送端 state.frameCount），用于丢包/乱序观测与
 *   诊断日志；协议上不依赖它重组。
 * 
 * ID 升级为 shared_window_image_v2：与旧客户端/旧服务端混布时，未注册方直接忽略
 * 该通道（窗口不可见），而不是按旧 schema 解出错帧。两端需同批升级。
 */
public record SharedWindowImagePayload(long windowHandle, int frameNumber, int x, int y, int width, int height, byte[] imageData, double pivotX, double pivotY, double pivotZ, double normalX, double normalY, double normalZ, double downX, double downY, double downZ, double viewScale, int geometryWidth, int geometryHeight, int senderPixelsPerBlock, long captureTimestampMs, int frameSeq) implements CustomPacketPayload {
	
	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "shared_window_image_v2");
	
	public static final CustomPacketPayload.Type<SharedWindowImagePayload> TYPE = new CustomPacketPayload.Type<>(ID);
	
	public static final StreamCodec<RegistryFriendlyByteBuf, SharedWindowImagePayload> CODEC = StreamCodec.of(
		(buf, payload) -> {
			buf.writeLong(payload.windowHandle);
			buf.writeVarInt(payload.frameNumber);
			buf.writeVarInt(payload.x);
			buf.writeVarInt(payload.y);
			buf.writeVarInt(payload.width);
			buf.writeVarInt(payload.height);
			buf.writeVarInt(payload.imageData.length);
			buf.writeBytes(payload.imageData);
			buf.writeDouble(payload.pivotX);
			buf.writeDouble(payload.pivotY);
			buf.writeDouble(payload.pivotZ);
			buf.writeDouble(payload.normalX);
			buf.writeDouble(payload.normalY);
			buf.writeDouble(payload.normalZ);
			buf.writeDouble(payload.downX);
			buf.writeDouble(payload.downY);
			buf.writeDouble(payload.downZ);
			buf.writeDouble(payload.viewScale);
			buf.writeVarInt(payload.geometryWidth);
			buf.writeVarInt(payload.geometryHeight);
			buf.writeVarInt(payload.senderPixelsPerBlock);
			buf.writeLong(payload.captureTimestampMs);
			buf.writeVarInt(payload.frameSeq);
		},
		buf -> {
			long windowHandle = buf.readLong();
			int frameNumber = buf.readVarInt();
			int x = buf.readVarInt();
			int y = buf.readVarInt();
			int width = buf.readVarInt();
			int height = buf.readVarInt();
			int dataLength = buf.readVarInt();
			byte[] imageData = new byte[dataLength];
			buf.readBytes(imageData);
			double pivotX = buf.readDouble();
			double pivotY = buf.readDouble();
			double pivotZ = buf.readDouble();
			double normalX = buf.readDouble();
			double normalY = buf.readDouble();
			double normalZ = buf.readDouble();
			double downX = buf.readDouble();
			double downY = buf.readDouble();
			double downZ = buf.readDouble();
			double viewScale = buf.readDouble();
			int geometryWidth = buf.readVarInt();
			int geometryHeight = buf.readVarInt();
			int senderPixelsPerBlock = buf.readVarInt();
			long captureTimestampMs = buf.readLong();
			int frameSeq = buf.readVarInt();
			return new SharedWindowImagePayload(windowHandle, frameNumber, x, y, width, height, imageData, pivotX, pivotY, pivotZ, normalX, normalY, normalZ, downX, downY, downZ, viewScale, geometryWidth, geometryHeight, senderPixelsPerBlock, captureTimestampMs, frameSeq);
		}
	);
	
	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
	
}
