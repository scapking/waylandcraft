package dev.evvie.waylandcraft.mixin;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.render.WindowTranslucencyHotfix;
import net.minecraft.client.Minecraft;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.world.phys.BlockHitResult;
import net.minecraft.world.phys.HitResult;
import net.minecraft.world.phys.Vec3;

@Mixin(Minecraft.class)
public class MinecraftMixin {
	
	@Inject(method = "runTick", at = @At(value = "INVOKE_STRING", target = "Lcom/mojang/blaze3d/platform/Window;setErrorSection(Ljava/lang/String;)V", args = "ldc=Post render"))
	public void updateRunTick(boolean doTick, CallbackInfo info) {
		WaylandCraft.instance.update();
	}
	
	@Inject(method = "renderFrame", at = @At(value = "INVOKE_STRING", target = "Lnet/minecraft/util/profiling/ProfilerFiller;push(Ljava/lang/String;)V", args = "ldc=present"))
	public void hotfixRenderFrame(boolean advanceGameTime, CallbackInfo info) {
		WindowTranslucencyHotfix.render();
	}
	
	@Inject(method = "pick", at = @At("TAIL"))
	public void pick(float partialTicks, CallbackInfo info) {
		HitResult result = Minecraft.getInstance().hitResult;
		Vec3 pos = Minecraft.getInstance().player.getEyePosition(partialTicks);
		
		WaylandCraft.instance.trueGameHitResult = result;
		if(WaylandCraft.instance.overridePickBlock) {
			Minecraft.getInstance().hitResult = BlockHitResult.miss(pos, Direction.DOWN, BlockPos.containing(pos));
			Minecraft.getInstance().crosshairPickEntity = null;
		}
	}

	/**
	 * 窗口焦点变化 → 通知输入法穿透层做事件驱动的焦点重协商。
	 *
	 * 背景：KWin 等合成器只在键盘焦点「变化」时向 text_input 广播 enter，
	 * 晚于焦点分配创建的 text_input 可能永远收不到 enter（IME 穿透 BLOCKED）。
	 * 旧实现用 15 秒定时轮询重建 —— 已删除。现在改为在窗口重新获得焦点的
	 * 事件里一次性重建 text_input，触发宿主合成器重新评估焦点路由。
	 * windowFocusChanged(boolean) 是 Minecraft.windowFocusChanged 的稳定签名。
	 */
	@Inject(method = "windowFocusChanged", at = @At("TAIL"))
	public void onWindowFocusChanged(boolean focused, CallbackInfo info) {
		if(focused && WaylandCraft.instance.bridge != null) {
			WaylandCraft.instance.bridge.notifyHostFocusGained();
		}
	}
	
}
