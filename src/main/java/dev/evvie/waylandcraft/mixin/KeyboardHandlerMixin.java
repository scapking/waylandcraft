package dev.evvie.waylandcraft.mixin;

import org.lwjgl.glfw.GLFW;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

import dev.evvie.waylandcraft.WaylandCraft;
import net.minecraft.client.KeyboardHandler;
import net.minecraft.client.Minecraft;
import net.minecraft.client.input.KeyEvent;

@Mixin(KeyboardHandler.class)
public class KeyboardHandlerMixin {
	
	/**
	 * 游戏内按键拦截（Ctrl+方向键 / Alt+Q / 键盘捕获转发）。
	 * 
	 * 注意：注入点必须用 HEAD —— 旧版用
	 *   @At(value="INVOKE", target="InputConstants;getKey(Lnet/minecraft/client/input/KeyEvent;)...", ordinal=1)
	 * 依赖 KeyboardHandler.keyPress 内部字节码结构。MC 26.1.2 起 keyPress
	 * 不再调用 InputConstants.getKey(KeyEvent)（改为直接 window.handle / key.getValue），
	 * target 找不到 → fabric mixin 默认只 warning 不报错 → 注入静默失效 →
	 * onKeyPress 永不执行 → Ctrl+方向键/Alt+Q/键盘捕获全部失效。
	 * HEAD 是方法入口，签名 keyPress(JILnet/minecraft/client/input/KeyEvent;)V 稳定，
	 * 不随内部实现变化。
	 */
	@Inject(method = "keyPress", at = @At("HEAD"), cancellable = true)
	public void onPressInGame(long windowHandle, int action, KeyEvent event, CallbackInfo info) {
		int scancode = WaylandCraft.correctScancode(event.scancode());
		
		if(Minecraft.getInstance().level == null) return;
		if(Minecraft.getInstance().screen != null) return;
		
		if(WaylandCraft.instance.onKeyPress(windowHandle, event.key(), scancode, action, event.modifiers())) info.cancel();
	}
	
	@Inject(method = "keyPress", at = @At("HEAD"), cancellable = false)
	public void onPressGlobal(long windowHandle, int action, KeyEvent event, CallbackInfo info) {
		int scancode = WaylandCraft.correctScancode(event.scancode());
		
		if(action != GLFW.GLFW_PRESS && action != GLFW.GLFW_RELEASE) return;
		if(WaylandCraft.instance.bridge == null) return;
		
		WaylandCraft.instance.bridge.internalKeyUpdate(scancode, action == GLFW.GLFW_PRESS);
	}
	
}
