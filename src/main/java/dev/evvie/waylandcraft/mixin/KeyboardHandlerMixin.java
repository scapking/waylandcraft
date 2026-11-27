package dev.evvie.waylandcraft.mixin;

import java.util.HashMap;
import java.util.Iterator;
import java.util.Map;

import org.lwjgl.glfw.GLFW;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraft.KeyboardCaptureMode;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.client.KeyboardHandler;
import net.minecraft.client.Minecraft;
import net.minecraft.client.input.KeyEvent;

@Mixin(KeyboardHandler.class)
public class KeyboardHandlerMixin {

	// ═════════ release 丢失误吞兜底（"窗口里有个键永远按住"） ═════════
	//
	// 复现条件（本修复前的两条真实丢 release 路径）：
	//  A) J/G 绑定（CAPTURE/HARD_CAPTURE）中按住窗口键 → G 模式鼠标未绑定，
	//     鼠标点开任意 MC 屏幕（暂停/设置）→ 本方法仍被回调，但下方
	//     screen!=null / level==null 早退把该键的 RELEASE 一并丢弃 →
	//     窗口收不到 wl_keyboard release，永远认为键按住（卡键/自动重复）。
	//  B) 绑定中按住窗口键 → ESC 退出绑定（模式变 NONE）→ 之后该键的
	//     RELEASE 虽能进 onKeyPress，但 NONE 分支只做共享 hover 兜底后
	//     return false，不路由本地 bridge → release 丢弃，同卡键。
	//  C) MC 自身的窗口失焦处理会重置按键但不走 KeyboardHandler.keyPress
	//     （如切窗口/最小化）→ release 事件根本不到这里。
	//
	// 修复（v1.4.6 syncCapsLock 同款"GLFW 真值校准"模式）：
	//  1) 跟踪所有被 WaylandCraft 转发进窗口的 press（scancode → 记录）。
	//     ESC 明确排除——其 release 的既定语义是退出绑定，绝不能合成；
	//  2) 被跟踪键的 RELEASE 在早退门之前走修复路径：照常走 onKeyPress
	//     完整路由（模式未变时路由目标正确）；press 走的是本地路径而模式
	//     已回 NONE 时 onKeyPress 不会路由本地 → 直接 bridge.releaseKey 兜底；
	//  3) 每个新 PRESS 事件对跟踪表做一次 GLFW 真值校准：glfwGetKey 报
	//     RELEASE 而表内仍按住 = release 曾被吞（路径 C）→ 合成 release。
	// 非窗口键（MC 自身消费的按键）零行为变化：跟踪表只记录被拦截转发的
	// press，其余按键流程与旧版完全一致。

	/** 被转发进窗口且尚未见到 release 的按键：scancode → 转发记录。 */
	private static final Map<Integer, ForwardedPress> FORWARDED_WINDOW_KEYS = new HashMap<>();

	/** 一条 press 转发记录：GLFW keycode（glfwGetKey 校准用）+ 是否本地 bridge 路径。 */
	private static final class ForwardedPress {
		final int keyCode;
		final boolean localWindow;

		ForwardedPress(int keyCode, boolean localWindow) {
			this.keyCode = keyCode;
			this.localWindow = localWindow;
		}
	}

	/**
	 * 被吞 release 的修复转发。本地键：模式未变时走 onKeyPress 完整路由；
	 * 模式已回 NONE（复现 B）时 NONE 分支不路由本地，直接 bridge.releaseKey。
	 * 共享键：一律走 onKeyPress——hover 在则 paired release，hover 丢则其
	 * releaseAllForwardedKeys 兜底，均无需本地动作。
	 */
	private static void forwardLostRelease(long windowHandle, int scancode, ForwardedPress held, int modifiers) {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null) return;
		boolean routed = false;
		if(held.localWindow) {
			if(wlc.keyboardCaptureMode != KeyboardCaptureMode.NONE) {
				routed = wlc.onKeyPress(windowHandle, held.keyCode, scancode, GLFW.GLFW_RELEASE, modifiers);
			}
			if(!routed && wlc.bridge != null) {
				wlc.bridge.releaseKey(scancode);
			}
		} else {
			wlc.onKeyPress(windowHandle, held.keyCode, scancode, GLFW.GLFW_RELEASE, modifiers);
		}
		// 修复转发是低频事件（只在丢 release 后发生一次），INFO 保留可观测性。
		WaylandCraftCommon.LOGGER.info("[kb-debug] 丢 release 修复转发 key={} scancode={} local={}",
			held.keyCode, scancode, held.localWindow);
	}

	/**
	 * GLFW 真值校准（新 PRESS 事件时执行）：glfwGetKey 已报 RELEASE 而跟踪表
	 * 仍按住的键 = release 事件曾被吞 → 合成修复转发。典型即 MC 失焦重置
	 * （路径 C）——那些 release 不会经过 KeyboardHandler.keyPress。
	 */
	private static void calibrateStuckKeys(long windowHandle, int modifiers) {
		if(FORWARDED_WINDOW_KEYS.isEmpty()) return;
		Iterator<Map.Entry<Integer, ForwardedPress>> it = FORWARDED_WINDOW_KEYS.entrySet().iterator();
		while(it.hasNext()) {
			Map.Entry<Integer, ForwardedPress> e = it.next();
			if(GLFW.glfwGetKey(windowHandle, e.getValue().keyCode) == GLFW.GLFW_RELEASE) {
				it.remove();
				forwardLostRelease(windowHandle, e.getKey(), e.getValue(), modifiers);
			}
		}
	}

	
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
		// v1.4.2 分级收口：P2-13 声明"逐键刷屏→debug"，但这两行漏网仍是 INFO——
		// 1.4.1 实测 latest.log 每键 2 行 INFO + 磁盘写放大，是"打字/走路卡顿"
		// 的直接贡献者之一。逐键行严格 verbose 门；"注入是否生效"的里程碑证据
		// 改由首次按键打一行（firstPressLogged）承担，发布档仍可见且不刷屏。
		if(dev.evvie.waylandcraft.utils.WcLogging.verbose()) {
			WaylandCraftCommon.LOGGER.info("[kb-debug] mixin onPress key={} action={} scancode={} level={} screen={} mode={}",
				event.key(), action, scancode,
				Minecraft.getInstance().level != null ? "in" : "none",
				Minecraft.getInstance().screen != null ? Minecraft.getInstance().screen.getClass().getSimpleName() : "none",
				WaylandCraft.instance.keyboardCaptureMode);
		} else if(dev.evvie.waylandcraft.utils.WcLogging.FIRST_KEY_PRESS_LOGGED.compareAndSet(false, true)) {
			WaylandCraftCommon.LOGGER.info("[kb-debug] mixin injected OK (first key: key={} scancode={}) — 后续逐键行仅在 verboseLogging=true 时输出",
				event.key(), scancode);
		}
		
		// 第一步：xkb 状态先行（PRESS/RELEASE 才更新，REPEAT 不重复触发 Caps Lock 切换）
		if((action == GLFW.GLFW_PRESS || action == GLFW.GLFW_RELEASE)
				&& WaylandCraft.instance.bridge != null) {
			// v1.4.6（大小写反转修复）：每键先用 GLFW 读到的**平台 Caps Lock
			// 真值**校准 seat xkb 的 locked 位——用户在其他应用/聊天框切 caps
			// 时我们不经过按键事件，locked 会与真实状态漂移，窗口里 en 模式
			// 大小写整体反转。syncCapsLock 幂等（一致时零事件零开销）。
			boolean capsOn = GLFW.glfwGetKey(windowHandle, GLFW.GLFW_KEY_CAPS_LOCK) == GLFW.GLFW_TRUE;
			WaylandCraft.instance.bridge.syncCapsLock(capsOn);
			WaylandCraft.instance.bridge.internalKeyUpdate(scancode, action == GLFW.GLFW_PRESS);
		}

		// 第一步半：被吞 release 的修复路径（先于早退门——见类头"复现条件"）。
		// RELEASE：本键曾被转发进窗口 → 立即修复转发（不进 MC 屏幕流程）。
		// PRESS：顺路做一次 GLFW 真值校准，合成曾被吞掉的 release。
		if(action == GLFW.GLFW_RELEASE) {
			ForwardedPress held = FORWARDED_WINDOW_KEYS.remove(scancode);
			if(held != null) {
				forwardLostRelease(windowHandle, scancode, held, event.modifiers());
				return;
			}
		} else if(action == GLFW.GLFW_PRESS) {
			calibrateStuckKeys(windowHandle, event.modifiers());
		}

		// 第二步：游戏内（无 UI 打开）才拦截转发到窗口
		if(Minecraft.getInstance().level == null) return;
		if(Minecraft.getInstance().screen != null) return;

		boolean intercepted = WaylandCraft.instance.onKeyPress(windowHandle, event.key(), scancode, action, event.modifiers());
		if(intercepted) info.cancel();
		// [kb-debug] onKeyPress 返回值 = 是否拦截（true 才 cancel，Minecraft 收不到该键）
		// v1.4.2：逐键 INFO → verbose 门（同上）。
		if(dev.evvie.waylandcraft.utils.WcLogging.verbose()) {
			WaylandCraftCommon.LOGGER.info("[kb-debug] onKeyPress 返回 {} (key={} action={})", intercepted, event.key(), action);
		}

		// 第三步：跟踪窗口按键 press（release 兜底的依据）。只记录"被拦截
		// 且 mode!=NONE（本地 bridge）或 NONE+hover（共享窗口）"的 press；
		// ESC 排除——其 release 的既定语义是退出绑定，绝不能合成修复转发。
		if(action == GLFW.GLFW_PRESS && intercepted && event.key() != GLFW.GLFW_KEY_ESCAPE) {
			FORWARDED_WINDOW_KEYS.put(scancode, new ForwardedPress(event.key(),
					WaylandCraft.instance.keyboardCaptureMode != KeyboardCaptureMode.NONE));
		}
	}
	
}
