package dev.evvie.waylandcraft.utils;

import java.util.ArrayList;
import java.util.List;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import com.sun.jna.Library;
import com.sun.jna.Native;
import com.sun.jna.Pointer;

/**
 * 通过 JNA 枚举 X11 顶层窗口（标题 / PID / 应用 ID）。
 *
 * 用于桌面窗口捕获的辅助发现：在不走 Wayland portal 的情况下，
 * 列出当前 X11 显示上的顶层窗口，方便用户选择要捕获的窗口。
 */
public class X11WindowLister {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-x11");

	private static final int XA_STRING = 31;
	private static final int XA_CARDINAL = 6;
	private static final int XA_WINDOW = 33;

	private interface X11 extends Library {
		X11 INSTANCE = Native.load("X11", X11.class);

		Pointer XOpenDisplay(String displayName);
		int XCloseDisplay(Pointer display);
		long XDefaultRootWindow(Pointer display);
		int XQueryTree(Pointer display, long window, long[] rootReturn, long[] parentReturn, Pointer[] childrenReturn, int[] nchildrenReturn);
		int XFetchName(Pointer display, long window, String[] nameReturn);
		int XFree(Pointer data);
		// NOTE: 参数类型必须是 int 而不是 boolean —— JNA 5.14.0 会把 boolean true 映射成 -1 (0xffffffff)
		// 而不是 1，导致 X11 请求的 onlyIfExists/delete 字段变成 0xff，X server 返回
		// "BadValue (X_InternAtom, 0xff)" 并让 Xlib 默认错误处理直接退出进程。
		long XInternAtom(Pointer display, String atomName, int onlyIfExists);
		int XGetWindowProperty(Pointer display, long window, long property, long longOffset, long longLength,
				int delete, long reqType, long[] actualTypeReturn, int[] actualFormatReturn,
				long[] nitemsReturn, long[] bytesAfterReturn, Pointer[] propReturn);
		int XGetWMName(Pointer display, long window, Pointer[] textPropertyReturn);
	}

	/** 一个 X11 顶层窗口的元信息 */
	public static class WindowInfo {
		/** 窗口 id（十六进制），用作捕获时的标识 */
		public final String hash;
		public final String title;
		public final String appId;
		public final int pid;

		public WindowInfo(String hash, String title, String appId, int pid) {
			this.hash = hash;
			this.title = title;
			this.appId = appId;
			this.pid = pid;
		}
	}

	private X11WindowLister() {}

	/**
	 * 列出当前 X11 显示的所有顶层窗口（使用环境变量 DISPLAY）。
	 * 无 X11 显示时返回空列表（不抛异常）。
	 */
	public static List<WindowInfo> getDesktopWindows() {
		return getDesktopWindows(null);
	}

	/**
	 * 列出指定 X11 显示的所有顶层窗口。
	 * displayName 为 null 时使用环境变量 DISPLAY（XOpenDisplay(null) 语义）。
	 * 无 X11 显示时返回空列表（不抛异常）。
	 *
	 * @param displayName X display 名（如 ":2"），null = 默认
	 */
	public static List<WindowInfo> getDesktopWindows(String displayName) {
		List<WindowInfo> result = new ArrayList<>();
		Pointer display = X11.INSTANCE.XOpenDisplay(displayName);
		if(display == null) {
			LOGGER.debug("No X11 display available (requested '{}')", displayName);
			return result;
		}

		try {
			long root = X11.INSTANCE.XDefaultRootWindow(display);
			Pointer[] children = new Pointer[1];
			int[] count = new int[1];

			// XQueryTree(root, &rootRet, &parentRet, &children, &count)
			if(X11.INSTANCE.XQueryTree(display, root, new long[1], new long[1], children, count) != 0) {
				Pointer childPtr = children[0];
				if(childPtr != null && count[0] > 0) {
					long[] childWindows = childPtr.getLongArray(0, count[0]);
					for(long window : childWindows) {
						WindowInfo info = describeWindow(display, window);
						if(info != null) result.add(info);
					}
				}
				if(childPtr != null) X11.INSTANCE.XFree(childPtr);
			}
		}
		catch(Throwable t) {
			LOGGER.warn("Failed to enumerate X11 windows", t);
		}
		finally {
			X11.INSTANCE.XCloseDisplay(display);
		}

		return result;
	}

	private static WindowInfo describeWindow(Pointer display, long window) {
		try {
			String title = fetchStringProperty(display, window, "WM_NAME");
			if(title == null || title.isEmpty()) return null;

			String wmClass = fetchStringProperty(display, window, "WM_CLASS");
			String appId = null;
			if(wmClass != null) {
				// WM_CLASS 是 NUL 分隔的 实例名\0类名\0，取第一个（实例名）作 appId
				int nul = wmClass.indexOf('\0');
				appId = (nul >= 0 ? wmClass.substring(0, nul) : wmClass).trim();
				if(appId.isEmpty()) appId = null;
			}

			int pid = 0;
			String pidStr = fetchCardinalProperty(display, window, "_NET_WM_PID");
			if(pidStr != null) {
				try {
					pid = Integer.parseInt(pidStr);
				}
				catch(NumberFormatException ignored) {}
			}

			return new WindowInfo(Long.toHexString(window), title, appId, pid);
		}
		catch(Throwable t) {
			return null;
		}
	}

	private static String fetchStringProperty(Pointer display, long window, String atomName) {
		long atom = X11.INSTANCE.XInternAtom(display, atomName, 1);
		if(atom == 0) return null;

		long[] type = new long[1];
		int[] format = new int[1];
		long[] nitems = new long[1];
		long[] bytesAfter = new long[1];
		Pointer[] prop = new Pointer[1];

		int status = X11.INSTANCE.XGetWindowProperty(display, window, atom, 0, 1024, 0,
				XA_STRING, type, format, nitems, bytesAfter, prop);
		if(status != 0 || prop[0] == null || nitems[0] <= 0) return null;

		try {
			return prop[0].getString(0);
		}
		finally {
			X11.INSTANCE.XFree(prop[0]);
		}
	}

	private static String fetchCardinalProperty(Pointer display, long window, String atomName) {
		long atom = X11.INSTANCE.XInternAtom(display, atomName, 1);
		if(atom == 0) return null;

		long[] type = new long[1];
		int[] format = new int[1];
		long[] nitems = new long[1];
		long[] bytesAfter = new long[1];
		Pointer[] prop = new Pointer[1];

		int status = X11.INSTANCE.XGetWindowProperty(display, window, atom, 0, 1, 0,
				XA_CARDINAL, type, format, nitems, bytesAfter, prop);
		if(status != 0 || prop[0] == null || nitems[0] <= 0) return null;

		try {
			if(format[0] == 32) return String.valueOf(prop[0].getInt(0));
			if(format[0] == 16) return String.valueOf(prop[0].getShort(0) & 0xFFFF);
			if(format[0] == 8) return String.valueOf(prop[0].getByte(0) & 0xFF);
			return null;
		}
		finally {
			X11.INSTANCE.XFree(prop[0]);
		}
	}
}
