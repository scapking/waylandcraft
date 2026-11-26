package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import io.netty.buffer.Unpooled;
import net.minecraft.core.RegistryAccess;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.SkipPacketDecoderException;

/**
 * P2-4/P2-3 修复证明（codec 直测，假 payload 构造，不启 MC 运行时）：
 * - register：读端兼容旧客户端（缺 targetPlayer 字段 → ""），写端格式不变。
 * - image/audio decode：恶意 VarInt 长度不再 NegativeArraySize/OOM，
 *   而是 SkipPacketDecoderException（丢包断连安全降级）。
 */
class PayloadCodecCompatTest {

	private static RegistryFriendlyByteBuf buf() {
		return new RegistryFriendlyByteBuf(Unpooled.buffer(), RegistryAccess.EMPTY);
	}

	@Test
	@DisplayName("register codec: 新客户端 round-trip（handle+title+targetPlayer）")
	void registerCodecRoundTripsTargeted() {
		var payload = new SharedWindowRegisterPayload(0xdeadBEEFL, "Firefox — Nightly", "SomePlayer");
		var out = buf();
		SharedWindowRegisterPayload.CODEC.encode(out, payload);
		var decoded = assertDoesNotThrow(() -> SharedWindowRegisterPayload.CODEC.decode(out));
		assertEquals(0xdeadBEEFL, decoded.windowHandle());
		assertEquals("Firefox — Nightly", decoded.windowTitle());
		assertEquals("SomePlayer", decoded.targetPlayer());
	}

	@Test
	@DisplayName("P2-4: 旧客户端只写 handle+title 时解码不 EOF，targetPlayer 补空串")
	void registerCodecBackCompatibleWithoutTargetField() {
		var out = buf();
		// 模拟旧版本写端：无 targetPlayer 字段
		out.writeLong(42L);
		out.writeUtf("OldClientWindow");
		out.markReaderIndex();

		var decoded = assertDoesNotThrow(() -> SharedWindowRegisterPayload.CODEC.decode(out));
		assertEquals(42L, decoded.windowHandle());
		assertEquals("OldClientWindow", decoded.windowTitle());
		assertEquals("", decoded.targetPlayer(), "缺字段必须按公开共享处理，而不是抛 EOF");
	}

	@Test
	@DisplayName("P2-4: 带显式空 targetPlayer 的当前写端仍解析为公开共享")
	void registerCodecEmptyTargetMeansPublic() {
		var out = buf();
		out.writeLong(7L);
		out.writeUtf("T");
		out.writeUtf("");
		var decoded = SharedWindowRegisterPayload.CODEC.decode(out);
		assertEquals("", decoded.targetPlayer());
	}

	@Test
	@DisplayName("P2-3: image decode 对超大 dataLength 抛 SkipPacketDecoderException 而非分配崩溃")
	void imageCodecRejectsOversizedDataLength() {
		var out = buf();
		out.writeLong(1L);
		out.writeVarInt(0);           // frameNumber
		out.writeVarInt(0);           // x
		out.writeVarInt(0);           // y
		out.writeVarInt(8192);        // width
		out.writeVarInt(8192);        // height
		out.writeVarInt(500_000_000); // 恶意：5 亿字节声明

		assertThrows(SkipPacketDecoderException.class, () -> SharedWindowImagePayload.CODEC.decode(out));
	}

	@Test
	@DisplayName("P2-3: image decode 对超出可读字节数的长度同样拒绝（防 OOM 分配）")
	void imageCodecRejectsLengthBeyondReadableBytes() {
		var out = buf();
		out.writeLong(1L);
		out.writeVarInt(0);
		out.writeVarInt(0);
		out.writeVarInt(0);
		out.writeVarInt(10);
		out.writeVarInt(10);
		out.writeVarInt(1_000_000); // 声称 1MB，实际后面没数据
		out.writeVarInt(0);         // 少量后续字节

		assertThrows(SkipPacketDecoderException.class, () -> SharedWindowImagePayload.CODEC.decode(out));
	}

	@Test
	@DisplayName("P2-3: image decode 负数长度拒绝")
	void imageCodecRejectsNegativeLength() {
		var out = buf();
		out.writeLong(1L);
		out.writeVarInt(0);
		out.writeVarInt(0);
		out.writeVarInt(0);
		out.writeVarInt(4);
		out.writeVarInt(4);
		out.writeVarInt(-7); // 恶意负数

		assertThrows(SkipPacketDecoderException.class, () -> SharedWindowImagePayload.CODEC.decode(out));
	}

	@Test
	@DisplayName("image codec 合法 round-trip 不受守卫影响（写端行为不变）")
	void imageCodecRoundTripStillWorks() {
		var payload = new SharedWindowImagePayload(
			0xABCDL, 7, 100, 200, 640, 480, new byte[]{1, 2, 3, 4},
			0.5, 0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0,
			1.0, 1280, 720, 200, 123456789L, 42);
		var out = buf();
		SharedWindowImagePayload.CODEC.encode(out, payload);
		var decoded = SharedWindowImagePayload.CODEC.decode(out);
		// record equals 对 byte[] 分量按引用比较，这里逐字段断言
		assertEquals(payload.windowHandle(), decoded.windowHandle());
		assertEquals(payload.frameNumber(), decoded.frameNumber());
		assertEquals(payload.width(), decoded.width());
		assertEquals(payload.height(), decoded.height());
		assertArrayEquals(payload.imageData(), decoded.imageData());
		assertEquals(payload.captureTimestampMs(), decoded.captureTimestampMs());
		assertEquals(payload.frameSeq(), decoded.frameSeq());
	}

	@Test
	@DisplayName("P2-3: audio decode 超过 1MB 声明长度抛 SkipPacketDecoderException")
	void audioCodecRejectsOversizedPcmLength() {
		var out = buf();
		out.writeLong(1L);
		out.writeVarInt(0);      // seq
		out.writeVarInt(48000);  // sampleRate
		out.writeVarInt(2);      // channels
		out.writeVarInt(2_000_000); // 恶意：2MB 声明（decode cap 1MB）

		assertThrows(SkipPacketDecoderException.class, () -> SharedWindowAudioPayload.CODEC.decode(out));
	}

	@Test
	@DisplayName("audio codec 合法 round-trip 不受守卫影响")
	void audioCodecRoundTripStillWorks() {
		var payload = new SharedWindowAudioPayload(0x99L, 5, 48000, 2, new byte[]{9, 8, 7, 6}, 111L);
		var out = buf();
		SharedWindowAudioPayload.CODEC.encode(out, payload);
		var decoded = SharedWindowAudioPayload.CODEC.decode(out);
		assertEquals(payload.windowHandle(), decoded.windowHandle());
		assertEquals(payload.seq(), decoded.seq());
		assertEquals(payload.sampleRate(), decoded.sampleRate());
		assertEquals(payload.channels(), decoded.channels());
		assertArrayEquals(payload.pcmData(), decoded.pcmData());
		assertEquals(payload.captureTimestampMs(), decoded.captureTimestampMs());
	}
}
