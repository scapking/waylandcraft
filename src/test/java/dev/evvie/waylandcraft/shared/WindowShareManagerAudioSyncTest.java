package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.shared.WindowShareManager.AudioSyncAction;

/**
 * ⑧ v1.4.8 音频意图合并 reconcile 纯函数测试。
 * 红线：/wl share start all 连发 N 个 start 意图，只允许发生**一次**真实
 * PipeWire 会话建立（旧实现 N 次同步"停旧起新"全套阻塞在命令线程，
 * native 预算最坏 10+15s/次 = "瞬间卡一下"的主病灶）。
 */
class WindowShareManagerAudioSyncTest {
	
	@Test
	void staleIntentTasksAreSkipped() {
		// 意图已推进到 3；1/2 的任务迟到执行 → 全部 SKIP，绝不 boot
		assertEquals(AudioSyncAction.SKIP, WindowShareManager.decideAudioSync(3, 1, 0, false));
		assertEquals(AudioSyncAction.SKIP, WindowShareManager.decideAudioSync(3, 2, 0, false));
	}
	
	@Test
	void onlyLatestIntentTaskBootsOnce() {
		// 最新意图任务 START；重复提交（同意图已应用）→ ALREADY 幂等
		assertEquals(AudioSyncAction.START, WindowShareManager.decideAudioSync(3, 3, 0, false));
		assertEquals(AudioSyncAction.ALREADY, WindowShareManager.decideAudioSync(3, 3, 3, true));
	}
	
	@Test
	void stopIntentResolvesToStopOrAlready() {
		assertEquals(AudioSyncAction.STOP, WindowShareManager.decideAudioSync(0, 0, 7, true));
		assertEquals(AudioSyncAction.ALREADY, WindowShareManager.decideAudioSync(0, 0, 0, false));
	}
	
	@Test
	void failedStartRetriesOnNextReconcile() {
		// start 失败后 applied 归 0 且捕获未运行：下一次同意图任务必须重新 START（幂等≠躺平）
		assertEquals(AudioSyncAction.START, WindowShareManager.decideAudioSync(5, 5, 5, false));
	}
}
