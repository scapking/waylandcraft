package dev.evvie.waylandcraft.mixin;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

import dev.evvie.waylandcraft.ime.ImeCandidateOverlay;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.gui.screens.Screen;

/**
 * 候选窗 overlay 绘制钩子（Phase 2）。
 *
 * 注入点选 {@code Screen.extractRenderStateWithTooltipAndSubtitles} 的
 * RETURN：该方法是 **final** 且所有 Screen 每帧必经（聊天框/搜索框/服务器
 * 地址栏全覆盖），在其返回后绘制可保证候选窗永远浮在最上层（tooltip 同级）。
 * 这是 docs/IME_REDESIGN.md 既定的挂载方式（Gizmo/RenderState 体系下的
 * 等价注入点，避免 HudElementRegistry——HUD 在 Screen 打开时不渲染）。
 */
@Mixin(Screen.class)
public class ScreenRenderMixin {

	@Inject(method = "extractRenderStateWithTooltipAndSubtitles", at = @At("RETURN"))
	private void imeCandidateOverlay(GuiGraphicsExtractor context, int mouseX, int mouseY,
			float tickDelta, CallbackInfo info) {
		ImeCandidateOverlay.extract(context);
	}
}
