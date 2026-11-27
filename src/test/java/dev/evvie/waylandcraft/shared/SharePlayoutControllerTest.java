package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

/**
 * ⑧ v1.4.8 SharePlayoutController 纯函数/时钟测试。
 * 语义锚点（对应用户实测三条）：
 * - 共享 playout 时钟：视频+音频 transit 样本喂同一估计（音画同锚 → 消除恒定错拍）；
 * - 自适应深度：拥塞证据连续才升（+2s/步，封顶 32s），30s 干净才降（−1s/步，封底 14s），
 *   驻留期防振荡；
 * - 容量配套：缓冲上限随目标深度与实测码率伸缩（固定 48MB 会把深缓冲钳回浅=自伤）；
 * - 手动档兼容：0=低延迟、&gt;0=手动 语义原样。
 */
class SharePlayoutControllerTest {
	
	@BeforeEach
	void reset() {
		SharePlayoutController.resetForTest();
	}
	
	// ===== 深度解析（settings 语义） =====
	
	@Test
	void manualAndLowLatencySemanticsPreserved() {
		assertEquals(0, SharePlayoutController.resolveDelayMs(0));
		assertEquals(5000, SharePlayoutController.resolveDelayMs(5000));
		assertEquals(32000, SharePlayoutController.resolveDelayMs(32000));
	}
	
	@Test
	void autoResolvesToAdaptiveTargetWithinRange() {
		long d = SharePlayoutController.resolveDelayMs(SharePlayoutController.AUTO);
		assertEquals(14_000, d, "初始自适应目标必须是 14s（用户裁定下限）");
		assertTrue(d >= SharePlayoutController.AUTO_MIN_MS && d <= SharePlayoutController.AUTO_MAX_MS);
	}
	
	@Test
	void corruptNegativeFallsBackToAutoStart() {
		// 设置文件被外部改成非法 -5：保守回 14s（绝不回 0 造成卡顿回归）
		assertEquals(14_000, SharePlayoutController.resolveDelayMs(-5));
	}
	
	// ===== 共享 playout 时钟 =====
	
	@Test
	void clockIsSharedAcrossTracks() {
		// 视频先到 transit=300（含编码），音频后到 transit=50：min-tracking 让
		// 两轨共用同一个下界——音频样本把估计拉低，视频排期同步提前（同锚核心）
		SharePlayoutController.observeTransit(7L, 300, 1000);
		assertEquals(300, SharePlayoutController.transitMs(7L));
		SharePlayoutController.observeTransit(7L, 50, 1100);
		assertEquals(50, SharePlayoutController.transitMs(7L), "共享估计必须被音频样本立即拉低（min 快速下收敛）");
	}
	
	@Test
	void clockUpwardDriftIsRateLimited() {
		SharePlayoutController.observeTransit(8L, 100, 1000);
		// 单样本尖峰 5000：只允许 +3ms 上漂（抗中继批量到达把两轨一起推远）
		SharePlayoutController.observeTransit(8L, 5000, 1100);
		assertEquals(103, SharePlayoutController.transitMs(8L));
	}
	
	@Test
	void unknownWindowHasNoClockYet() {
		assertEquals(-1, SharePlayoutController.transitMs(999L));
	}
	
	@Test
	void forgetWindowResetsEstimate() {
		SharePlayoutController.observeTransit(9L, 200, 1000);
		SharePlayoutController.forgetWindow(9L);
		assertEquals(-1, SharePlayoutController.transitMs(9L));
	}
	
	@Test
	void expiredClocksSwept() {
		SharePlayoutController.observeTransit(10L, 100, 1000);
		SharePlayoutController.observeTransit(11L, 100, 5_000L);
		int removed = SharePlayoutController.sweepExpiredClocks(5_000L + 60_001L);
		assertEquals(2, removed, "空闲超过 60s 的时钟必须回收（多窗口长会话防泄漏）");
	}
	
	// ===== 自适应状态机（迟滞） =====
	
	private static SharePlayoutController.ControllerState s(long target, int streak, long goodSince, long holdUntil) {
		return new SharePlayoutController.ControllerState(target, streak, goodSince, holdUntil);
	}
	
	@Test
	void congestionStreakRaisesTargetStepwise() {
		SharePlayoutController.ControllerState st = s(14_000, 0, 0, 0);
		st = SharePlayoutController.nextState(st, 0.9, 1000);   // streak 1
		assertEquals(14_000, st.targetMs(), "单次拥塞不升（连续判据）");
		st = SharePlayoutController.nextState(st, 0.9, 2000);   // streak 2
		st = SharePlayoutController.nextState(st, 0.9, 3000);   // streak 3 → 升
		assertEquals(16_000, st.targetMs(), "连续 3 采样拥塞 → +2s");
	}
	
	@Test
	void holdWindowPreventsOscillationAfterRaise() {
		SharePlayoutController.ControllerState st = s(14_000, 2, 2000, 0);
		st = SharePlayoutController.nextState(st, 0.9, 3000);   // 升档 → hold 13s
		assertEquals(16_000, st.targetMs());
		SharePlayoutController.ControllerState again = SharePlayoutController.nextState(st, 0.9, 4000);
		assertEquals(16_000, again.targetMs(), "驻留期内不得连升（防一步到位的振荡）");
		again = SharePlayoutController.nextState(again, 0.9, 12_000);  // 连续 3 次：4000..12000
		// 4000,5000,6000 连击（每次 streak 递增，到 3 且过 hold 才升）——12000 时 hold 未到(13000)
		// 允许 4000 起 streak≥3 但 now<hold → 不升。到 13000 才升 18000。
		// 这里断言行为定义：hold(13000) 之前不升
		assertTrue(again.targetMs() <= 16_000);
	}
	
	@Test
	void raiseIsClampedAtMax() {
		SharePlayoutController.ControllerState st = s(30_000, 2, 0, 0);
		st = SharePlayoutController.nextState(st, 0.9, 1000);
		assertEquals(32_000, st.targetMs());
		st = SharePlayoutController.nextState(s(32_000, 2, 0, 0), 0.9, 1000);
		assertEquals(32_000, st.targetMs(), "封顶 32s（用户裁定上限）");
	}
	
	@Test
	void quietPeriodLowersSlowlyAndFloorsAtMin() {
		SharePlayoutController.ControllerState st = s(20_000, 0, 0, 0);
		st = SharePlayoutController.nextState(st, 0.0, 1000);          // goodSince 建立
		assertEquals(20_000, st.targetMs(), "刚干净不许立刻降");
		st = SharePlayoutController.nextState(st, 0.0, 1000 + 31_000); // 连续 30s 干净
		assertEquals(19_000, st.targetMs(), "30s 干净 → −1s 缓降");
		st = SharePlayoutController.nextState(s(14_000, 0, 0, 0), 0.0, 200_000);
		assertEquals(14_000, st.targetMs(), "封底 14s（绝不降回旧的 4–8s 档）");
	}
	
	@Test
	void intermittentCongestionBreaksQuietCounter() {
		SharePlayoutController.ControllerState st = s(20_000, 0, 0, 0);
		st = SharePlayoutController.nextState(st, 0.0, 1000);
		st = SharePlayoutController.nextState(st, 0.4, 20_000);  // 一次拥塞打断"连续"
		st = SharePlayoutController.nextState(st, 0.0, 50_000);  // 距 goodSince(20000) 已 30s？
		// 拥塞样本把 goodSince 重置到 20000 → 50000-20000=30000 ≥ 30000 → 允许降
		assertEquals(19_000, st.targetMs());
	}
	
	@Test
	void sampleApiDrivesStateAndSweeps() {
		SharePlayoutController.observeTransit(42L, 100, 500);
		// 连续 3 个拥塞采样 → 升
		for(int i = 0; i < 3; i++) {
			SharePlayoutController.sample(1000 + i * 1000L,
				new SharePlayoutController.PlayoutMetrics(5, 0, 0, 0.5));
		}
		assertEquals(16_000, SharePlayoutController.targetMs());
	}
	
	// ===== 拥塞分数 =====
	
	@Test
	void congestionScorePrioritizesHardEvidence() {
		// 容量截断丢帧：缓冲被钳浅的最硬证据
		assertEquals(0.9, SharePlayoutController.congestionScore(
			new SharePlayoutController.PlayoutMetrics(1, 0, 0, 0.0)), 1e-9);
		// 解码卡死与音频迟到同级
		assertEquals(0.6, SharePlayoutController.congestionScore(
			new SharePlayoutController.PlayoutMetrics(0, 1, 0, 0.0)), 1e-9);
		assertEquals(0.6, SharePlayoutController.congestionScore(
			new SharePlayoutController.PlayoutMetrics(0, 0, 3, 0.0)), 1e-9);
		// 空闲：0
		assertEquals(0.0, SharePlayoutController.congestionScore(
			new SharePlayoutController.PlayoutMetrics(0, 0, 0, 0.2)), 1e-9);
	}
	
	// ===== 容量公式 =====
	
	@Test
	void videoCapFloorAtLowRate() {
		assertEquals(SharePlayoutController.VIDEO_CAP_FLOOR_BYTES,
			SharePlayoutController.videoBufferCapBytes(14_000, 0));
		// 100KB/s × 14s × 2.5 = 3.5MB < 48MB 地板
		assertEquals(48L * 1024 * 1024, SharePlayoutController.videoBufferCapBytes(14_000, 100_000));
	}
	
	@Test
	void videoCapScalesWithDepthAndRate() {
		// 2,000,000 B/s × 32s × 2.5 = 160,000,000 B（< 192MiB 天花板；公式按十进制字节）
		assertEquals(160_000_000L,
			SharePlayoutController.videoBufferCapBytes(32_000, 2_000_000), "容量必须随深度×码率伸缩");
	}
	
	@Test
	void videoCapCeilsAtHighRate() {
		// 10MB/s × 32s × 2.5 = 800MB → 钳 192MB
		assertEquals(SharePlayoutController.VIDEO_CAP_CEIL_BYTES,
			SharePlayoutController.videoBufferCapBytes(32_000, 10_000_000));
	}
	
	@Test
	void audioPendingCapGrowsWithDepth() {
		assertEquals(300, SharePlayoutController.audioPendingChunks(14_000));
		assertEquals(360, SharePlayoutController.audioPendingChunks(32_000),
			"32s 深度必须配 ≥320 包（旧固定 300≈30s 会钳死最深档）");
		assertEquals(400, SharePlayoutController.audioPendingChunks(360_000));
	}
}
