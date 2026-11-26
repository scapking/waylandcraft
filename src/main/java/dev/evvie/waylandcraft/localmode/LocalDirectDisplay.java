package dev.evvie.waylandcraft.localmode;

import java.util.ArrayList;
import java.util.List;

/**
 * 本地直显模式的显示目标判定（纯函数，无 MC/bridge 依赖，可单测）。
 *
 * 回归锁定（缺陷 C）：无 mod 服"有进程没窗口"的失效面是显示创建**唯一**依赖
 * isMapped()（buffer attach 事件必须抵达 Java）。该信号一旦因事件链路/协商
 * 状态丢失，Java 侧永远不建显示且无任何报错。修复：显示条件放宽为
 * "存活 且（已映射 或 Java 层已有可渲染 framebuffer）"，并保证新建 toplevel
 * 在判定发生的当帧即进入显示列表（测试 localDirectNewToplevelIsShownSameFrame）。
 */
public final class LocalDirectDisplay {

	private LocalDirectDisplay() {}

	/**
	 * 一个 toplevel 在本帧的显示判定原料。
	 * @param handle            窗口句柄
	 * @param alive             Java 侧窗口对象仍有效
	 * @param mapped            buffer attach 已抵达（isMapped 语义）
	 * @param framebufferReady  Java 层已有非零尺寸 framebuffer（attach 丢失时的兜底信号）
	 * @param alreadyDisplayed  已存在 WindowDisplay
	 * @param hiddenByUser      用户通过 /wl hide 主动隐藏（本地直显下必须尊重，否则下帧被自动重建）
	 */
	public record DisplayTarget(long handle, boolean alive, boolean mapped, boolean framebufferReady,
			boolean alreadyDisplayed, boolean hiddenByUser) {}

	/** 本帧是否应为该窗口创建显示。 */
	public static boolean shouldCreateDisplay(DisplayTarget t) {
		if(!t.alive()) return false;
		if(t.alreadyDisplayed()) return false;
		if(t.hiddenByUser()) return false;
		// 双信号：mapped 是标准路径；framebufferReady 兜住 attach 事件丢失的半死态，
		// 使"有窗口对象但恒不显示"不再可能静默发生。
		return t.mapped() || t.framebufferReady();
	}

	/** 枚举顺序过滤出本帧应新建显示的窗口（保持 bridge 枚举顺序=创建顺序，核心锚定语义稳定）。 */
	public static List<DisplayTarget> autoPlaceTargets(DisplayTarget[] all) {
		List<DisplayTarget> out = new ArrayList<DisplayTarget>(all.length);
		for(DisplayTarget t : all) {
			if(shouldCreateDisplay(t)) out.add(t);
		}
		return out;
	}

	/** 新显示的首帧姿态来源。 */
	public enum InitialPose {
		/** 布局开启且该窗口参与布局：摆到布局中心附近，下一 tick 由 WindowLayoutManager 精排（布局优先）。 */
		LAYOUT,
		/** 布局不活跃且该 (服务器, 窗口别名) 有记忆位置：直接回位。 */
		REMEMBERED,
		/** 无布局无记忆：玩家面前默认距离摆放（旧行为）。 */
		CAMERA_FRONT
	}

	/**
	 * 需求 A：布局优先。布局参与时新窗口交给布局槽位系统（与物品窗口同一条
	 * layoutManager.tick 路径）；/wl place 仅是临时偏移，下一次布局重排会覆盖。
	 */
	public static InitialPose chooseInitialPose(boolean layoutActive, boolean hasRememberedPlacement) {
		if(layoutActive) return InitialPose.LAYOUT;
		if(hasRememberedPlacement) return InitialPose.REMEMBERED;
		return InitialPose.CAMERA_FRONT;
	}

	/** place 距离档位：默认 2.0，允许 1-6 格（与命令注册参数一致）。 */
	public static final double DEFAULT_PLACE_DISTANCE = 2.0;
	public static final double MIN_PLACE_DISTANCE = 1.0;
	public static final double MAX_PLACE_DISTANCE = 6.0;

	public static double clampPlaceDistance(double distance) {
		return Math.clamp(distance, MIN_PLACE_DISTANCE, MAX_PLACE_DISTANCE);
	}
}
