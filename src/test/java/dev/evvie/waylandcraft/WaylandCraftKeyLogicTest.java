package dev.evvie.waylandcraft;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.WaylandCraft.KeyboardCaptureMode;

/**
 * P1-1 修复证明（纯函数，不依赖 MC/GLFW 运行时）：
 * J 键事件的"本地消费 vs 转发"决策矩阵。
 *
 * 核心回归用例：CAPTURE（G 模式）下 J 的 RELEASE 必须**不被吞**——
 * 旧实现 `action == 0 return true` 对所有模式生效，而 CAPTURE 下 press
 * 已被转发给远端窗口，release 被吞导致远端窗口永远维持 J 按住（卡键）。
 *
 * GLFW action 常量：0=RELEASE, 1=PRESS, 2=REPEAT。
 */
class WaylandCraftKeyLogicTest {

	private static final int RELEASE = 0;
	private static final int PRESS = 1;
	private static final int REPEAT = 2;

	@Test
	@DisplayName("P1-1 核心修复：CAPTURE 模式下 J 的 press/repeat/release 全部转发（不本地消费）")
	void captureModeForwardsAllJActions() {
		assertFalse(WaylandCraft.consumeJKeyLocally(KeyboardCaptureMode.CAPTURE, PRESS),
			"G 模式下 J 是窗口普通键，press 必须转发");
		assertFalse(WaylandCraft.consumeJKeyLocally(KeyboardCaptureMode.CAPTURE, REPEAT));
		assertFalse(WaylandCraft.consumeJKeyLocally(KeyboardCaptureMode.CAPTURE, RELEASE),
			"release 被吞 = 远端卡键（本次修复的回归用例）");
	}

	@Test
	@DisplayName("NONE 模式下 J 全部本地消费（press 触发绑定；release 无对应转发 press，吞掉防孤儿 release）")
	void noneModeConsumesAll() {
		assertTrue(WaylandCraft.consumeJKeyLocally(KeyboardCaptureMode.NONE, PRESS));
		assertTrue(WaylandCraft.consumeJKeyLocally(KeyboardCaptureMode.NONE, REPEAT));
		assertTrue(WaylandCraft.consumeJKeyLocally(KeyboardCaptureMode.NONE, RELEASE));
	}

	@Test
	@DisplayName("HARD_CAPTURE 模式下 J 全部本地消费（J 是控制键，press 从未转发给窗口）")
	void hardCaptureConsumesAll() {
		assertTrue(WaylandCraft.consumeJKeyLocally(KeyboardCaptureMode.HARD_CAPTURE, PRESS));
		assertTrue(WaylandCraft.consumeJKeyLocally(KeyboardCaptureMode.HARD_CAPTURE, REPEAT));
		assertTrue(WaylandCraft.consumeJKeyLocally(KeyboardCaptureMode.HARD_CAPTURE, RELEASE));
	}
}
