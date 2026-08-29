package dev.evvie.waylandcraft.ime;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;
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
 *
 * C 方案（v0.9.39 之后）：mod 不再当 IME 引擎，光标上报仅在 XIM 路径
 * （xterm 等纯 X11 应用）有效。当前 firefox 等 ti3 应用自己处理候选窗
 * 锚点（通过 zwp_text_input_v3.set_cursor_rectangle）。本类调用的
 * updateCursorRectNative 是 no-op 兼容函数 ——保留类是为不破坏 Java 端
 * 旧调用，但实际不产生效果。
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
		// 优先用 MC 自己的焦点跟踪（Screen.getFocused）：它记录最后一次交互的组件，
		// 多输入框场景（聊天/搜索/服务器地址共存）不会误取非聚焦框 —— 实测
		// 遍历 children 检查 isFocused() 会在多个框都返回 true 时取错框，
		// 导致光标矩形在 y=21 ↔ y=244 间跳变（v0.9.33 实机漂移根因之一）。
		GuiEventListener focused = mc.screen.getFocused();
		if (focused instanceof EditBox eb) {
			box = eb;
		}
		if (box == null) {
			for (GuiEventListener child : mc.screen.children()) {
				if (child instanceof EditBox eb && eb.isFocused()) {
					box = eb;
					break;
				}
			}
		}
		if (box == null) {
			// 无焦点文本框 → 光标矩形不可用；发一个归零让后端把候选窗收走/落底。
			// 只有上次有值时补发一次，避免每 tick 刷 JNI。
			if (lastX != Integer.MIN_VALUE) {
				lastX = lastY = Integer.MIN_VALUE;
				// P0 诊断：值丢失瞬间记录 getFocused() 返回什么（null / 非 EditBox 组件），
				// 定位 (0,0,0,0) 锚点漂移的触发源。只在有值→无值转换时打一次，防刷屏。
				String focusedDesc = focused == null ? "null" : focused.getClass().getSimpleName();
				WaylandCraftCommon.LOGGER.info("[cursor] 焦点丢失 focused={} screen={} -> 补发 (0,0,0,0)",
					focusedDesc, mc.screen.getClass().getSimpleName());
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
			// P0 诊断：位置变化时记录 EditBox 身份（类 + 光标前文本），
			// 确认锚点来源是哪个框（WaylandCraft 世界 UI vs MC 聊天框）。
			String text = box.getValue();
			if (text.length() > 16) {
				text = text.substring(0, 16) + "...";
			}
			WaylandCraftCommon.LOGGER.info("[cursor] EditBox class={} value=\"{}\" -> ({},{},{},{}) scale={}",
				box.getClass().getSimpleName(), text, x, y, 2 * guiScale, 9 * guiScale, guiScale);
			lastX = x;
			lastY = y;
			// 光标近似矩形：宽 2 高 9（字体行高）→ 物理像素；桌面候选窗只取左上锚点。
			WaylandCraft.instance.bridge.updateCursorRect(x, y, 2 * guiScale, 9 * guiScale);
		}
	}
}
