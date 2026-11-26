package dev.evvie.waylandcraft.utils;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import com.sun.jna.Library;
import com.sun.jna.Native;
import com.sun.jna.Pointer;

/**
 * 通过 XTest 向 X11 显示注入指针/按键/滚轮事件。
 *
 * 用于 X11-only 窗口（微信）的远程交互：这类窗口不是 wayland surface，
 * waylandcraft 的 wl 指针注入对它们无效，必须用 XTest 伪造全局 X 事件。
 *
 * 注意：XTest 移动的是 X server 的全局指针（会同时移动宿主机的物理鼠标）。
 * 这是 X11 共享的固有取舍（类似 VNC 控制），远程玩家操作时会接管鼠标。
 *
 * 连接缓存：交互事件频率高（鼠标移动），每次 XOpenDisplay 开销大。
 * 这里缓存最近使用的 X 连接（display 名不变时复用），首次使用时建立。
 * JNA 调用本身非线程安全，但所有注入都发生在 Minecraft 主线程（网络回调
 * execute 到主线程），无需额外加锁。
 */
public class X11Interaction {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-x11-interaction");

	// X11 滚轮按键号（X 没有真实滚轮事件，传统上用 button 4/5/6/7）
	private static final int BTN_WHEEL_UP = 4;
	private static final int BTN_WHEEL_DOWN = 5;
	private static final int BTN_WHEEL_LEFT = 6;
	private static final int BTN_WHEEL_RIGHT = 7;

	private interface X11 extends Library {
		Pointer XOpenDisplay(String displayName);
		int XCloseDisplay(Pointer display);
		int XKeysymToKeycode(Pointer display, long keysym);
		// P1-8：交互注入前把目标窗口 raise + 设定输入焦点并回读验证
		int XRaiseWindow(Pointer display, long window);
		int XSetInputFocus(Pointer display, long focus, int revertTo, long timestamp);
		int XGetInputFocus(Pointer display, Pointer[] focusReturn, com.sun.jna.ptr.IntByReference revertToReturn);
		int XSync(Pointer display, boolean discard);
	}

	private interface Xtst extends Library {
		int XTestFakeMotionEvent(Pointer display, int screenNumber, int x, int y, long delay);
		int XTestFakeButtonEvent(Pointer display, int button, int isPress, long delay);
		int XTestFakeKeyEvent(Pointer display, int keycode, int isPress, long delay);
	}

	/**
	 * 惰性加载 libX11 / libXtst：无 X11 的平台（Windows/macOS/iOS viewer）
	 * 返回 null，所有注入方法先判空，避免类初始化时 UnsatisfiedLinkError 崩客户端。
	 */
	private static final X11 X11_LIB = loadX11();
	private static final Xtst XTST_LIB = loadXtst();

	private static X11 loadX11() {
		try {
			return Native.load("X11", X11.class);
		} catch(Throwable t) {
			LOGGER.debug("libX11 not available on this platform (X11 interaction disabled)", t);
			return null;
		}
	}

	private static Xtst loadXtst() {
		try {
			return Native.load("Xtst", Xtst.class);
		} catch(Throwable t) {
			LOGGER.debug("libXtst not available on this platform (X11 interaction disabled)", t);
			return null;
		}
	}

	// ===== 连接缓存 =====
	private static String cachedDisplayName = null;
	private static Pointer cachedDisplay = null;

	private static synchronized Pointer display(String displayName) {
		if(X11_LIB == null) {
			return null;
		}
		String name = (displayName == null || displayName.isEmpty()) ? null : displayName;
		if(cachedDisplay != null && java.util.Objects.equals(name, cachedDisplayName)) {
			return cachedDisplay;
		}
		closeCached();
		Pointer d = X11_LIB.XOpenDisplay(name);
		if(d == null) {
			LOGGER.warn("Cannot open X11 display '{}' for interaction injection", name);
			return null;
		}
		cachedDisplay = d;
		cachedDisplayName = name;
		return d;
	}

	private static synchronized void closeCached() {
		if(cachedDisplay != null) {
			try {
				X11_LIB.XCloseDisplay(cachedDisplay);
			} catch(Throwable ignored) {}
			cachedDisplay = null;
			cachedDisplayName = null;
		}
	}

	private X11Interaction() {}

	/**
	 * 移动全局指针到屏幕绝对坐标 (x, y)。
	 */
	public static boolean injectPointerMotion(String displayName, int x, int y) {
		if(XTST_LIB == null) {
			return false;
		}
		Pointer display = display(displayName);
		if(display == null) return false;
		try {
			XTST_LIB.XTestFakeMotionEvent(display, 0, x, y, 0);
			return true;
		} catch(Throwable t) {
			LOGGER.warn("XTestFakeMotionEvent failed", t);
			return false;
		}
	}

	/**
	 * 按下/释放鼠标按钮。X11 button: 1=左, 2=中, 3=右。
	 */
	public static boolean injectButton(String displayName, int button, boolean pressed) {
		if(XTST_LIB == null) {
			return false;
		}
		Pointer display = display(displayName);
		if(display == null) return false;
		try {
			XTST_LIB.XTestFakeButtonEvent(display, button, pressed ? 1 : 0, 0);
			return true;
		} catch(Throwable t) {
			LOGGER.warn("XTestFakeButtonEvent failed", t);
			return false;
		}
	}

	/**
	 * 按下/释放键盘按键（keysym）。
	 */
	public static boolean injectKey(String displayName, long keysym, boolean pressed) {
		if(X11_LIB == null || XTST_LIB == null) {
			return false;
		}
		if(keysym <= 0) return false;
		Pointer display = display(displayName);
		if(display == null) return false;
		try {
			int keycode = X11_LIB.XKeysymToKeycode(display, keysym);
			if(keycode == 0) {
				LOGGER.warn("No keycode for keysym 0x{}", Long.toHexString(keysym));
				return false;
			}
			XTST_LIB.XTestFakeKeyEvent(display, keycode, pressed ? 1 : 0, 0);
			return true;
		} catch(Throwable t) {
			LOGGER.warn("XTestFakeKeyEvent failed", t);
			return false;
		}
	}

	// XSetInputFocus 常量
	private static final int REVERT_TO_PARENT = 2;   // RevertToParent
	private static final long CURRENT_TIME = 0L;     // CurrentTime

	/**
	 * P1-8：把目标窗口提到最前并把 X 输入焦点设定到它，回读验证成功才返回 true。
	 *
	 * XTest 的按键事件作用于**输入焦点窗口**而不是任何坐标——不做这一步，
	 * 授予 INTERACT 就等于把 owner 桌面级的键盘注入权交给了远端玩家
	 * （owner 正聚焦终端时，远端的按键全部打进终端）。
	 * 回读 XGetInputFocus 防止 BadMatch（窗口未映射/已销毁）被 Xlib 默认
	 * 错误处理器吞掉后"看似成功"地继续注入。
	 */
	public static boolean raiseAndFocusWindow(String displayName, long xid) {
		if(X11_LIB == null || xid == 0) {
			return false;
		}
		Pointer display = display(displayName);
		if(display == null) return false;
		try {
			X11_LIB.XRaiseWindow(display, xid);
			X11_LIB.XSetInputFocus(display, xid, REVERT_TO_PARENT, CURRENT_TIME);
			X11_LIB.XSync(display, false); // 冲刷请求，使 BadMatch 在回读前生效

			Pointer[] focusOut = new Pointer[1];
			com.sun.jna.ptr.IntByReference revertOut = new com.sun.jna.ptr.IntByReference();
			int status = X11_LIB.XGetInputFocus(display, focusOut, revertOut);
			long got = (status != 0 || focusOut[0] == null) ? -1 : Pointer.nativeValue(focusOut[0]);
			if(got != xid) {
				LOGGER.warn("X input focus not on window 0x{} (got 0x{}); key injection rejected",
					Long.toHexString(xid), Long.toHexString(got));
				return false;
			}
			return true;
		} catch(Throwable t) {
			LOGGER.warn("raiseAndFocusWindow(0x{}) failed; key injection rejected", Long.toHexString(xid), t);
			return false;
		}
	}

	/**
	 * 注入滚轮事件。
	 * @param scrollX 水平滚动量（>0 右，<0 左）
	 * @param scrollY 垂直滚动量（>0 上，<0 下）
	 */
	public static boolean injectScroll(String displayName, double scrollX, double scrollY) {
		if(XTST_LIB == null) {
			return false;
		}
		Pointer display = display(displayName);
		if(display == null) return false;
		try {
			// 每 0.5 步一个滚轮滴答，凑整数事件数
			int verticalTicks = (int) Math.round(Math.abs(scrollY) * 2);
			int horizontalTicks = (int) Math.round(Math.abs(scrollX) * 2);
			for(int i = 0; i < verticalTicks; i++) {
				int btn = scrollY > 0 ? BTN_WHEEL_UP : BTN_WHEEL_DOWN;
				XTST_LIB.XTestFakeButtonEvent(display, btn, 1, 0);
				XTST_LIB.XTestFakeButtonEvent(display, btn, 0, 0);
			}
			for(int i = 0; i < horizontalTicks; i++) {
				int btn = scrollX > 0 ? BTN_WHEEL_RIGHT : BTN_WHEEL_LEFT;
				XTST_LIB.XTestFakeButtonEvent(display, btn, 1, 0);
				XTST_LIB.XTestFakeButtonEvent(display, btn, 0, 0);
			}
			return true;
		} catch(Throwable t) {
			LOGGER.warn("XTest scroll injection failed", t);
			return false;
		}
	}
}
