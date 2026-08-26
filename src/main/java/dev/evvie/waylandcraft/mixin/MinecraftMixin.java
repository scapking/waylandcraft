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

	// 窗口焦点注入已迁移到 WindowMixin：26.x 起 GLFW 焦点回调由
	// com.mojang.blaze3d.platform.Window#onFocus(JZ) 直接承载，
	// Minecraft.windowFocusChanged(Z)（Yarn）/setWindowActive(Z)（Mojmap）
	// 在 26.1.2 中已不存在，继续在此注入会导致启动崩溃。

}
