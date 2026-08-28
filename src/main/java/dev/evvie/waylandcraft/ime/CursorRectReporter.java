package dev.evvie.waylandcraft.ime;

import dev.evvie.waylandcraft.WaylandCraft;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.components.EditBox;
import net.minecraft.client.gui.components.events.GuiEventListener;

/**
 * 焦点文本框光标矩形上报 —— 桌面候选窗锚点（防漂移核心）。
 *
 * 候选窗由桌面输入法框架显示（fcitx5 kimpanel / ibus panel / GNOME 集成），
 * 游戏不自绘。这里只负责把锚点报准：
 * 每 tick 检查当前 Screen 里聚焦的 EditBox，取其光标屏幕坐标，
 * 位置变化时经 JNI 上报 → Rust SetCursorRect（fcitx5）/
 * SetCursorLocationRelative（ibus），让桌面候选窗钉在光标处。
 */
public final class CursorRectReporter {
	private static int lastX = Integer.MIN_VALUE;
	private static int lastY = Integer.MIN_VALUE;

	private CursorRectReporter() {
	}

	/** 每 tick 调用（MinecraftMixin.runTick HEAD）。 */
	public static void tick() {
		Minecraft mc = Minecraft.getInstance();
		if (mc == null || mc.screen == null || WaylandCraft.instance == null
				|| WaylandCraft.instance.bridge == null) {
			return;
		}
		EditBox box = null;
		for (GuiEventListener child : mc.screen.children()) {
			if (child instanceof EditBox eb && eb.isFocused()) {
				box = eb;
				break;
			}
		}
		if (box == null) {
			// 无焦点文本框 → 光标矩形不可用；发一个归零让后端把候选窗收走/落底。
			// 只有上次有值时补发一次，避免每 tick 刷 JNI。
			if (lastX != Integer.MIN_VALUE) {
				lastX = lastY = Integer.MIN_VALUE;
				WaylandCraft.instance.bridge.updateCursorRect(0, 0, 0, 0);
			}
			return;
		}
		int x = box.getScreenX(box.getCursorPosition());
		int y = box.getY();
		// MC 的 getScreenX/getY 是 GUI 逻辑坐标（相对窗口左上，guiScale 坐标系）；
		// Wayland SetCursorLocationRelative / fcitx5 SetCursorRect 期望 surface 物理像素。
		// 不乘 scale 时，候选窗锚点差一个 scale 因子 → 输入法"漂移"（v0.9.33 实机根因之一）。
		int guiScale = (int) Minecraft.getInstance().getWindow().getGuiScale();
		x *= guiScale;
		y *= guiScale;
		if (x != lastX || y != lastY) {
			lastX = x;
			lastY = y;
			// 光标近似矩形：宽 2 高 9（字体行高）→ 物理像素；桌面候选窗只取左上锚点。
			WaylandCraft.instance.bridge.updateCursorRect(x, y, 2 * guiScale, 9 * guiScale);
		}
	}
}
