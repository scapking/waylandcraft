package dev.evvie.waylandcraft.utils;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import com.sun.jna.Library;
import com.sun.jna.Native;
import com.sun.jna.Pointer;

/**
 * 通过 JNA XGetImage 抓取 X11 窗口像素（RGBA, top-down）。
 *
 * 用于 X11-only 应用（如微信）的窗口共享：这类窗口不在 xdg toplevel 列表里，
 * waylandcraft 的 framebuffer 抓帧拿不到，必须直接问 X server 要像素。
 *
 * 线程安全：本类的方法都打开独立 X 连接（XOpenDisplay/XCloseDisplay 配对），
 * 不持有跨调用状态，可以在任意线程调用。
 */
public class X11Capture {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-x11-capture");

	// XGetImage format: ZPixmap = 2
	private static final int ZPIXMAP = 2;

	private interface X11 extends Library {
		Pointer XOpenDisplay(String displayName);
		int XCloseDisplay(Pointer display);
		long XDefaultRootWindow(Pointer display);

		int XGetGeometry(Pointer display, long window, long[] rootReturn, int[] xReturn, int[] yReturn,
				int[] widthReturn, int[] heightReturn, int[] borderWidthReturn, int[] depthReturn);

		int XTranslateCoordinates(Pointer display, long srcW, long destW, int srcX, int srcY,
				int[] destXReturn, int[] destYReturn, long[] childReturn);

		/** 返回 XImage*（JNA Pointer）；失败返回 null */
		Pointer XGetImage(Pointer display, long window, int x, int y, int width, int height, long planeMask, int format);
		int XDestroyImage(Pointer image);
	}

	/**
	 * 惰性加载 libX11：无 X11 的平台（Windows/macOS/iOS viewer）返回 null，
	 * 所有方法先判空再调用，避免类初始化时 UnsatisfiedLinkError 崩客户端。
	 */
	private static final X11 X11_LIB = loadX11();

	private static X11 loadX11() {
		try {
			return Native.load("X11", X11.class);
		} catch(Throwable t) {
			LOGGER.debug("libX11 not available on this platform (X11 sharing disabled)", t);
			return null;
		}
	}

	/** 窗口几何信息（尺寸 + 根窗口坐标，供交互注入定位） */
	public record Geometry(long xid, int width, int height, int rootX, int rootY) {}

	/** 抓帧结果：RGBA 像素（top-down，每像素4字节）+ 尺寸 + 窗口根坐标（交互注入用） */
	public record Frame(ByteBuffer rgba, int width, int height, int rootX, int rootY) {}

	private X11Capture() {}

	/**
	 * 获取窗口几何信息：宽高 + 窗口原点在根窗口（屏幕）上的坐标。
	 * 窗口不存在/无法访问时返回 null。
	 */
	public static Geometry getGeometry(String displayName, long xid) {
		if(X11_LIB == null) {
			return null;
		}
		Pointer display = X11_LIB.XOpenDisplay(displayName);
		if(display == null) {
			LOGGER.debug("No X11 display available (requested '{}')", displayName);
			return null;
		}
		try {
			long root = X11_LIB.XDefaultRootWindow(display);
			long[] rootRet = new long[1];
			int[] x = new int[1];
			int[] y = new int[1];
			int[] w = new int[1];
			int[] h = new int[1];
			int[] bw = new int[1];
			int[] depth = new int[1];

			if(X11_LIB.XGetGeometry(display, xid, rootRet, x, y, w, h, bw, depth) == 0) {
				return null;
			}

			// 窗口原点换算到根窗口坐标（XGetGeometry 给的是相对父窗口的坐标，
			// 顶层窗口父是 root，直接可用；保险起见用 XTranslateCoordinates）
			int[] rootX = new int[1];
			int[] rootY = new int[1];
			long[] child = new long[1];
			if(X11_LIB.XTranslateCoordinates(display, xid, root, 0, 0, rootX, rootY, child) != 0) {
				return new Geometry(xid, w[0], h[0], rootX[0], rootY[0]);
			}
			return new Geometry(xid, w[0], h[0], x[0], y[0]);
		}
		catch(Throwable t) {
			LOGGER.warn("Failed to query X11 geometry for 0x{}", Long.toHexString(xid), t);
			return null;
		}
		finally {
			X11_LIB.XCloseDisplay(display);
		}
	}

	/**
	 * 抓取窗口当前像素，转为 RGBA（top-down）。
	 * 窗口不可见/最小化/已销毁时返回 null。
	 */
	public static Frame captureRgba(String displayName, long xid) {
		if(X11_LIB == null) {
			return null;
		}
		Pointer display = X11_LIB.XOpenDisplay(displayName);
		if(display == null) {
			return null;
		}
		try {
			long[] rootRet = new long[1];
			int[] x = new int[1];
			int[] y = new int[1];
			int[] w = new int[1];
			int[] h = new int[1];
			int[] bw = new int[1];
			int[] depth = new int[1];
			if(X11_LIB.XGetGeometry(display, xid, rootRet, x, y, w, h, bw, depth) == 0) {
				return null;
			}
			if(w[0] <= 0 || h[0] <= 0) {
				return null;
			}

			Pointer img = X11_LIB.XGetImage(display, xid, 0, 0, w[0], h[0], 0xffffffffL, ZPIXMAP);
			if(img == null) {
				return null;
			}
			try {
				Frame frame = convertToRgba(img, w[0], h[0]);
				if(frame == null) {
					return null;
				}
				// 附带窗口根坐标（交互注入定位用）
				int[] rootX = new int[1];
				int[] rootY = new int[1];
				long[] child = new long[1];
				if(X11_LIB.XTranslateCoordinates(display, xid, rootRet[0], 0, 0, rootX, rootY, child) != 0) {
					return new Frame(frame.rgba(), frame.width(), frame.height(), rootX[0], rootY[0]);
				}
				return new Frame(frame.rgba(), frame.width(), frame.height(), x[0], y[0]);
			}
			finally {
				X11_LIB.XDestroyImage(img);
			}
		}
		catch(Throwable t) {
			LOGGER.debug("Failed to capture X11 window 0x{}", Long.toHexString(xid), t);
			return null;
		}
		finally {
			X11_LIB.XCloseDisplay(display);
		}
	}

	/**
	 * 把 XImage 结构内存转成 RGBA ByteBuffer（top-down）。
	 *
	 * XImage 结构（64位 Linux）字段偏移：
	 *   0  width, 4 height, 8 xoffset, 12 format, 16 data(指针), 24 byte_order,
	 *   28 bitmap_unit, 32 bitmap_bit_order, 36 bitmap_pad, 40 depth,
	 *   44 bytes_per_line, 48 bits_per_pixel, 56 red_mask, 64 green_mask, 72 blue_mask
	 */
	private static Frame convertToRgba(Pointer img, int width, int height) {
		int byteOrder = img.getInt(24);
		int depth = img.getInt(40);
		int bytesPerLine = img.getInt(44);
		int bitsPerPixel = img.getInt(48);
		long redMask = img.getLong(56);
		long greenMask = img.getLong(64);
		long blueMask = img.getLong(72);
		Pointer data = img.getPointer(16);
		if(data == null) {
			return null;
		}

		// 只支持 32bpp（xwayland 默认 24 深/32bpp）与 24bpp
		if(bitsPerPixel != 32 && bitsPerPixel != 24) {
			LOGGER.warn("Unsupported X11 pixel depth: bpp={} depth={}", bitsPerPixel, depth);
			return null;
		}

		int bpp = bitsPerPixel / 8;
		int redShift = Long.numberOfTrailingZeros(redMask);
		int greenShift = Long.numberOfTrailingZeros(greenMask);
		int blueShift = Long.numberOfTrailingZeros(blueMask);
		// byte_order: 0 = LSBFirst, 1 = MSBFirst
		boolean msbFirst = (byteOrder == 1);

		ByteBuffer rgba = ByteBuffer.allocateDirect(width * height * 4).order(ByteOrder.nativeOrder());
		byte[] row = new byte[Math.max(bytesPerLine, width * bpp)];

		for(int rowIdx = 0; rowIdx < height; rowIdx++) {
			// 服务器像素内存按 byte_order 存储；x86 上 LSBFirst == Java native order。
			// 直接 memcpy 到 byte[]，再按掩码提取。
			data.read((long) rowIdx * bytesPerLine, row, 0, bytesPerLine);

			int base = rowIdx * width * 4;
			for(int col = 0; col < width; col++) {
				int pixel;
				int off = col * bpp;
				if(msbFirst) {
					if(bpp == 4) {
						pixel = ((row[off] & 0xFF) << 24)
								| ((row[off + 1] & 0xFF) << 16)
								| ((row[off + 2] & 0xFF) << 8)
								| (row[off + 3] & 0xFF);
					} else {
						pixel = ((row[off] & 0xFF) << 16)
								| ((row[off + 1] & 0xFF) << 8)
								| (row[off + 2] & 0xFF);
					}
				} else if(bpp == 4) {
					pixel = (row[off] & 0xFF)
							| ((row[off + 1] & 0xFF) << 8)
							| ((row[off + 2] & 0xFF) << 16)
							| ((row[off + 3] & 0xFF) << 24);
				} else {
					pixel = (row[off] & 0xFF)
							| ((row[off + 1] & 0xFF) << 8)
							| ((row[off + 2] & 0xFF) << 16);
				}

				rgba.put(base + col * 4, (byte) ((pixel & redMask) >>> redShift));
				rgba.put(base + col * 4 + 1, (byte) ((pixel & greenMask) >>> greenShift));
				rgba.put(base + col * 4 + 2, (byte) ((pixel & blueMask) >>> blueShift));
				rgba.put(base + col * 4 + 3, (byte) 0xFF);
			}
		}
		return new Frame(rgba, width, height, 0, 0);
	}
}
