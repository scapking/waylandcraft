package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.network.SharedWindowInteractionPayload.InteractionType;

/**
 * ⑨ v1.4.9 观看端交互 WAYLAND 注入分支纯函数测试（批 3：远端能做本地能做的一切）。
 * 语义锚点：
 * - surfaceCoord：viewer 像素（origin+fb偏移口径）→ root-surface 相对坐标
 *   （payload − fbOff + geomOff − rootSub）；四个偏移全 0 时恒等（绝大多数窗口）；
 * - glfwKeyToScancode：GLFW keycode → scancode（bridge 键盘链路参数），未知键兜底原值；
 * - KEY_REPEAT(6)：新交互类型 id 可解析，既有 id 不变（协议兼容红线）。
 */
class InteractionWaylandInjectTest {
	
	@Test
	void zeroOffsetsIsIdentity() {
		assertEquals(120.0, SharedWindowClientHandler.surfaceCoord(120, 0, 0, 0), 1e-9,
			"无 CSD/无偏移窗口：viewer 像素坐标直接可用");
	}
	
	@Test
	void offsetCompositionMatchesLocalMath() {
		// payload = geometryLocal + fbOff；surface = geometryLocal + geomOff − rootSub
		// 例：geometryLocal=100, fbOff=8, geomOff=12, rootSub=0 → payload=108 → surface=112
		assertEquals(112.0, SharedWindowClientHandler.surfaceCoord(108, 8, 12, 0), 1e-9);
		// root surface 自身有 subpos（罕见但合法）→ 再减
		assertEquals(110.5, SharedWindowClientHandler.surfaceCoord(108, 8, 12, 1.5), 1e-9);
	}
	
	@Test
	void negativeGeometryOffsetHandled() {
		// CSD 阴影窗口：geometry.x 常为负（阴影区）
		assertEquals(88.0, SharedWindowClientHandler.surfaceCoord(100, 0, -12, 0), 1e-9);
	}
	
	@Test
	void scancodeResolvedAndFallbackSafe() {
		// headless 环境 GLFW scancode 表可能为空（返回 0/-1）：断言不依赖真实表——
		// 只验证函数整体行为：合法键结果非负且幂等；未知键兜底原值
		int a = SharedWindowClientHandler.glfwKeyToScancode(org.lwjgl.glfw.GLFW.GLFW_KEY_A);
		assertTrue(a >= 0, "合法键不得产生负 scancode（兜底链路保证）");
		assertEquals(SharedWindowClientHandler.glfwKeyToScancode(org.lwjgl.glfw.GLFW.GLFW_KEY_SPACE),
			SharedWindowClientHandler.glfwKeyToScancode(org.lwjgl.glfw.GLFW.GLFW_KEY_SPACE),
			"同键同码（幂等）");
		// 未知/非法键兜底原值（旧桥接容错行为）
		assertEquals(-1, SharedWindowClientHandler.glfwKeyToScancode(-1));
	}
	
	@Test
	void repeatTypeParsableAndLegacyIdsStable() {
		assertEquals(InteractionType.KEY_REPEAT, InteractionType.fromId(6));
		// 既有 id 红线：旧包语义不得漂移
		assertEquals(InteractionType.MOUSE_MOVE, InteractionType.fromId(0));
		assertEquals(InteractionType.MOUSE_CLICK, InteractionType.fromId(1));
		assertEquals(InteractionType.MOUSE_RELEASE, InteractionType.fromId(2));
		assertEquals(InteractionType.KEY_PRESS, InteractionType.fromId(3));
		assertEquals(InteractionType.KEY_RELEASE, InteractionType.fromId(4));
		assertEquals(InteractionType.SCROLL, InteractionType.fromId(5));
		// 未知 id 回落（旧 owner 收到新类型安全忽略）
		assertEquals(InteractionType.MOUSE_MOVE, InteractionType.fromId(99));
	}
}
