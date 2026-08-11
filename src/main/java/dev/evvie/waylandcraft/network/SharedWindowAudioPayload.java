package dev.evvie.waylandcraft.network;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;

/**
 * 共享窗口音频包（PCM 直传）。
 * 
 * 发送端把 PipeWire 捕获的 PCM 分包（每包 ≤ ~30KB）发送；
 * 服务端原样转发给有 VIEW 权限的其他玩家；
 * 接收端按 windowHandle 用 OpenAL 连续播放。
 * 
 * @param windowHandle 共享窗口句柄（用于区分不同共享源）
 * @param seq         递增序号（同窗口内保序；丢包可接受，音频不重组）
 * @param sampleRate  采样率（Hz），如 48000
 * @param channels    声道数（1=mono, 2=stereo）
 * @param pcmData     S16LE PCM 数据
 */
public record SharedWindowAudioPayload(long windowHandle, int seq, int sampleRate, int channels, byte[] pcmData) implements CustomPacketPayload {
	
	public static final Identifier ID = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "shared_window_audio");
	
	public static final CustomPacketPayload.Type<SharedWindowAudioPayload> TYPE = new CustomPacketPayload.Type<>(ID);
	
	public static final StreamCodec<RegistryFriendlyByteBuf, SharedWindowAudioPayload> CODEC = StreamCodec.of(
		(buf, payload) -> {
			buf.writeLong(payload.windowHandle);
			buf.writeVarInt(payload.seq);
			buf.writeVarInt(payload.sampleRate);
			buf.writeVarInt(payload.channels);
			buf.writeVarInt(payload.pcmData.length);
			buf.writeBytes(payload.pcmData);
		},
		buf -> {
			long windowHandle = buf.readLong();
			int seq = buf.readVarInt();
			int sampleRate = buf.readVarInt();
			int channels = buf.readVarInt();
			int dataLength = buf.readVarInt();
			byte[] pcmData = new byte[dataLength];
			buf.readBytes(pcmData);
			return new SharedWindowAudioPayload(windowHandle, seq, sampleRate, channels, pcmData);
		}
	);
	
	@Override
	public Type<? extends CustomPacketPayload> type() {
		return TYPE;
	}
	
}
