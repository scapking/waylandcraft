package dev.evvie.waylandcraft.gui;

import org.joml.Matrix3x2fStack;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraft.KeyboardCaptureMode;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.bridge.IconSurface;
import dev.evvie.waylandcraft.gui.theme.WcColors;
import dev.evvie.waylandcraft.render.RenderUtils;
import dev.evvie.waylandcraft.render.WindowFramebuffer;
import net.fabricmc.fabric.api.client.rendering.v1.hud.HudElementRegistry;
import net.fabricmc.fabric.api.client.rendering.v1.hud.VanillaHudElements;
import net.minecraft.client.DeltaTracker;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.Font;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.Identifier;

/**
 * 极简 HUD：纯命令行模式下只保留两类必要信息——
 * 1. 键盘捕获状态警告（否则玩家不知道键盘已被窗口接管）
 * 2. 拖放图标（drag & drop 的必要反馈）
 */
public class WaylandHudRenderer {
	
	private WaylandCraft wlc;
	private static final Identifier APP_LIST = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "app-list");
	private static final Identifier DND_ICON = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "dnd-icon");
	
	public WaylandHudRenderer(WaylandCraft wlc) {
		this.wlc = wlc;
	}
	
	private static final Identifier IME_BUILTIN = Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "ime-builtin-panel");

	public void register() {
		HudElementRegistry.attachElementAfter(VanillaHudElements.BOSS_BAR, APP_LIST, this::extractAppListRenderState);
		HudElementRegistry.attachElementAfter(VanillaHudElements.BOSS_BAR, DND_ICON, this::extractDNDIconRenderState);
		// 内置候选面板 HUD 形态：键盘捕获（screen==null）往应用窗口打字时，
		// 原版字体文本面板显示在屏幕底部中央（ImeBuiltinPanel 内部再判形态）。
		HudElementRegistry.attachElementAfter(VanillaHudElements.BOSS_BAR, IME_BUILTIN,
				(ctx, tracker) -> dev.evvie.waylandcraft.ime.ImeBuiltinPanel.extractHud(ctx));
	}
	
	private void extractAppListRenderState(GuiGraphicsExtractor context, DeltaTracker deltaTracker) {
		Font font = Minecraft.getInstance().font;
		int yoff = 30;
		
		if(WaylandCraft.instance.keyboardCaptureMode == KeyboardCaptureMode.CAPTURE) {
			String text = "KEYBOARD CAPTURED [PRESS ESCAPE]";
			drawWarningTag(context, font, text, yoff);
		}
		else if(WaylandCraft.instance.keyboardCaptureMode == KeyboardCaptureMode.HARD_CAPTURE) {
			String text = "KEYBOARD CAPTURED [PRESS ALT+Q]";
			drawWarningTag(context, font, text, yoff);
		}
	}
	
	private void drawWarningTag(GuiGraphicsExtractor context, Font font, String text, int yoff) {
		int x = context.guiWidth() - font.width(text) - 14;
		// 危险提示条
		context.fill(x - 8, yoff - 3, context.guiWidth() - 6, yoff + font.lineHeight + 3, WcColors.DANGER_DIM);
		context.fill(x - 2, yoff - 3, x, yoff + font.lineHeight + 3, WcColors.DANGER);
		context.text(font, text, x, yoff, WcColors.DANGER, true);
	}
	
	private void extractDNDIconRenderState(GuiGraphicsExtractor context, DeltaTracker tracker) {
		if(wlc == null || wlc.bridge == null) return; // native disabled: no DND icon
		int guiScale = (int) Minecraft.getInstance().getWindow().getGuiScale();
		
		IconSurface dndIcon = wlc.bridge.dndIcon;
		if(dndIcon != null && dndIcon.framebuffer != null) {
			WindowFramebuffer buf = dndIcon.framebuffer;
			
			int x = -buf.getXOff();
			int y = -buf.getYOff();
			int w = buf.getWidth();
			int h = buf.getHeight();
			
			Matrix3x2fStack stack = context.pose();
			stack.pushMatrix();
			stack.translate(context.guiWidth() / 2, context.guiHeight() / 2);
			stack.scale(1.0f / guiScale, 1.0f / guiScale);
			RenderUtils.renderFramebuffer2D(context, buf, x, y, w, h);
			stack.popMatrix();
		}
	}
	
}
