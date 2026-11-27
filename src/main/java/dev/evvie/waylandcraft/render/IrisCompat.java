package dev.evvie.waylandcraft.render;

import net.fabricmc.loader.api.FabricLoader;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * Iris（光影）兼容检测。
 * 
 * Iris 通过 mixin 全局拦截 {@code GlDevice.getOrCompilePipeline}（{@code iris$redirectIrisProgram}），
 * 把所有自定义 RenderPipeline / RenderType 交给它的 shader 系统重写；WaylandCraft 的自定义
 * shader 管线（rendertype_window / window_blit 等）不被 Iris 认识 → 渲染时抛异常 → 窗口画面出不来。
 * 
 * 检测到 Iris 加载时，世界空间窗口渲染改用原版 entity 管线（Iris 认识并能正确处理），
 * 2D GUI 渲染改用原版 GUI_TEXTURED 管线，从而在光影开启时也能正常显示窗口。
 */
public final class IrisCompat {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-iris-compat");
	
	private static final boolean IRIS_LOADED = detectIris();
	
	private IrisCompat() {}
	
	private static boolean detectIris() {
		try {
			boolean loaded = FabricLoader.getInstance().isModLoaded("iris");
			if(loaded) {
				LOGGER.info("Iris (shaders) detected - using vanilla pipeline fallback for window rendering");
			}
			return loaded;
		} catch(Throwable t) {
			// FabricLoader 不可用时（异常环境）保守起见当作未加载
			return false;
		}
	}
	
	/**
	 * 当前是否加载了 Iris 光影 mod
	 */
	public static boolean isIrisLoaded() {
		return IRIS_LOADED;
	}
	
	// ── v1.4.7 运行时探测（ShaderCompatPolicy 用）──────────────────────
	// Iris 官方 API（net.irisshaders.iris.api.v0.IrisApi）：
	//   isShaderPackInUse() / getCurrentShaderPackName()。
	// 用反射避免对 iris 的编译期依赖（viewer jar 三端通用），并 2s 节流缓存
	// （tick 热路径零反射开销）。API 缺失/异常一律保守回退（未启用/无名字），
	// 最坏结果=不补偿或走 GENERIC 档，不会崩。
	
	private static volatile long probeAt = 0;
	private static volatile boolean packInUse = false;
	private static volatile String packName = null;
	
	private static final long PROBE_INTERVAL_MS = 2000;
	
	public static boolean isShaderPackInUse() {
		probeIfNeeded();
		return packInUse;
	}
	
	/** 当前光影包名（含 zip 后缀的显示名），未知/未启用返回 null。 */
	public static @org.jetbrains.annotations.Nullable String shaderPackName() {
		probeIfNeeded();
		return packName;
	}
	
	private static void probeIfNeeded() {
		if(!IRIS_LOADED) return;
		long now = System.currentTimeMillis();
		if(now - probeAt < PROBE_INTERVAL_MS) return;
		probeAt = now;
		try {
			Class<?> apiCls = Class.forName("net.irisshaders.iris.api.v0.IrisApi");
			Object api = apiCls.getMethod("getInstance").invoke(null);
			Object inUse = apiCls.getMethod("isShaderPackInUse").invoke(api);
			packInUse = inUse instanceof Boolean b && b;
			String name = null;
			try {
				Object n = apiCls.getMethod("getCurrentShaderPackName").invoke(api);
				name = n instanceof String s && !s.isEmpty() ? s : null;
			} catch(NoSuchMethodException ignored) {
				// 老版 Iris 无此方法：只有开关，无包名 → GENERIC 档
			}
			packName = name;
		} catch(Throwable t) {
			packInUse = false;
			packName = null;
		}
	}
	
}
