package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P2-1/P2-2/P2-3 修复证明：转发入口的帧/音频语义校验（纯判定函数）。
 * - 图像：w/h ∈ [1, 8192]，字节数 ∈ [0, 1.9MB]，超限入口即丢（不进缓存）。
 * - 音频：pcm ∈ (0, 64KB]，sampleRate ∈ [8000, 192000]，channels ∈ {1, 2}。
 */
class FrameAudioIntakeTest {

	@Test
	@DisplayName("isFrameAcceptable: 合法边界 (1..8192, ≤1.9MB) 放行")
	void frameAcceptsLegalBoundaries() {
		assertTrue(SharedWindowFrameRelay.isFrameAcceptable(1, 1, 1));
		assertTrue(SharedWindowFrameRelay.isFrameAcceptable(8192, 8192, SharedWindowFrameRelay.MAX_FRAME_BYTES));
		assertTrue(SharedWindowFrameRelay.isFrameAcceptable(1920, 1080, 1_000_000));
	}

	@Test
	@DisplayName("isFrameAcceptable: 负值/零/超限尺寸与超大字节拒绝")
	void frameRejectsNegativeOversizeAndHugeBytes() {
		assertFalse(SharedWindowFrameRelay.isFrameAcceptable(0, 100, 10), "w=0");
		assertFalse(SharedWindowFrameRelay.isFrameAcceptable(100, -1, 10), "h 负值");
		assertFalse(SharedWindowFrameRelay.isFrameAcceptable(8193, 100, 10), "w 超上限");
		assertFalse(SharedWindowFrameRelay.isFrameAcceptable(100, 8193, 10), "h 超上限");
		assertFalse(SharedWindowFrameRelay.isFrameAcceptable(100, 100, SharedWindowFrameRelay.MAX_FRAME_BYTES + 1), "超大帧");
		assertFalse(SharedWindowFrameRelay.isFrameAcceptable(100, 100, -1), "负长度");
		assertFalse(SharedWindowFrameRelay.isFrameAcceptable(Integer.MIN_VALUE, 1, 1), "极端负尺寸");
		assertFalse(SharedWindowFrameRelay.isFrameAcceptable(1, 1, Integer.MAX_VALUE), "极端大长度");
	}

	@Test
	@DisplayName("isAudioPayloadAcceptable: 合法 PCM 参数放行")
	void audioAcceptsLegalParameters() {
		assertTrue(AudioFrameRelay.isAudioPayloadAcceptable(48000, 2, 30 * 1024));
		assertTrue(AudioFrameRelay.isAudioPayloadAcceptable(8000, 1, 1));
		assertTrue(AudioFrameRelay.isAudioPayloadAcceptable(192000, 2, 64 * 1024), "边界值");
	}

	@Test
	@DisplayName("isAudioPayloadAcceptable: 超限/零值/坏声道拒绝（P2-2 零校验修复）")
	void audioRejectsIllegalParameters() {
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(48000, 2, 64 * 1024 + 1), ">64KB 丢弃");
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(48000, 2, 0), "空包");
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(48000, 2, -5), "负长度");
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(0, 2, 100), "sampleRate=0（客户端 OpenAL 会崩）");
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(-48000, 2, 100), "负采样率");
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(7999, 2, 100), "采样率低于下界");
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(192001, 2, 100), "采样率超上界");
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(48000, 0, 100), "channels=0");
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(48000, 3, 100), "channels=3 非 PCM 立体声");
		assertFalse(AudioFrameRelay.isAudioPayloadAcceptable(48000, -1, 100), "负声道");
	}
}
