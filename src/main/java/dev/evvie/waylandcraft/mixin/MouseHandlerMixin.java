package dev.evvie.waylandcraft.mixin;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

import dev.evvie.waylandcraft.WaylandCraft;
import net.minecraft.client.MouseHandler;
import net.minecraft.client.input.MouseButtonInfo;

@Mixin(MouseHandler.class)
public class MouseHandlerMixin {
	
	// 注：注入点用 HEAD（与 KeyboardHandlerMixin 相同）。旧版用
	//   @At(value="INVOKE", target="KeyMapping;set(...)") / FIELD(Minecraft.player)
	// 依赖 MouseHandler 内部字节码结构；MC 26.1.2 起内部实现变化 →
	// target 找不到 → fabric mixin 默认只 warning 不报错 → 注入静默失效
	// （v0.8.6 已在 KeyboardHandlerMixin 踩过同一坑）。
	// HEAD 是方法入口，签名稳定，不随内部实现变化。
	@Inject(method = "onButton", at = @At("HEAD"), cancellable = true)
	public void onButton(long windowHandle, MouseButtonInfo buttonInfo, int action, CallbackInfo info) {
		if(WaylandCraft.instance.onButtonPress(windowHandle, buttonInfo.button(), action, buttonInfo.modifiers())) info.cancel();
	}
	
	@Inject(method = "onScroll", at = @At("HEAD"), cancellable = true)
	public void onScroll(long windowHandle, double scrollX, double scrollY, CallbackInfo info) {
		if(WaylandCraft.instance.onScroll(windowHandle, scrollX, scrollY)) info.cancel();
	}
	
	@Shadow public double accumulatedDX;
	@Shadow public double accumulatedDY;
	
	@Inject(method = "turnPlayer", at = @At("HEAD"), cancellable = true)
	public void onTurnPlayer(double timeDelta, CallbackInfo info) {
		if(WaylandCraft.instance.onMouseTurn(accumulatedDX, accumulatedDY)) info.cancel();
	}
	
}
