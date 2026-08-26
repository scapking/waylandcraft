package dev.evvie.waylandcraft.mixin;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

import com.mojang.blaze3d.platform.Window;

import dev.evvie.waylandcraft.WaylandCraft;

/**
 * 窗口焦点变化 → 通知输入法穿透层做事件驱动的焦点重协商。
 *
 * 背景：KWin 等合成器只在键盘焦点「变化」时向 text_input 广播 enter，
 * 晚于焦点分配创建的 text_input 可能永远收不到 enter（IME 穿透 BLOCKED）。
 * 旧实现用 15 秒定时轮询重建 —— 已删除。现在改为在窗口重新获得焦点的
 * 事件里一次性重建 text_input，触发宿主合成器重新评估焦点路由。
 *
 * 目标选择（MC 26.1.2）：自 26.x 渲染层重构后，GLFW 回调统一收归
 * {@code com.mojang.blaze3d.platform.Window}（onMove/onResize/onFocus/...），
 * 旧的 {@code Minecraft.windowFocusChanged(Z)}（Yarn 名，Mojmap 曾为
 * {@code setWindowActive}）已不存在 —— 在它上面注入会导致
 * InvalidInjectionException 启动崩溃（v0.9.27 实测）。
 * {@code Window#onFocus(JZ)V} 是 GLFW glfwSetWindowFocusCallback 的
 * 直接回调，携带 (handle, focused)，语义与旧方法一致且更底层、稳定。
 *
 * require = 0：本注入是增强路径（焦点重协商），若未来版本再次改名，
 * 降级为 WARN 日志而非让整个游戏崩溃；穿透功能其余部分不受影响。
 */
@Mixin(Window.class)
public class WindowMixin {

	@Inject(method = "onFocus", at = @At("TAIL"), require = 0)
	public void waylandcraft$onWindowFocus(long handle, boolean focused, CallbackInfo info) {
		if (focused && WaylandCraft.instance != null && WaylandCraft.instance.bridge != null) {
			WaylandCraft.instance.bridge.notifyHostFocusGained();
		}
	}
}
