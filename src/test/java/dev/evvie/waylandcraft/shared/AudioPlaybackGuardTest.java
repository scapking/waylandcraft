package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

/**
 * ⑨ v1.4.9 接收端音频防线纯函数测试。
 * 红线（Other AGENT 发现 + 用户实测）：
 * - distanceGain：range&lt;near 时旧式 (dist-near)/(range-near) 分母为负 →
 *   越远增益越大（反向放大爆音）——修复后 range≤near 必须恒 1.0；
 * - validateAudioPacket：异常/恶意服务端的畸形包（空包/超采率/坏声道/
 *   超长/非帧对齐）必须在进 OpenAL 前拦下。
 */
class AudioPlaybackGuardTest {
	
	@Test
	void fullGainInsideNearZone() {
		assertEquals(1.0f, AudioPlaybackManager.distanceGain(0, 32), 1e-6);
		assertEquals(1.0f, AudioPlaybackManager.distanceGain(2, 32), 1e-6, "near 边界恒满量");
	}
	
	@Test
	void linearFalloffBetweenNearAndRange() {
		// dist=17, range=32, near=2 → 1 - 15/30 = 0.5
		assertEquals(0.5f, AudioPlaybackManager.distanceGain(17, 32), 1e-4);
		// range 边界衰减到 0
		assertEquals(0.0f, AudioPlaybackManager.distanceGain(32, 32), 1e-6);
		// 超出 range 不得为负
		assertEquals(0.0f, AudioPlaybackManager.distanceGain(100, 32), 1e-6);
	}
	
	@Test
	void disabledRangeIsAlwaysFullGain() {
		assertEquals(1.0f, AudioPlaybackManager.distanceGain(999, 0), 1e-6, "range=0=旧全局广播行为");
		assertEquals(1.0f, AudioPlaybackManager.distanceGain(999, -5), 1e-6);
	}
	
	@Test
	void rangeBelowNearMustNotAmplifyWithDistance() {
		// 🔴 反向增益红线：range=1 < near=2 → 分母为负，旧式公式 dist=8 时
		// gain = 1 - 6/(-1) = 7（越远越响）。修复后必须恒 1.0
		assertEquals(1.0f, AudioPlaybackManager.distanceGain(8, 1), 1e-6);
		assertEquals(1.0f, AudioPlaybackManager.distanceGain(2.1, 1.9), 1e-6);
		assertEquals(1.0f, AudioPlaybackManager.distanceGain(100, 2), 1e-6, "range==near 也要防除零");
	}
	
	// ===== 入参校验 =====
	
	@Test
	void validPacketPasses() {
		assertNull(AudioPlaybackManager.validateAudioPacket(48000, 2, new byte[19200]));
		assertNull(AudioPlaybackManager.validateAudioPacket(44100, 1, new byte[8820]));
	}
	
	@Test
	void malformedPacketsRejected() {
		assertTrue(AudioPlaybackManager.validateAudioPacket(48000, 2, null) != null, "空包");
		assertTrue(AudioPlaybackManager.validateAudioPacket(48000, 2, new byte[0]) != null, "零长包");
		assertTrue(AudioPlaybackManager.validateAudioPacket(1000, 2, new byte[19200]) != null, "采样率过低");
		assertTrue(AudioPlaybackManager.validateAudioPacket(999_000, 2, new byte[19200]) != null, "采样率过高");
		assertTrue(AudioPlaybackManager.validateAudioPacket(48000, 0, new byte[19200]) != null, "声道 0");
		assertTrue(AudioPlaybackManager.validateAudioPacket(48000, 7, new byte[19200]) != null, "声道 7");
		assertTrue(AudioPlaybackManager.validateAudioPacket(48000, 2, new byte[19201]) != null, "非帧对齐（4 字节帧单位）");
		assertTrue(AudioPlaybackManager.validateAudioPacket(48000, 2, new byte[128 * 1024]) != null, "超过 64KB 协议上限");
	}
	
	@Test
	void monoFrameAlignmentChecked() {
		// 单声道帧单位 2 字节：奇数长度必须拒绝
		assertTrue(AudioPlaybackManager.validateAudioPacket(48000, 1, new byte[3]) != null);
		assertNull(AudioPlaybackManager.validateAudioPacket(48000, 1, new byte[2]));
	}
}
