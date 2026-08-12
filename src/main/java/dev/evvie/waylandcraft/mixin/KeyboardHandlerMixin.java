package dev.evvie.waylandcraft.mixin;

import org.lwjgl.glfw.GLFW;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.client.KeyboardHandler;
import net.minecraft.client.Minecraft;
import net.minecraft.client.input.KeyEvent;

@Mixin(KeyboardHandler.class)
public class KeyboardHandlerMixin {
	
	/**
	 * 游戏内按键拦截 + xkb 状态同步（合并为单个 HEAD 注入，顺序保证）。
	 *
	 * 顺序是修复"大小写/修饰键时灵时不灵"的关键（底层 xkb 顺序 bug）：
	 *   1) 先 internalKeyUpdate 更新 compositor 的 xkb_state（Caps Lock / Shift /
	 *      Ctrl 等锁定与瞬间修饰键在这里进入状态）；
	 *   2) 再 onKeyPress → bridge.pressKey 把键事件转发给窗口。
	 *
	 * 旧代码是两个独立 HEAD 注入（onPressInGame 先转发、onPressGlobal 后更新 xkb），
	 * 导致每次发给窗口的 wl_keyboard.modifiers 都落后一拍：按 Caps Lock 时窗口先收到
	 * caps key + locked=0（旧状态），浏览器按旧 modifiers 把大写锁定重置回关 →
	 * 下一个字母变小写 / 大小写无法作用窗口。Minecraft 自己读 GLFW 输入所以正常，
	 * 窗口走我们的 wl 事件所以中招——这正是用户观察到的"窗口不行"。
	 *
	 * xkb 更新对所有键都执行（包括聊天/菜单打开时，只要 bridge 存在），
	 * 保证在 Minecraft 里切换 Caps Lock 后，捕获窗口时键盘聚焦事件的 locked 状态正确。
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
	public void onPress(long windowHandle, int action, KeyEvent event, CallbackInfo info) {
		int scancode = WaylandCraft.correctScancode(event.scancode());
		
		// [kb-debug] mixin 注入生效的证据：这里执行 = KeyboardHandlerMixin 已注入成功。
		// 若用户日志里完全没有 [kb-debug] 行 → 注入失效，按键根本没进 WaylandCraft。
		WaylandCraftCommon.LOGGER.info("[kb-debug] mixin onPress key={} action={} scancode={} level={} screen={} mode={}",
			event.key(), action, scancode,
			Minecraft.getInstance().level != null ? "in" : "none",
			Minecraft.getInstance().screen != null ? Minecraft.getInstance().screen.getClass().getSimpleName() : "none",
			WaylandCraft.instance.keyboardCaptureMode);
		
		// 第一步：xkb 状态先行（PRESS/RELEASE 才更新，REPEAT 不重复触发 Caps Lock 切换）
		if((action == GLFW.GLFW_PRESS || action == GLFW.GLFW_RELEASE)
				&& WaylandCraft.instance.bridge != null) {
			WaylandCraft.instance.bridge.internalKeyUpdate(scancode, action == GLFW.GLFW_PRESS);
		}
		
		// 第二步：游戏内（无 UI 打开）才拦截转发到窗口
		if(Minecraft.getInstance().level == null) return;
		if(Minecraft.getInstance().screen != null) return;
		
		boolean intercepted = WaylandCraft.instance.onKeyPress(windowHandle, event.key(), scancode, action, event.modifiers());
		if(intercepted) info.cancel();
		// [kb-debug] onKeyPress 返回值 = 是否拦截（true 才 cancel，Minecraft 收不到该键）
		WaylandCraftCommon.LOGGER.info("[kb-debug] onKeyPress 返回 {} (key={} action={})", intercepted, event.key(), action);
	}
	
}
