package dev.evvie.waylandcraft.localmode;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.localmode.LocalDirectDisplay.DisplayTarget;
import dev.evvie.waylandcraft.localmode.LocalDirectDisplay.InitialPose;

/**
 * 缺陷 C（无 mod 服"有进程没窗口"）的复现与回归锁——纯逻辑层。
 *
 * 核心断言：新建（首个可见帧）的 toplevel 必须**当帧**就出现在显示创建目标列表里。
 * 旧实现把条件唯一押在 isMapped()（buffer attach 事件抵达 Java）上；该信号一经
 * 丢失（事件链/协商噪声半死态），窗口对象明明存在却恒不显示、全程无报错——
 * 正是用户实测"toplevels=2、进程在、世界里没窗口"的形态。
 * 修复：判定放宽为"存活 且（mapped 或 Java 层已有 framebuffer）"，并要求
 * updateWorld 用全量 toplevel 枚举（而非 mapped-only 快照）来做本地直显判定。
 */
class LocalDirectDisplayTest {

	private static DisplayTarget target(long h, boolean alive, boolean mapped, boolean fb, boolean shown, boolean hidden) {
		return new DisplayTarget(h, alive, mapped, fb, shown, hidden);
	}

	@Test
	@DisplayName("复现回归核心：新建的已映射 toplevel 在当次判定中必须进入显示目标列表")
	void newMappedToplevelIsTargetedSameCall() {
		DisplayTarget[] all = { target(0x1111, true, true, false, false, false) };
		List<DisplayTarget> targets = LocalDirectDisplay.autoPlaceTargets(all);
		assertEquals(1, targets.size(), "新窗口不得缺席显示列表");
		assertEquals(0x1111, targets.get(0).handle());
	}

	@Test
	@DisplayName("attach 信号丢失半死态兜底：未 mapped 但 Java 已有 framebuffer 的窗口同样显示（旧实现在此恒不显示=回归本体）")
	void framebufferOnlyWindowStillShown() {
		DisplayTarget lost = target(0x2222, true, false, true, false, false);
		assertTrue(LocalDirectDisplay.shouldCreateDisplay(lost),
			"buffer attach 丢失不得静默隐藏窗口——必须走 framebuffer 兜底信号");
	}

	@Test
	@DisplayName("真·未映射（无 buffer 无 framebuffer）不显示：窗口还没画出任何内容")
	void completelyBlankWindowNotShown() {
		assertFalse(LocalDirectDisplay.shouldCreateDisplay(target(3, true, false, false, false, false)));
	}

	@Test
	@DisplayName("已显示/已隐藏/已消亡三类必须跳过：不重复建、不复活、不残留")
	void skipsShownHiddenDead() {
		assertFalse(LocalDirectDisplay.shouldCreateDisplay(target(4, true, true, true, true, false)), "已有显示不得重复建");
		assertFalse(LocalDirectDisplay.shouldCreateDisplay(target(5, true, true, false, false, true)), "/wl hide 的窗口不得被自动摆位复活（旧实现会下一帧重建=hide 失效）");
		assertFalse(LocalDirectDisplay.shouldCreateDisplay(target(6, false, true, true, false, false)), "消亡窗口不得建显示");
	}

	@Test
	@DisplayName("autoPlaceTargets 保持 bridge 枚举顺序（首窗口=核心锚的语义稳定）")
	void targetsPreserveEnumerationOrder() {
		DisplayTarget[] all = {
			target(0xA, true, false, false, true, false),   // 已显示：跳过
			target(0xB, true, true, false, false, false),   // 新：进入
			target(0xC, true, false, true, false, false),   // 兜底：进入
			target(0xD, false, true, true, false, false),   // 死：跳过
		};
		List<DisplayTarget> out = LocalDirectDisplay.autoPlaceTargets(all);
		assertEquals(List.of(0xBL, 0xCL), out.stream().map(DisplayTarget::handle).toList());
	}

	@Test
	@DisplayName("需求 A：布局活跃时首帧姿态交给布局（与物品窗口同路径），记忆摆位让位")
	void layoutTakesPriority() {
		assertEquals(InitialPose.LAYOUT, LocalDirectDisplay.chooseInitialPose(true, true));
		assertEquals(InitialPose.LAYOUT, LocalDirectDisplay.chooseInitialPose(true, false));
	}

	@Test
	@DisplayName("需求 A：布局不活跃时记忆摆位优先于面前默认位")
	void rememberedBeatsCameraFront() {
		assertEquals(InitialPose.REMEMBERED, LocalDirectDisplay.chooseInitialPose(false, true));
		assertEquals(InitialPose.CAMERA_FRONT, LocalDirectDisplay.chooseInitialPose(false, false));
	}

	@Test
	@DisplayName("place 距离默认 2.0 并钳制在 1-6（与命令注册参数一致，越界值不得穿透）")
	void placeDistanceClamped() {
		assertEquals(2.0, LocalDirectDisplay.DEFAULT_PLACE_DISTANCE);
		assertEquals(1.0, LocalDirectDisplay.clampPlaceDistance(0.3));
		assertEquals(1.0, LocalDirectDisplay.clampPlaceDistance(1.0));
		assertEquals(6.0, LocalDirectDisplay.clampPlaceDistance(99));
		assertEquals(2.0, LocalDirectDisplay.clampPlaceDistance(2.0));
	}
}
