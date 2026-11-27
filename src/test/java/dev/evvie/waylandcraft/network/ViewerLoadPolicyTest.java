package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.UUID;

import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * 任务A-2（overload-guard）：观众负载分级纯策略 + 迟滞状态机 + 放行门。
 * 时钟全部显式注入，无 MC 依赖、无真实等待。
 *
 * 采样节奏说明：relay 每 50ms 批次观测一次；测试用 250ms 密集采样保持
 * unwritable 时长占比 ≈ 1.0（与持续不可写的观众一致），稀疏采样下时长会被
 * MAX_SAMPLE_GAP_MS 截断、占比随窗口拉长而稀释——这是设计内行为（单次
 * 长间隔观测不应等价于持续不可写），压力证据靠 episode 事件数兜底。
 */
class ViewerLoadPolicyTest {

	private final UUID viewer = UUID.randomUUID();

	@BeforeEach
	void resetState() {
		ViewerLoadController.clear();
	}

	// ===== targetLevel：观测窗压力 → 目标级别 =====

	@Test
	@DisplayName("无压力 / 观测窗非法 → L0")
	void noPressureStaysNormal() {
		assertEquals(ViewerLoadController.LEVEL_NORMAL, ViewerLoadController.targetLevel(10_000, 0, 0));
		assertEquals(ViewerLoadController.LEVEL_NORMAL, ViewerLoadController.targetLevel(0, 5_000, 9));
		assertEquals(ViewerLoadController.LEVEL_NORMAL, ViewerLoadController.targetLevel(-1, 5_000, 9));
	}

	@Test
	@DisplayName("时长占比阈值：20% → L1，50% → L2，80% → L3（边界含入）")
	void durationFractionThresholds() {
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.targetLevel(10_000, 2_000, 0));
		assertEquals(ViewerLoadController.LEVEL_NORMAL, ViewerLoadController.targetLevel(10_000, 1_999, 0));
		assertEquals(ViewerLoadController.LEVEL_KEYFRAME, ViewerLoadController.targetLevel(10_000, 5_000, 0));
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.targetLevel(10_000, 4_999, 0), "差 1ms 不足 50% 保持 L1");
		assertEquals(ViewerLoadController.LEVEL_PAUSED, ViewerLoadController.targetLevel(10_000, 8_000, 0));
		assertEquals(ViewerLoadController.LEVEL_KEYFRAME, ViewerLoadController.targetLevel(10_000, 7_999, 0));
	}

	@Test
	@DisplayName("事件数阈值：≥2 次 1s episode → L1，≥4 次 → L2；事件与占比同时成立时抬高到其中更高档（L3 仅由占比 80% 触发）")
	void eventCountThresholds() {
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.targetLevel(10_000, 0, 2));
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.targetLevel(10_000, 0, 3));
		assertEquals(ViewerLoadController.LEVEL_KEYFRAME, ViewerLoadController.targetLevel(10_000, 0, 4));
		assertEquals(ViewerLoadController.LEVEL_NORMAL, ViewerLoadController.targetLevel(10_000, 0, 1));
		// 占比 20%（L1 档）+ 事件 9 次（L2 档）→ 取更高档 L2
		assertEquals(ViewerLoadController.LEVEL_KEYFRAME, ViewerLoadController.targetLevel(10_000, 2_000, 9));
	}

	// ===== advance：迟滞状态机 =====

	@Test
	@DisplayName("升级迟滞：目标需持续 DEMOTE_HOLD_MS 才 +1，且逐级不跳级")
	void demoteRequiresHoldAndStepsOneLevel() {
		long hold = ViewerLoadController.DEMOTE_HOLD_MS;

		ViewerLoadController.LevelStep s1 = ViewerLoadController.advance(0, 0, 0, 3, 1_000);
		assertEquals(0, s1.level(), "目标刚出现不升级");
		assertEquals(3, s1.pending());
		assertEquals(1_000, s1.pendingSinceMs());

		assertEquals(0, ViewerLoadController.advance(0, 3, 1_000, 3, 1_000 + hold - 1).level(), "差 1ms 不升级");
		ViewerLoadController.LevelStep s2 = ViewerLoadController.advance(0, 3, 1_000, 3, 1_000 + hold);
		assertEquals(1, s2.level(), "满 hold 升一级（不直接跳到 3）");
		assertEquals(1_000 + hold, s2.pendingSinceMs(), "每级升级都要求重新计满 hold");

		ViewerLoadController.LevelStep s3 = ViewerLoadController.advance(1, 3, 1_000 + hold, 3, 1_000 + 2 * hold);
		assertEquals(2, s3.level(), "第二级同样需要完整 hold");
	}

	@Test
	@DisplayName("目标抖动会重置 pending 计时（抖动凑不满 hold）")
	void targetFlipResetsHoldTimer() {
		assertEquals(0, ViewerLoadController.advance(0, 1, 1_000, 0, 2_000).level(), "目标回落，pending 重置");
		ViewerLoadController.LevelStep s = ViewerLoadController.advance(0, 0, 2_000, 1, 5_000);
		assertEquals(1, s.pending());
		assertEquals(5_000, s.pendingSinceMs(), "pending 变化重置计时起点");
		assertEquals(0, s.level());
	}

	@Test
	@DisplayName("降级迟滞：PROMOTE_HOLD_MS 后直接跳回目标级（尽快恢复体验）")
	void promoteJumpsToTargetAfterHold() {
		long hold = ViewerLoadController.PROMOTE_HOLD_MS;
		assertEquals(2, ViewerLoadController.advance(2, 0, 1_000, 0, 1_000 + hold - 1).level(), "未满 hold 保持降级");
		ViewerLoadController.LevelStep s = ViewerLoadController.advance(2, 0, 1_000, 0, 1_000 + hold);
		assertEquals(0, s.level(), "满 hold 直接跳回目标级");
	}

	// ===== shouldSendVideo / shouldSendAudio：放行门纯函数 =====

	@Test
	@DisplayName("视频放行：L0 全量；L1 隔帧；L2 每 1s 一帧心跳；L3 全停")
	void videoGateByLevel() {
		assertTrue(ViewerLoadController.shouldSendVideo(0, false, 1_000, 0));
		assertTrue(ViewerLoadController.shouldSendVideo(0, true, 1_000, 0));

		assertTrue(ViewerLoadController.shouldSendVideo(1, false, 1_000, 0), "L1 toggle=false → 本帧放");
		assertFalse(ViewerLoadController.shouldSendVideo(1, true, 1_000, 0), "L1 toggle=true → 本帧跳");

		assertFalse(ViewerLoadController.shouldSendVideo(2, false, 500, 0), "L2 心跳间隔未到");
		assertTrue(ViewerLoadController.shouldSendVideo(2, false, ViewerLoadController.KEYFRAME_INTERVAL_MS, 0));
		assertFalse(ViewerLoadController.shouldSendVideo(2, false, ViewerLoadController.KEYFRAME_INTERVAL_MS - 1, 0));

		assertFalse(ViewerLoadController.shouldSendVideo(3, false, 100_000, 0), "L3 暂停");
	}

	@Test
	@DisplayName("音频放行：L0/L1 全量；L2 隔包；L3 全停")
	void audioGateByLevel() {
		assertTrue(ViewerLoadController.shouldSendAudio(0, false));
		assertTrue(ViewerLoadController.shouldSendAudio(0, true));
		assertTrue(ViewerLoadController.shouldSendAudio(1, true), "L1 音频不减（视频才是带宽大头）");
		assertFalse(ViewerLoadController.shouldSendAudio(2, true));
		assertTrue(ViewerLoadController.shouldSendAudio(2, false));
		assertFalse(ViewerLoadController.shouldSendAudio(3, false));
	}

	// ===== 运行时观测（状态机整体行为，时钟注入） =====

	/** 密集采样辅助：每 250ms 观测一次不可写（保持时长占比 ≈ 1.0） */
	private void observeUnwritableDense(UUID id, long fromMs, long toMs, boolean thresholdEvent) {
		ViewerLoadController.observeUnwritable(id, fromMs, thresholdEvent);
		for (long t = fromMs + 250; t <= toMs; t += 250) {
			ViewerLoadController.observeUnwritable(id, t, false);
		}
	}

	@Test
	@DisplayName("持续不可写观众：3s 后降 L1，之后每 3s 逐级到 L3（不跳级）")
	void sustainedPressureDemotesStepwise() {
		observeUnwritableDense(viewer, 0, 3_000, true);
		assertEquals(ViewerLoadController.LEVEL_NORMAL, ViewerLoadController.levelOf(viewer), "差 250ms 未满 3s hold");

		ViewerLoadController.observeUnwritable(viewer, 3_250, false);
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.levelOf(viewer), "满 3s hold 升到 L1");

		observeUnwritableDense(viewer, 3_500, 6_000, false);
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.levelOf(viewer));
		ViewerLoadController.observeUnwritable(viewer, 6_250, false);
		assertEquals(ViewerLoadController.LEVEL_KEYFRAME, ViewerLoadController.levelOf(viewer));

		observeUnwritableDense(viewer, 6_500, 9_000, false);
		assertEquals(ViewerLoadController.LEVEL_KEYFRAME, ViewerLoadController.levelOf(viewer));
		ViewerLoadController.observeUnwritable(viewer, 9_250, false);
		assertEquals(ViewerLoadController.LEVEL_PAUSED, ViewerLoadController.levelOf(viewer), "0→1→2→3 各 3s，逐级不跳级");
	}

	@Test
	@DisplayName("L1 时 trySendVideo 隔帧放行（toggle 每个机会翻转，不卡死）")
	void halfRateGateAlternatesOnRealSends() {
		// 两次 1s episode 事件 + hold 满 → L1（稀疏采样下事件数兜底压力证据）
		ViewerLoadController.observeUnwritable(viewer, 0, true);
		ViewerLoadController.observeUnwritable(viewer, 1_000, true);
		ViewerLoadController.observeUnwritable(viewer, 4_000, false);
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.levelOf(viewer));

		assertTrue(ViewerLoadController.trySendVideo(viewer, 4_001), "首帧放行");
		assertFalse(ViewerLoadController.trySendVideo(viewer, 4_002), "第二帧跳");
		assertTrue(ViewerLoadController.trySendVideo(viewer, 4_003), "第三帧放（隔帧节奏）");
		assertFalse(ViewerLoadController.trySendVideo(viewer, 4_004), "第四帧跳");
		assertTrue(ViewerLoadController.trySendVideo(viewer, 4_005), "节奏持续交替，不会卡死在跳帧态");
	}

	@Test
	@DisplayName("L2 心跳门：每 1s 恰好放行一帧；L3 视频/音频全停")
	void keyframeAndPausedGates() {
		// 密集采样把该观众逐级压到 L2（0→1 @3250，1→2 @6250）
		observeUnwritableDense(viewer, 0, 3_000, true);
		ViewerLoadController.observeUnwritable(viewer, 3_250, false);
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.levelOf(viewer));
		observeUnwritableDense(viewer, 3_500, 6_000, false);
		ViewerLoadController.observeUnwritable(viewer, 6_250, false);
		assertEquals(ViewerLoadController.LEVEL_KEYFRAME, ViewerLoadController.levelOf(viewer));

		assertTrue(ViewerLoadController.trySendVideo(viewer, 6_300), "L2 首帧心跳放行");
		assertFalse(ViewerLoadController.trySendVideo(viewer, 6_400), "间隔内全跳");
		assertFalse(ViewerLoadController.trySendVideo(viewer, 7_299), "差 1ms 不放");
		assertTrue(ViewerLoadController.trySendVideo(viewer, 7_300), "整 1s 放下一帧心跳");

		// 继续压力到 L3
		observeUnwritableDense(viewer, 6_500, 9_000, false);
		ViewerLoadController.observeUnwritable(viewer, 9_250, false);
		assertEquals(ViewerLoadController.LEVEL_PAUSED, ViewerLoadController.levelOf(viewer));
		assertFalse(ViewerLoadController.trySendVideo(viewer, 20_000), "L3 视频全停");
		assertFalse(ViewerLoadController.trySendAudio(viewer), "L3 音频全停");
	}

	@Test
	@DisplayName("音频放行按级：L0/L1 全量；L2 隔包减半")
	void audioGateRuntimeByLevel() {
		ViewerLoadController.observeUnwritable(viewer, 0, true);
		assertTrue(ViewerLoadController.trySendAudio(viewer), "L0 音频全量");

		// 密集采样（占比 ≈ 1.0）把该观众逐级压到 L2
		observeUnwritableDense(viewer, 250, 3_000, false);
		ViewerLoadController.observeUnwritable(viewer, 3_250, false); // 满 hold → L1
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.levelOf(viewer));
		assertTrue(ViewerLoadController.trySendAudio(viewer), "L1 音频不减");

		observeUnwritableDense(viewer, 3_500, 6_000, false);
		ViewerLoadController.observeUnwritable(viewer, 6_250, false); // 再满 hold → L2
		assertEquals(ViewerLoadController.LEVEL_KEYFRAME, ViewerLoadController.levelOf(viewer));
		assertTrue(ViewerLoadController.trySendAudio(viewer), "L2 首包放行");
		assertFalse(ViewerLoadController.trySendAudio(viewer), "L2 第二包跳");
		assertTrue(ViewerLoadController.trySendAudio(viewer), "L2 第三包放（隔包节奏）");
	}

	@Test
	@DisplayName("恢复路径：可写观测推进窗口；PROMOTE_HOLD_MS 后跳回 L0")
	void writableEvidenceRecoversLevel() {
		// 压到 L1（事件数兜底）
		ViewerLoadController.observeUnwritable(viewer, 0, true);
		ViewerLoadController.observeUnwritable(viewer, 1_000, true);
		ViewerLoadController.observeUnwritable(viewer, 4_000, false);
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.levelOf(viewer));

		// 通道恢复：可写观测使观测窗滚动清零（t=10000 时窗龄 ≥10s）
		ViewerLoadController.observeWritable(viewer, 10_000);
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.levelOf(viewer), "恢复迟滞期保持降级");
		ViewerLoadController.observeWritable(viewer, 14_999);
		assertEquals(ViewerLoadController.LEVEL_HALF_RATE, ViewerLoadController.levelOf(viewer), "差 1ms 不恢复");
		ViewerLoadController.observeWritable(viewer, 15_000);
		assertEquals(ViewerLoadController.LEVEL_NORMAL, ViewerLoadController.levelOf(viewer), "满 5s 恢复迟滞跳回 L0");
		assertTrue(ViewerLoadController.trySendVideo(viewer, 15_001), "恢复后全量放行");
		assertTrue(ViewerLoadController.trySendAudio(viewer), "恢复后音频全量");
	}

	@Test
	@DisplayName("无观测记录的观众 fail-open 全量放行；下线清理 + 快照只读")
	void unknownViewerFailsOpenAndCleanupWorks() {
		UUID stranger = UUID.randomUUID();
		assertTrue(ViewerLoadController.trySendVideo(stranger, 1_000));
		assertTrue(ViewerLoadController.trySendAudio(stranger));
		assertEquals(ViewerLoadController.LEVEL_NORMAL, ViewerLoadController.levelOf(stranger));
		assertEquals(0, ViewerLoadController.snapshot().size());

		ViewerLoadController.observeUnwritable(viewer, 0, true);
		assertEquals(1, ViewerLoadController.snapshot().size());
		ViewerLoadController.retainOnly(java.util.Set.of(stranger));
		assertEquals(0, ViewerLoadController.snapshot().size(), "不在场观众状态被清理");
	}
}
