package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

import java.util.List;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.shared.WindowShareManager.ShareState;

/**
 * P2-18 纯函数层测试：音频"单例跟随最近窗口"的回落候选选择。
 * 停止被跟随窗口后，应回落到其余仍在共享窗口中 startTime 最新的一个；
 * 没有候选返回 null（停止路径据此提示并保持无音频，而不是指向已停窗口）。
 */
class AudioFollowFallbackTest {

	private static ShareState state(long handle, long startTime) {
		ShareState s = new ShareState(handle, "w" + handle);
		s.startTime = startTime;
		return s;
	}

	@Test
	@DisplayName("排除停止的窗口后取 startTime 最新者")
	void picksMostRecentRemaining() {
		List<ShareState> states = List.of(
			state(1L, 100L),
			state(2L, 300L),
			state(3L, 200L));
		ShareState next = WindowShareManager.pickAudioFallback(states, 2L);
		// 排除 2 号后，1/3 中 3 号最新
		assertEquals(3L, next.windowHandle);
	}

	@Test
	@DisplayName("唯一窗口停止 → null（无候选不指向幽灵窗口）")
	void nullWhenNothingRemains() {
		List<ShareState> states = List.of(state(7L, 100L));
		assertNull(WindowShareManager.pickAudioFallback(states, 7L));
		assertNull(WindowShareManager.pickAudioFallback(List.of(), 1L));
	}

	@Test
	@DisplayName("并列 startTime 稳定返回先遇到者（无随机性）")
	void tieIsStable() {
		List<ShareState> states = List.of(state(4L, 500L), state(5L, 500L));
		assertEquals(4L, WindowShareManager.pickAudioFallback(states, 9L).windowHandle);
	}
}
