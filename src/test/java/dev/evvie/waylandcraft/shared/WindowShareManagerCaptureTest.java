package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.util.ArrayList;
import java.util.List;

import org.junit.jupiter.api.Test;

/**
 * ⑧ v1.4.8 发送端纯函数测试：首帧风暴预算（pickCaptureOrder）。
 * 语义锚点：/wl share start all 后 N 个新窗口不得同 tick 全部放行首帧
 * （旧路径每窗首帧 2–3 次全帧同步回读 + 渲染线程编码 = "瞬间卡一下"），
 * 预算只削新窗口，稳态老窗口零影响。
 */
class WindowShareManagerCaptureTest {
	
	private static WindowShareManager.ShareState state(long handle, long frameCount) {
		WindowShareManager.ShareState s = new WindowShareManager.ShareState(handle, "w" + handle);
		s.frameCount = frameCount;
		return s;
	}
	
	@Test
	void firstFramesAreBudgeted() {
		List<WindowShareManager.ShareState> states = new ArrayList<>();
		for(long i = 1; i <= 8; i++) states.add(state(i, 0));
		List<WindowShareManager.ShareState> picked =
			WindowShareManager.pickCaptureOrder(states, WindowShareManager.FIRST_FRAME_CAPTURES_PER_TICK);
		assertEquals(WindowShareManager.FIRST_FRAME_CAPTURES_PER_TICK, picked.size(),
			"8 个首帧窗只放行预算个（其余顺延后续 tick）");
	}
	
	@Test
	void steadyWindowsAreNeverGated() {
		List<WindowShareManager.ShareState> states = new ArrayList<>();
		for(long i = 1; i <= 8; i++) states.add(state(i, 100));
		List<WindowShareManager.ShareState> picked = WindowShareManager.pickCaptureOrder(states, 1);
		assertEquals(8, picked.size(), "稳态窗口（frameCount>0）不受首帧预算影响——不牵扯现有行为");
	}
	
	@Test
	void mixedStatesConsumeBudgetOnlyByNew() {
		List<WindowShareManager.ShareState> states = new ArrayList<>();
		states.add(state(1, 5));   // 老
		states.add(state(2, 0));   // 新1
		states.add(state(3, 0));   // 新2
		states.add(state(4, 0));   // 新3（超预算）
		states.add(state(5, 7));   // 老
		List<WindowShareManager.ShareState> picked = WindowShareManager.pickCaptureOrder(states, 2);
		assertEquals(4, picked.size());
		assertEquals(5, picked.get(3).windowHandle, "被跳过的必须恰好是超预算的新窗 0x4");
	}
	
	@Test
	void nullEntriesAreSkippedDefensively() {
		List<WindowShareManager.ShareState> states = new ArrayList<>();
		states.add(null);
		states.add(state(9, 3));
		assertEquals(1, WindowShareManager.pickCaptureOrder(states, 2).size());
	}
	
	@Test
	void zeroBudgetAllowsOnlySteady() {
		List<WindowShareManager.ShareState> states = new ArrayList<>();
		states.add(state(1, 0));
		states.add(state(2, 4));
		List<WindowShareManager.ShareState> picked = WindowShareManager.pickCaptureOrder(states, 0);
		assertEquals(1, picked.size());
		assertEquals(2, picked.get(0).windowHandle);
	}
}
