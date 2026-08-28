package dev.evvie.waylandcraft.bridge;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.Iterator;
import java.util.LinkedList;
import java.util.List;
import java.util.Map;
import java.util.stream.Collectors;
import java.util.stream.Stream;

import org.apache.commons.lang3.ArrayUtils;
import org.jetbrains.annotations.Nullable;
import org.lwjgl.glfw.GLFW;
import org.lwjgl.glfw.GLFWNativeEGL;
import org.lwjgl.glfw.GLFWNativeWayland;
import org.lwjgl.system.Platform;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.bridge.WLCAbstractWindow.SurfaceGeometry;
import dev.evvie.waylandcraft.desktop.RawDesktopEntry;
import dev.evvie.waylandcraft.render.BufferTexture.DmabufTexture;
import dev.evvie.waylandcraft.render.WindowFramebuffer;
import dev.evvie.waylandcraft.utils.CursorShape;
import net.minecraft.util.profiling.Profiler;
import net.minecraft.util.profiling.ProfilerFiller;

public class WaylandCraftBridge {
	
	private long instance;
	private ArrayList<WLCToplevel> toplevels = new ArrayList<WLCToplevel>();
	private ArrayList<WLCPopup> popups = new ArrayList<WLCPopup>();
	private ArrayList<WLCSurface> surfaces = new ArrayList<WLCSurface>();
	private ArrayList<DmabufTexture> dmabufs = new ArrayList<DmabufTexture>();
	private ArrayList<WindowFramebuffer> framebuffers = new ArrayList<WindowFramebuffer>();
	
	public IconSurface dndIcon = null;
	
	private LinkedList<WLCToplevel> focusOrder = new LinkedList<WLCToplevel>();
	
	private ArrayList<WLCToplevel> newToplevels = new ArrayList<WLCToplevel>();
	
	private @Nullable Integer lastMoveRequestSerial = null;
	private @Nullable ResizeRequest lastResizeRequest = null;
	
	/** False when the native library could not be loaded (e.g. Android launchers
	 * with a bionic runtime). The mod then disables itself instead of crashing. */
	private static boolean nativeAvailable = false;
	public static boolean isNativeAvailable() { return nativeAvailable; }
	
	static {
		boolean loaded = false;
		InputStream inputStream = openNativeLibraryFromJar();
		if(inputStream != null) {
			try {
				byte[] data = inputStream.readAllBytes();
				inputStream.close();
				
				File temp = File.createTempFile("waylandcraft-", "-libwaylandcraft.so");
				temp.deleteOnExit();
				
				FileOutputStream outputStream = new FileOutputStream(temp);
				outputStream.write(data);
				outputStream.close();
				
				loadNativeLibrary(temp);
				loaded = true;
				
				WaylandCraftCommon.LOGGER.info("Loaded native library from jar");
			} catch (IOException e) {
				e.printStackTrace();
			} catch (UnsatisfiedLinkError e) {
				WaylandCraftCommon.LOGGER.error("Native library is unavailable on this platform: {}", e.getMessage());
			}
		}
		
		if(!loaded) {
			WaylandCraftCommon.LOGGER.info("Native library could not be loaded from jar. Attempting to load from system");
			try {
				loadNativeLibrary(null);
				loaded = true;
			} catch (UnsatisfiedLinkError e) {
				WaylandCraftCommon.LOGGER.error("Native library is unavailable on this platform: {}", e.getMessage());
			}
		}
		
		nativeAvailable = loaded;
		
		if(loaded) {
			// 解压内嵌的 xwayland-satellite 二进制（如 jar 里带的话），
			// 让 X11 应用无需用户手动安装 satellite 也能拿到 DISPLAY。
			extractSatelliteBinary();
		}
	}
	
	/**
	 * 从 jar 资源里解压内嵌的 xwayland-satellite 可执行文件到临时目录并 chmod +x，
	 * 然后通过 JNI 把路径交给 native 层。找不到内嵌二进制（如非 x86_64/arm64 平台）
	 * 时静默跳过，native 层会 fallback 到系统 PATH。
	 */
	private static void extractSatelliteBinary() {
		try {
			String resource = satelliteResourcePath();
			if(resource == null) {
				WaylandCraftCommon.LOGGER.info("No bundled xwayland-satellite for this platform; will fall back to system PATH");
				return;
			}
			
			InputStream inputStream = loadResource(resource);
			if(inputStream == null) {
				WaylandCraftCommon.LOGGER.warn("Bundled xwayland-satellite resource '{}' not found; will fall back to system PATH", resource);
				return;
			}
			
			byte[] data = inputStream.readAllBytes();
			inputStream.close();
			
			File temp = File.createTempFile("waylandcraft-", "-xwayland-satellite");
			temp.deleteOnExit();
			
			FileOutputStream outputStream = new FileOutputStream(temp);
			outputStream.write(data);
			outputStream.close();
			
			if(!temp.setExecutable(true, true)) {
				WaylandCraftCommon.LOGGER.warn("Failed to chmod +x bundled xwayland-satellite at {}", temp.getAbsolutePath());
			}
			
			setSatelliteBinary(temp.getAbsolutePath());
			WaylandCraftCommon.LOGGER.info("Bundled xwayland-satellite extracted to {}", temp.getAbsolutePath());
		} catch (IOException e) {
			WaylandCraftCommon.LOGGER.warn("Failed to extract bundled xwayland-satellite: {}", e.toString());
		}
	}
	
	private static String satelliteResourcePath() {
		String arch;
		switch(Platform.getArchitecture()) {
		case X64: arch = "x86_64"; break;
		case ARM64: arch = "arm64"; break;
		default: return null;
		}
		return "/xwayland-satellite-linux-gnu-" + arch;
	}
	
	private static InputStream loadResource(String path) {
		WaylandCraftCommon.LOGGER.info("Looking for '" + path + "'...");
		return WaylandCraftBridge.class.getResourceAsStream(path);
	}
	
	/** Detect the Android runtime (bionic libc). The JVM reports os.name "Linux",
	 * so we probe for the canonical Android marker file instead. */
	private static boolean isAndroid() {
		try {
			return new File("/system/build.prop").exists();
		} catch (Throwable t) {
			return false;
		}
	}

	/** Detect Windows. */
	private static boolean isWindows() {
		String os = System.getProperty("os.name", "");
		return os.toLowerCase().contains("win");
	}

	/** Detect macOS. */
	private static boolean isMac() {
		String os = System.getProperty("os.name", "");
		String lower = os.toLowerCase();
		return lower.contains("mac") || lower.contains("darwin");
	}

	/**
	 * Detect iOS (PojavLauncher / Amethyst runtime). The JVM there reports
	 * os.name "Linux", so we probe for canonical iOS marker files as well.
	 */
	private static boolean isIOS() {
		String os = System.getProperty("os.name", "");
		if(os.toLowerCase().contains("ios")) return true;
		try {
			return new File("/System/Library/CoreServices/SystemVersion.plist").exists()
					|| new File("/var/mobile").exists();
		} catch (Throwable t) {
			return false;
		}
	}

	/**
	 * Native lib name platform component, e.g. "linux-gnu" or "android".
	 * Returns null on platforms with no native capture support (Windows/macOS/iOS):
	 * the mod then runs in viewer-only mode — local window capture is unavailable,
	 * but shared windows can still be received and rendered.
	 */
	private static String nativePlatform() {
		if(isWindows() || isMac() || isIOS()) return null;
		return isAndroid() ? "android" : "linux-gnu";
	}
	
	private static InputStream openNativeLibraryFromJar() {
		InputStream stream = null;
		
		/* Attempt to load the architecture-specific release library first.
		 * A jar may contain multiple builds (libwaylandcraft-linux-gnu-x86_64.so /
		 * libwaylandcraft-linux-gnu-arm64.so / libwaylandcraft-android-*.so); the
		 * unqualified /libwaylandcraft.so is only the build host's library and must
		 * NOT be preferred, otherwise an arm64 device will try to load an x86_64
		 * build. On Android (bionic) we prefer the android build; on desktop the
		 * linux-gnu build. */
		String arch;
		switch(Platform.getArchitecture()) {
		case X64: arch = "x86_64"; break;
		case ARM64: arch = "arm64"; break;
		default: arch = null; break;
		}
		
		if(arch != null) {
			String platform = nativePlatform();
			if(platform != null) {
				String full = platform + "-" + arch;
				stream = loadResource("/libwaylandcraft-" + full + ".so");
				if(stream != null) return stream;
				
				// Fall back to the other platform's build (e.g. an android jar running
				// in an emulator that reports linux-gnu, or a linux jar on android).
				String otherPlatform = (isAndroid() ? "linux-gnu" : "android") + "-" + arch;
				stream = loadResource("/libwaylandcraft-" + otherPlatform + ".so");
				if(stream != null) return stream;
			} else {
				WaylandCraftCommon.LOGGER.info("WaylandCraft local window capture is not supported on this OS; running in viewer-only mode");
			}
		}
		
		/* Attempt to load manually built native library (fallback) */
		stream = loadResource("/libwaylandcraft.so");
		if(stream != null) return stream;
		
		return null;
	}
	
	/**
	 * Load the native library, retrying with bundled dependency .so files
	 * (libxkbcommon/libpipewire/...) if the first attempt fails with
	 * UnsatisfiedLinkError. This is needed on stripped runtimes such as Android
	 * launchers where glibc exists but libxkbcommon.so.0 / libpipewire-0.3.so.0
	 * are absent. Desktop systems resolve their deps from the OS on the first
	 * try and never touch the bundled deps. Pass null for libFile to load from
	 * the system library path (System.loadLibrary fallback).
	 */
	private static void loadNativeLibrary(File libFile) {
		try {
			if(libFile != null) {
				System.load(libFile.getAbsolutePath());
			} else {
				System.loadLibrary("waylandcraft");
			}
		} catch (UnsatisfiedLinkError e) {
			WaylandCraftCommon.LOGGER.warn("Native library load failed ({}); retrying after preloading bundled dependencies...", e.getMessage());
			if(loadBundledNativeDeps()) {
				if(libFile != null) {
					System.load(libFile.getAbsolutePath());
				} else {
					System.loadLibrary("waylandcraft");
				}
				return;
			}
			throw e;
		}
	}
	
	/**
	 * Extract and load the bundled native dependencies listed in the manifest
	 * /native-deps/&lt;linux-gnu-arch&gt;/deps.list (written by CI's collect_deps.py).
	 * The libs are loaded in a dependency-safe retry loop: a lib whose own deps
	 * aren't loaded yet simply fails and is retried on the next pass. Returns
	 * true only if every bundled dep loaded. The manifest is read via
	 * getResourceAsStream because the class's code source location is not
	 * reliably available inside Fabric's KnotClassLoader.
	 */
	private static boolean loadBundledNativeDeps() {
		String arch;
		switch(Platform.getArchitecture()) {
		case X64: arch = "x86_64"; break;
		case ARM64: arch = "arm64"; break;
		default: return false;
		}
		// On Android (bionic) prefer the android bundle (built against bionic libc);
		// fall back to the linux-gnu bundle for jars that only ship glibc deps.
		String resourceDir = "/native-deps/" + nativePlatform() + "-" + arch + "/";
		InputStream manifestStream = loadResource(resourceDir + "deps.list");
		if(manifestStream == null && isAndroid()) {
			resourceDir = "/native-deps/linux-gnu-" + arch + "/";
			manifestStream = loadResource(resourceDir + "deps.list");
		}
		
		List<String> names = new ArrayList<String>();
		try {
			if(manifestStream == null) {
				WaylandCraftCommon.LOGGER.info("No bundled native dependency manifest under {}", resourceDir);
				return false;
			}
			BufferedReader reader = new BufferedReader(new InputStreamReader(manifestStream));
			String line;
			while((line = reader.readLine()) != null) {
				line = line.trim();
				if(!line.isEmpty()) names.add(line);
			}
			manifestStream.close();
		} catch (IOException e) {
			WaylandCraftCommon.LOGGER.warn("Failed to read bundled native dependency manifest: {}", e.toString());
			return false;
		}
		if(names.isEmpty()) {
			WaylandCraftCommon.LOGGER.info("Bundled native dependency manifest is empty");
			return false;
		}
		
		List<File> files = new ArrayList<File>();
		for(String name : names) {
			try {
				InputStream inputStream = loadResource(resourceDir + name);
				if(inputStream == null) {
					WaylandCraftCommon.LOGGER.warn("Bundled native dep resource '{}' not found", resourceDir + name);
					continue;
				}
				byte[] data = inputStream.readAllBytes();
				inputStream.close();
				
				File temp = File.createTempFile("waylandcraft-", "-" + name);
				temp.deleteOnExit();
				
				FileOutputStream outputStream = new FileOutputStream(temp);
				outputStream.write(data);
				outputStream.close();
				
				files.add(temp);
			} catch (IOException e) {
				WaylandCraftCommon.LOGGER.warn("Failed to extract bundled native dep '{}': {}", name, e.toString());
			}
		}
		
		if(files.isEmpty()) return false;
		
		List<File> remaining = new ArrayList<File>(files);
		Map<String, String> errors = new HashMap<String, String>();
		for(int pass = 0; pass <= remaining.size(); pass++) {
			boolean progress = false;
			Iterator<File> it = remaining.iterator();
			while(it.hasNext()) {
				File f = it.next();
				try {
					System.load(f.getAbsolutePath());
					WaylandCraftCommon.LOGGER.info("Loaded bundled native dependency {}", f.getName());
					it.remove();
					progress = true;
				} catch (UnsatisfiedLinkError e) {
					// dependency not loaded yet; retry on the next pass
					errors.put(f.getName(), e.getMessage());
				}
			}
			if(remaining.isEmpty()) return true;
			if(!progress) break;
		}
		for(File f : remaining) {
			WaylandCraftCommon.LOGGER.warn("Could not load bundled native dependency {}: {}", f.getAbsolutePath(), errors.getOrDefault(f.getName(), "unknown error"));
		}
		return false;
	}
	
	private WaylandCraftBridge(long instance) {
		this.instance = instance;
	}
	
	public static WaylandCraftBridge start() {
		if(!nativeAvailable) {
			throw new UnsatisfiedLinkError("waylandcraft native library is not available on this platform");
		}
		
		long eglDisplay = GLFWNativeEGL.glfwGetEGLDisplay();
		if(eglDisplay == 0) {
			throw new RuntimeException("Failed to get EGL display!");
		}

		// Wayland 后端下拿到 wl_display 指针，供 native 做系统输入法穿透；
		// X11/XWayland 后端返回 0，穿透自动禁用（graceful fallback）。
		long waylandDisplay = GLFWNativeWayland.glfwGetWaylandDisplay();
		WaylandCraftCommon.LOGGER.info("[waylandcraft][system_ime] Java side: eglDisplay=0x{} waylandDisplay=0x{} (0 == X11/XWayland backend)", Long.toHexString(eglDisplay), Long.toHexString(waylandDisplay));

		long handle = init(GLFW.Functions.GetProcAddress, eglDisplay, waylandDisplay);
		return new WaylandCraftBridge(handle);
	}
	
	protected WLCToplevel getOrCreateToplevel(long topLevelHandle) {
		for(WLCToplevel toplevel : toplevels) {
			if(toplevel.getHandle() == topLevelHandle) return toplevel;
		}
		WLCToplevel toplevel = new WLCToplevel(topLevelHandle);
		
		long surfaceHandle = toplevelSurface(this.instance, topLevelHandle);
		WLCSurface surface = getOrCreateSurface(surfaceHandle);
		toplevel.surface = surface;
		
		toplevels.add(toplevel);
		return toplevel;
	}
	
	public WLCToplevel[] getNewToplevels() {
		WLCToplevel[] toplevels = newToplevels.toArray(WLCToplevel[]::new);
		newToplevels.clear();
		
		return toplevels;
	}
	
	protected WLCPopup getOrCreatePopup(long handle) {
		for(WLCPopup popup : popups) {
			if(popup.getHandle() == handle) return popup;
		}
		WLCPopup popup = new WLCPopup(handle);
		
		long surfaceHandle = popupSurface(this.instance, handle);
		WLCSurface surface = getOrCreateSurface(surfaceHandle);
		popup.surface = surface;
		
		popup.parentHandle = popupParent(this.instance, handle);
		
		popups.add(popup);
		return popup;
	}
	
	protected WLCSurface getOrCreateSurface(long handle) {
		for(WLCSurface surface : surfaces) {
			if(surface.getHandle() == handle) return surface;
		}
		WLCSurface surface = new WLCSurface(handle);
		surfaces.add(surface);
		return surface;
	}
	
	protected DmabufTexture getDmabuf(long handle) {
		for(DmabufTexture dmabuf : dmabufs) {
			if(dmabuf.handle == handle) return dmabuf;
		}
		return null;
	}
	
	protected void addDmabuf(DmabufTexture dmabuf) {
		dmabufs.add(dmabuf);
	}
	
	private void deleteNonExistingToplevels(long[] remainingHandles) {
		ArrayList<WLCToplevel> toplevels_new = new ArrayList<WLCToplevel>();
		for(WLCToplevel toplevel : this.toplevels) {
			if(ArrayUtils.contains(remainingHandles, toplevel.getHandle())) {
				toplevels_new.add(toplevel);
			}
			else {
				freeToplevel(this.instance, toplevel.takeHandle());
			}
		}
		this.toplevels = toplevels_new;
	}
	
	private void deleteNonExistingPopups(long[] remainingHandles) {
		ArrayList<WLCPopup> popups_new = new ArrayList<WLCPopup>();
		for(WLCPopup popup : this.popups) {
			if(ArrayUtils.contains(remainingHandles, popup.getHandle())) {
				popups_new.add(popup);
			}
			else {
				freePopup(this.instance, popup.takeHandle());
			}
		}
		this.popups = popups_new;
	}
	
	private void updateDmabufs() {
		long[] remainingHandles = dmabufs(instance);
		ArrayList<DmabufTexture> dmabufs_new = new ArrayList<DmabufTexture>();
		for(DmabufTexture dmabuf : this.dmabufs) {
			// If the dmabuf texture is not attached to a real wl_buffer anymore, free the EGL resources
			boolean retained = ArrayUtils.contains(remainingHandles, dmabuf.handle);
			if(!retained) dmabuf.freeEGL();
			
			// Remove it from the list and free the texture if no longer attached to any surface
			boolean used = false;
			for(WLCSurface surface : surfaces) {
				if(surface.getBuffer() == dmabuf) {
					used = true;
					break;
				}
			}
			if(retained || used) {
				dmabufs_new.add(dmabuf);
			}
			else {
				dmabuf.doReleaseTexure();
			}
		}
		this.dmabufs = dmabufs_new;
	}
	
	private void deleteUnvisitedSurfaces() {
		ArrayList<WLCSurface> surfaces_new = new ArrayList<WLCSurface>();
		for(WLCSurface surface : this.surfaces) {
			if(surface.visited) {
				surfaces_new.add(surface);
			}
			else {
				freeSurface(this.instance, surface.takeHandle());
			}
		}
		this.surfaces = surfaces_new;
	}
	
	private void findPopupParent(WLCPopup popup) {
		// Popups cannot change their parent, so if one is found, it's the one
		if(popup.parent != null) return;
		
		for(WLCToplevel toplevel : toplevels) {
			if(toplevel.getHandle() == popup.parentHandle) {
				popup.parent = toplevel;
				return;
			}
		}
		
		for(WLCPopup popup2 : popups) {
			if(popup2.getHandle() == popup.parentHandle) {
				popup.parent = popup2;
				return;
			}
		}
	}
	
	public void update() {
		ProfilerFiller profiler = Profiler.get();
		profiler.push("wayland");
		
		// Update wayland clients
		profiler.push("update");
		update(this.instance);
		profiler.pop();
		
		// Find all available toplevels and delete ones that no longer exist
		long[] toplevelHandles = toplevels(instance);
		deleteNonExistingToplevels(toplevelHandles);
		
		// Find all available popups and delete ones that no longer exist
		long[] popupHandles = popups(instance);
		deleteNonExistingPopups(popupHandles);
		
		long[] minimizeRequests = minimizeReq(instance);
		long[] maximizeRequests = maximizeReq(instance);
		long[] unmaximizeRequests = unmaximizeReq(instance);
		long[] fullscreenRequests = fullscreenReq(instance);
		long[] unfullscreenRequests = unfullscreenReq(instance);
		long[] fullscreened = fullscreened(instance);
		
		int[] moveRequest = moveRequest(instance);
		if(moveRequest != null) {
			lastMoveRequestSerial = moveRequest[0];
		}
		
		int[] resizeRequest = resizeRequest(instance);
		if(resizeRequest != null) {
			lastResizeRequest = new ResizeRequest(resizeRequest[0], resizeRequest[1]);
		}
		
		// Reset surface visited state
		for(WLCSurface surface : surfaces) {
			surface.visited = false;
		}
		
		profiler.push("surfaces");
		// Create new toplevels when necessary
		// Update surface tree geometry and properties of all toplevels
		for(long handle : toplevelHandles) {
			WLCToplevel toplevel = getOrCreateToplevel(handle);
			WLCSurface root = toplevel.getSurfaceTree();
			toplevel.lastChild = updateSurfaceTree(this.instance, root);
			
			updateGeometry(toplevel);
			toplevel.title = toplevelTitle(toplevel.getHandle());
			toplevel.appID = toplevelAppID(toplevel.getHandle());
			toplevel.pid = toplevelPid(this.instance, toplevel.getHandle());
			
			if(ArrayUtils.contains(minimizeRequests, handle)) toplevel.requests.minimize = true;
			if(ArrayUtils.contains(maximizeRequests, handle)) toplevel.requests.maximize= true;
			if(ArrayUtils.contains(unmaximizeRequests, handle)) toplevel.requests.unmaximize = true;
			if(ArrayUtils.contains(fullscreenRequests, handle)) toplevel.requests.fullscreen = true;
			if(ArrayUtils.contains(unfullscreenRequests, handle)) toplevel.requests.unfullscreen = true;
			
			toplevel.fullscreen = ArrayUtils.contains(fullscreened, handle);
		}
		
		// Create new popups when necessary
		// Update surface tree geometry, parent relationships and offsets of all popups
		for(long handle : popupHandles) {
			WLCPopup popup = getOrCreatePopup(handle);
			findPopupParent(popup);
			
			int[] offset = popupOffset(handle);
			popup.offsetX = offset[0];
			popup.offsetY = offset[1];
			
			WLCSurface root = popup.getSurfaceTree();
			popup.lastChild = updateSurfaceTree(this.instance, root);
			updateGeometry(popup);
		}
		
		long dndIconHandle = dndIcon(instance);
		if(dndIconHandle != 0) {
			WLCSurface dndIconSurface = getOrCreateSurface(dndIconHandle);
			if(dndIcon != null && dndIcon.surface != dndIconSurface) dndIcon = null;
			if(dndIcon == null) dndIcon = new IconSurface(dndIconSurface);
			
			updateSurfaceData(instance, dndIcon.surface);
			dndIcon.surface.visited = true;
		}
		else {
			dndIcon = null;
		}
		
		// All surface trees have now been walked. Now delete all unvisited surfaces
		deleteUnvisitedSurfaces();
		profiler.pop();
		
		// Resolve surface parent handles to actual surfaces
		for(WLCSurface surface : surfaces) {
			if(surface.parentHandle != 0) {
				surface.parent = getOrCreateSurface(surface.parentHandle);
			}
			else {
				surface.parent = null;
			}
		}
		
		List<WLCAbstractWindow> allWindows = Stream.of(toplevels, popups).flatMap((l) -> l.stream()).collect(Collectors.toList());
		
		// Update all surface buffers
		for(WLCAbstractWindow window : allWindows) {
			WLCSurface root = window.getSurfaceTree();
			for(WLCSurface surface = root; surface != null; surface = surface.getNextChild()) {
				updateSurfaceData(instance, surface);
				calculateSubpos(surface);
			}
		}
		
		for(WLCToplevel toplevel : toplevels) {
			boolean mapped = toplevel.isMapped();
			if(mapped && !toplevel.wasMapped) {
				newToplevels.add(toplevel);
			}
			toplevel.wasMapped = mapped;
		}
		
		profiler.push("framebuffer");
		updateFramebuffers();
		profiler.pop();
		
		updateDmabufs();
		
		updateFocusOrder();
		
		// Do client frame callbacks
		for(WLCSurface surface : surfaces) {
			sendFrame(surface.getHandle());
		}
		
		profiler.pop();
	}
	
	private void updateFramebuffers() {
		List<WLCAbstractWindow> allWindows = Stream.of(toplevels, popups).flatMap((l) -> l.stream()).collect(Collectors.toList());
		
		// Render windows
		for(WLCAbstractWindow window : allWindows) {
			if(window.framebuffer == null) {
				window.framebuffer = new WindowFramebuffer(window.getSurfaceTree());
				framebuffers.add(window.framebuffer);
			}
			window.framebuffer.render();
		}
		
		// Render dnd icon
		if(dndIcon != null) {
			if(dndIcon.framebuffer == null) {
				dndIcon.framebuffer = new WindowFramebuffer(dndIcon.surface);
				framebuffers.add(dndIcon.framebuffer);
			}
			dndIcon.framebuffer.render();
		}
		
		// Cleanup unused framebuffers
		ArrayList<WindowFramebuffer> usedFramebuffers = new ArrayList<WindowFramebuffer>();
		for(WindowFramebuffer framebuffer : framebuffers) {
			if(framebuffer.surfaceTree.isAlive()) {
				usedFramebuffers.add(framebuffer);
			}
			else {
				framebuffer.destroy();
			}
		}
		framebuffers.retainAll(usedFramebuffers);
		
		WindowFramebuffer.endFrame();
	}
	
	private void updateGeometry(WLCAbstractWindow window) {
		int[] data = surfaceXDGGeometry(window.surface.getHandle());
		SurfaceGeometry geometry;
		
		if(data == null) {
			geometry = new SurfaceGeometry(0, 0, window.surface.width(), window.surface.height());
		}
		else {
			geometry = new SurfaceGeometry(data[0], data[1], data[2], data[3]);
		}
		
		window.geometry = geometry;
	}
	
	private void calculateSubpos(WLCSurface surface) {
		if(surface.parent != null) {
			calculateSubpos(surface.parent);
			surface.xSubpos = surface.parent.xSubpos + surface.xoff;
			surface.ySubpos = surface.parent.ySubpos + surface.yoff;
		}
		else {
			surface.xSubpos = 0;
			surface.ySubpos = 0;
		}
	}
	
	public WLCToplevel[] getToplevels() {
		return toplevels.toArray(new WLCToplevel[toplevels.size()]);
	}
	
	public WLCToplevel[] getMappedToplevels() {
		return toplevels.stream().filter((t) -> t.isMapped()).toArray(WLCToplevel[]::new);
	}
	
	public WLCToplevel getToplevel(long handle) {
		return toplevels.stream().filter((w) -> w.getHandle() == handle).findAny().orElse(null);
	}
	
	/**
	 * 注册捕获的虚拟 Toplevel
	 */
	public void registerCapturedToplevel(WLCToplevel toplevel) {
		toplevels.add(toplevel);
		newToplevels.add(toplevel);
	}
	
	/**
	 * 注销捕获的虚拟 Toplevel
	 */
	public void unregisterCapturedToplevel(WLCToplevel toplevel) {
		toplevels.remove(toplevel);
		newToplevels.remove(toplevel);
		focusOrder.remove(toplevel);
	}
	
	/**
	 * 获取桌面窗口列表（通过 /proc）
	 * 返回格式：["pid:cmdline", ...]
	 */
	public String[] getDesktopWindows() {
		return getDesktopWindows(this.instance);
	}
	
	public WLCPopup[] getPopups() {
		return popups.toArray(new WLCPopup[popups.size()]);
	}
	
	public WLCPopup[] getMappedPopups() {
		return popups.stream().filter((t) -> t.isMapped()).toArray(WLCPopup[]::new);
	}
	
	public String getSocket() {
		return socket(this.instance);
	}
	
	/** 获取 xwayland-satellite 的 X display（如 ":2"）；未启动 satellite 时返回空串 */
	public String getSatelliteDisplay() {
		return getSatelliteDisplay(this.instance);
	}
	
	/** 获取原生 wayland 窗口所属客户端进程 PID（SO_PEERCRED）；0 = 未知/X11 窗口 */
	public int toplevelPid(long topLevelHandle) {
		return toplevelPid(this.instance, topLevelHandle);
	}
	
	public boolean inputRegionContains(WLCSurface surface, double x, double y) {
		return checkInputRegion(surface.getHandle(), x, y);
	}
	
	public void sendMotion(double x, double y) {
		pointerMotion(instance, x, y);
	}
	
	public void sendMotionRefocus(WLCSurface surface, double x, double y) {
		pointerMotionFocus(instance, surface.getHandle(), x, y);
	}
	
	public void sendRelativeMotion(double dx, double dy) {
		pointerRelMotion(instance, dx, dy);
	}
	
	public void sendMotionOutside() {
		pointerLeave(instance);
	}
	
	public boolean maybeLockPointer(WLCSurface surface) {
		return maybePointerLock(instance, surface.getHandle());
	}
	
	public void unlockPointer() {
		pointerUnlock(instance);
	}
	
	public int sendButton(int button, int state) {
		return pointerButton(instance, button, state);
	}
	
	public void sendScroll(int axis, double value) {
		pointerAxis(instance, axis, value);
	}
	
	public CursorShape getCursorShape() {
		return CursorShape.fromId(cursorShape(instance));
	}
	
	public void focusSurface(@Nullable WLCToplevel toplevel) {
		long handle = 0;
		if(toplevel != null) {
			handle = toplevel.getHandle();
		}
		
		keyboardFocus(instance, handle);
		
		// Make toplevel most recently focused
		if(toplevel != null) {
			focusOrder.remove(toplevel);
			focusOrder.addLast(toplevel);
		}
	}
	
	public void activateKeyboard() {
		keyboardActivate(instance);
	}
	
	public void deactivateKeyboard() {
		keyboardDeactivate(instance);
	}
	
	private void updateFocusOrder() {
		focusOrder.removeIf((t) -> !toplevels.contains(t));
		for(WLCToplevel toplevel : toplevels) {
			if(!focusOrder.contains(toplevel)) focusOrder.addLast(toplevel);
		}
	}
	
	// Find the most recently focused toplevel that exists
	public WLCToplevel getMostRecentFocus() {
		updateFocusOrder();
		return focusOrder.peekLast();
	}
	
	// Find the most recently focused toplevel that exists
	public Stream<WLCToplevel> getMostToLeastRecentFocus() {
		updateFocusOrder();
		return focusOrder.reversed().stream();
	}
	
	public void pressKey(int scancode) {
		keyboardInput(instance, scancode, 1);
	}
	
	public void releaseKey(int scancode) {
		keyboardInput(instance, scancode, 0);
	}
	
	/** 重复按键（长按）透传：Rust keyboardInput 三态 action=2（REPEAT），
	 * 由 xkb 状态机在 repeat 后输出对应字符/修饰位。REPEAT 不更新 xkb_state，
	 * 只把事件发进窗口（修复长按失效根因）。 */
	public void repeatKey(int scancode) {
		keyboardInput(instance, scancode, 2);
	}
	
	public void internalKeyUpdate(int scancode, boolean pressed) {
		keyboardUpdate(instance, scancode, pressed);
	}
	
	/** 让 Rust 侧 [kb-debug] 日志同时写入指定文件（默认只进 stderr）。
	 * bridge 初始化后立即调用，路径用 .minecraft/waylandcraft-kb.log，
	 * 用户上传该文件即可定位 Rust 侧键盘链路。 */
	public void setKbLogFile(String path) {
		setKbLogFileNative(path);
	}
	
	/** 让 Rust 侧 [audio] 全链路日志同时写入指定文件（默认只进 stderr）。
	 * bridge 初始化后立即调用，路径用 .minecraft/waylandcraft-audio.log。
	 * 覆盖：PID→拓扑枚举→节点匹配→stream 建连→pw-link→process 回调。 */
	public void setAudioLogFile(String path) {
		setAudioLogFileNative(path);
	}
	
	/** 让 Rust 侧 [system_ime] 全链路日志同时写入指定文件（默认只进 stderr）。
	 * bridge 初始化后立即调用，路径用 .minecraft/waylandcraft-ime.log。
	 * 覆盖：probe→connect→registry/globals→enter/leave→enable 状态机→
	 * commit/preedit→错误。任何一步失败都能从该文件瞬间定位。 */
	public void setImeLogFile(String path) {
		setImeLogFileNative(path);
	}
	
	/** 静态版：必须在 WaylandCraftBridge.start()（native 初始化）之前调用，
	 * 这样 SystemIme 的 BUILD/PHASE 初始化日志也能写入日志文件。 */
	public static void setImeLogFileStatic(String path) {
		setImeLogFileNative(path);
	}

	/** Minecraft 窗口重新获得 OS 键盘焦点时调用（GLFW focus 回调驱动）。
	 * 输入法穿透层据此做一次性事件驱动的焦点重协商：
	 * 若穿透 text_input 因创建晚于宿主焦点分配而收不到 enter
	 * （KWin 已知行为），在此重建 text_input 触发宿主重新评估。
	 * 替代已删除的 15 秒定时轮询。 */
	public void notifyHostFocusGained() {
		notifyHostFocusGainedNative(instance);
	}

	/** 取候选窗快照（JSON）。Java 每帧轮询；返回空串表示无新快照。
	 * 字段：visible / cursor / page_size / orientation / candidates / labels。
	 * 数据源：ibus UpdateLookupTable / fcitx5 UpdateClientSideUI 归一化。 */
	public String takeLookupTable() {
		return takeLookupTableNative(instance);
	}

	/** 候选窗用户操作 → 宿主输入法。
	 * action: 0=选字(arg=当前页内下标) 1=上一页 2=下一页。
	 * fcitx5 走 SelectCandidate/PrevPage/NextPage 专用方法；ibus portal
	 * 无候选方法（忽略，候选操作走按键通路）。 */
	public void candidateNav(int action, int arg) {
		candidateNavNative(instance, action, arg);
	}
	
	/** 查询 Rust 侧音频捕获链路状态（JSON 字符串），供 /wl audio status 展示。 */
	public String audioCaptureStatus() {
		return audioCaptureStatusNative(instance);
	}
	
	public void resizeToplevelInteractive(WLCToplevel toplevel, int width, int height) {
		toplevelResize(toplevel.getHandle(), width, height, true);
	}
	
	public void resizeToplevel(WLCToplevel toplevel, int width, int height) {
		toplevelResize(toplevel.getHandle(), width, height, false);
	}
	
	public void resizeToplevelOverride(WLCToplevel toplevel, int width, int height) {
		toplevelResizeOvr(toplevel.getHandle(), width, height);
	}
	
	public void maximizeToplevel(WLCToplevel toplevel) {
		toplevelMaximize(instance, toplevel.getHandle());
	}
	
	public void fullscreenToplevel(WLCToplevel toplevel) {
		toplevelFullscreen(instance, toplevel.getHandle());
	}
	
	public Integer checkMoveRequest() {
		if(lastMoveRequestSerial == null) return null;
		int serial = lastMoveRequestSerial.intValue();
		lastMoveRequestSerial = null;
		return serial;
	}
	
	public ResizeRequest checkResizeRequest() {
		if(lastResizeRequest == null) return null;
		ResizeRequest req = lastResizeRequest;
		lastResizeRequest = null;
		return req;
	}
	
	public void resizeOutput(int width, int height) {
		outputResize(instance, width, height);
	}
	
	public void setOutputBounds(int width, int height) {
		outputSetBounds(instance, width, height);
	}
	
	public Size getOutputSize() {
		int[] size = outputSize(instance);
		return new Size(size[0], size[1]);
	}
	
	public Size getOutputBounds() {
		int[] size = outputBounds(instance);
		return new Size(size[0], size[1]);
	}
	
	public RawDesktopEntry loadDesktopEntry(File path) {
		return loadDesktopEntry(instance, path.getAbsolutePath());
	}
	
	public RawDesktopEntry[] loadSystemDesktopEntries() {
		return loadDesktopEntries(instance);
	}
	
	public boolean renderSVG(File file, int width, int height, long bufferPtr) {
		return renderSVG(file.getAbsolutePath(), width, height, bufferPtr);
	}
	
	public boolean execApp(String appId) {
		return execApp(instance, appId);
	}
	
	/**
	 * 检测应用是否可以启动（静态检查，不真正启动进程）。
	 * 返回状态串: ok / not-found / no-exec / empty / missing:&lt;cmd&gt; / flatpak-missing:&lt;id&gt;
	 */
	public String checkApp(String appId) {
		return checkApp(instance, appId);
	}
	
	public void setKeymapDefault() {
		setKeymapDefault(instance);
	}
	
	public String exportKeymap() {
		return exportKeymap(instance);
	}
	
	public boolean setKeymapFromStr(String keymap) {
		return setKeymapFromStr(instance, keymap);
	}
	
	public Integer checkDndRequest() {
		int[] serial = checkDndRequest(instance);
		if(serial == null) return null;
		return serial[0];
	}
	
	public void dndCancel() {
		dndCancel(instance);
	}
	
	public void dndDrop() {
		dndDrop(instance);
	}
	
	public void sendDndMotion(WLCSurface surface, double x, double y) {
		long handle = surface == null ? 0 : surface.getHandle();
		dndMotion(instance, handle, x, y);
	}
	
	public static record Size(int width, int height) {}
	
	public static record ResizeRequest(int serial, int edges) {}
	
	private static native long init(long glfwGetProcAddress, long eglDisplay, long waylandDisplay);
	private static native void update(long instance);
	private static native String socket(long instance);
	private static native String getSatelliteDisplay(long instance);
	private static native void sendFrame(long surfaceHandle);
	
	private static native void updateSurfaceData(long instance, WLCSurface surface);
	
	private static native long[] toplevels(long instance);
	private static native long toplevelSurface(long instance, long topLevelHandle);
	private static native String toplevelTitle(long topLevelHandle);
	private static native String toplevelAppID(long topLevelHandle);
	private static native int toplevelPid(long instance, long topLevelHandle);
	// Resize toplevel
	private static native void toplevelResize(long topLevelHandle, int width, int height, boolean interactive);
	// Resize toplevel override, keep maximized and fullscreen state, stop interactive resize
	private static native void toplevelResizeOvr(long topLevelHandle, int width, int height);
	
	// Collect all toplevels that have sent a minimize request and clear the list
	private static native long[] minimizeReq(long instance);
	// Collect all toplevels that have sent a maximize request and clear the list
	private static native long[] maximizeReq(long instance);
	// Collect all toplevels that have sent an unmaximize request and clear the list
	private static native long[] unmaximizeReq(long instance);
	// Collect all toplevels that have sent a fullscreen request and clear the list
	private static native long[] fullscreenReq(long instance);
	// Collect all toplevels that have sent an unfullscreen request and clear the list
	private static native long[] unfullscreenReq(long instance);
	
	// Collect up to one serial of a sent interactive move request
	private static native int[] moveRequest(long instance);
	// Collect up to one serial of a sent interactive resize request
	private static native int[] resizeRequest(long instance);
	
	// All toplevels that are currently in fullscreen
	private static native long[] fullscreened(long instance);
	
	private static native void toplevelMaximize(long instance, long topLevelHandle);
	private static native void toplevelFullscreen(long instance, long topLevelHandle);
	
	private static native long[] popups(long instance);
	private static native long popupSurface(long instance, long topLevelHandle);
	// Query the parent of a popup
	// Returned handle is a handle either to a toplevel or another popup
	private static native long popupParent(long instance, long topLevelHandle);
	// Query popup local offset coordinates
	// Returns two-element list containing x,y
	private static native int[] popupOffset(long popupHandle);
	
	// Query the xdg_surface window geometry of a toplevel or popup.
	// handle should be the handle to the root WLCSurface
	// Returns four-element array containing x,y,width,height which could be null
	private static native int[] surfaceXDGGeometry(long surfaceHandle);
	
	private static native long[] dmabufs(long instance);
	
	// Updates the surface tree given by the root surface
	// This changes the doubly linked list of the WLCSurfaces.
	// The returned surface is the last (most deeply nested) child
	private native WLCSurface updateSurfaceTree(long instance, WLCSurface root);
	
	// Check if point in surface input region
	private static native boolean checkInputRegion(long surfaceHandle, double x, double y);
	
	// Create pointer motion event
	private static native void pointerMotion(long instance, double x, double y);
	
	// Create pointer motion event
	private static native void pointerMotionFocus(long instance, long surfaceHandle, double x, double y);
	
	// Send relative pointer motion to surface with pointer focus
	private static native void pointerRelMotion(long instance, double dx, double dy);
	
	private static native boolean maybePointerLock(long instance, long surfaceHandle);
	
	private static native void pointerUnlock(long instance);
	
	// Remove pointer focus from all surfaces
	private static native void pointerLeave(long instance);
	
	// Create pointer button event. `button` has to be the linux button code, state is 1 for pressed, 0 for released
	private static native int pointerButton(long instance, int button, int state);
	
	// Create pointer axis event. `axis` is the scroll axis (0 for vertical, 1 for horizontal)
	private static native void pointerAxis(long instance, int axis, double value);
	
	// Get active cursor image
	private static native int cursorShape(long instance);
	
	// Set keyboard focus to a wayland surface. The handle may be 0 to unfocus any surfaces
	private static native void keyboardFocus(long instance, long surfaceHandle);
	
	private static native void keyboardActivate(long instance);
	private static native void keyboardDeactivate(long instance);
	
	// Keyboard input. scancode is the raw keycode. action: 0 is released, 1 is pressed, 2 is repeated.
	private static native void keyboardInput(long instance, int scancode, int action);
	
	// Update internal key state
	private static native void keyboardUpdate(long instance, int scancode, boolean pressed);
	
	// Set Rust-side [kb-debug] log file path (eprintln also written to this file)
	private static native void setKbLogFileNative(String path);
	
	// Set Rust-side [audio] log file path (eprintln also written to this file)
	private static native void setAudioLogFileNative(String path);
	
	// Set Rust-side [system_ime] log file path (eprintln also written to this file)
	private static native void setImeLogFileNative(String path);
	private static native void notifyHostFocusGainedNative(long instance);
	private static native void candidateNavNative(long instance, int action, int arg);
	
	// Take candidate-window snapshot (JSON; empty string = no update)
	private static native String takeLookupTableNative(long instance);
	
	// Query Rust-side audio capture pipeline status (JSON string)
	private static native String audioCaptureStatusNative(long instance);
	
	private static native int[] outputSize(long instance);
	private static native int[] outputBounds(long instance);
	
	// Update virtual output dimensions
	private static native void outputResize(long instance, int width, int height);
	
	// Update virtual output maximum window bounds
	private static native void outputSetBounds(long instance, int width, int height);
	
	private static native void freeSurface(long instance, long surfaceHandle);
	private static native void freeToplevel(long instance, long toplevelHandle);
	private static native void freePopup(long instance, long popupHandle);
	
	private static native RawDesktopEntry loadDesktopEntry(long instance, String path);
	private static native RawDesktopEntry[] loadDesktopEntries(long instance);
	
	private static native boolean renderSVG(String path, int width, int height, long bufferPtr);
	
	private static native boolean execApp(long instance, String appId);
	
	/** 把 jar 内嵌的 xwayland-satellite 二进制路径交给 native 层（在 native 库加载后调用） */
	private static native void setSatelliteBinary(String path);
	
	private static native String checkApp(long instance, String appId);
	
	private static native void setKeymapDefault(long instance);
	private static native String exportKeymap(long instance);
	private static native boolean setKeymapFromStr(long instance, String keymap);
	
	private static native int[] checkDndRequest(long instance);
	private static native boolean checkDndActive(long instance);
	private static native void dndCancel(long instance);
	private static native void dndDrop(long instance);
	private static native void dndMotion(long instance, long surfaceHandle, double x, double y);
	private static native long dndIcon(long instance);
	
	// 获取桌面窗口列表（通过 /proc）
	private static native String[] getDesktopWindows(long instance);
	
	// Portal ScreenCast 捕获 (XDG Desktop Portal, 跨桌面通用)
	private static native byte[] portalCaptureStart(long instance);
	private static native byte[] portalCaptureFrame(long instance);
	private static native void portalCaptureStop(long instance);
	
	/** 启动 Portal 捕获会话（会弹出确认对话框） */
	public byte[] portalCaptureStart() { return portalCaptureStart(this.instance); }
	/** 获取当前帧: [width(4), height(4), rgba...] */
	public byte[] portalCaptureFrame() { return portalCaptureFrame(this.instance); }
	/** 停止捕获 */
	public void portalCaptureStop() { portalCaptureStop(this.instance); }
	
	// 按进程音频捕获 (PipeWire: 共享窗口的声音)
	private static native void audioCaptureStart(long instance, int pid);
	private static native byte[] audioCapturePoll(long instance);
	private static native void audioCaptureStop(long instance);
	
	/** 启动音频捕获（捕获指定 PID 进程的 PipeWire 输出） */
	public void audioCaptureStart(int pid) { audioCaptureStart(this.instance, pid); }
	/** 获取累积音频: [sampleRate(4), channels(4), pcm...] */
	public byte[] audioCapturePoll() { return audioCapturePoll(this.instance); }
	/** 停止音频捕获 */
	public void audioCaptureStop() { audioCaptureStop(this.instance); }
	
}
