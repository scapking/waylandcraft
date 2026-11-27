package dev.evvie.waylandcraft.render;

import java.util.Locale;

import org.jetbrains.annotations.Nullable;

/**
 * v1.4.7 光影兼容策略（用户反馈"光影模式会干扰窗口的画质可见度"）。
 *
 * 机制：Iris 着色器包的 composite 后处理（tonemap 压暗、去饱和、胶片对比、
 * 轻微提黑）作用在世界 framebuffer 上，窗口纹理作为世界空间内容一并被调色，
 * 应用 UI（本就按 sRGB 精确设计）因此发灰发暗、可读性下降。
 *
 * 方案：**预补偿**——离屏合成（本 mod 唯一可控的颜色出口）输出前按轻量逆
 * 曲线调整（见 window.fsh 的 compat uniform），与 shader 后处理的衰减方向
 * 相抵。参数是数据表：按光影包名（zip 文件名去版本）匹配 profile；新光影包
 * 适配 = 在 {@link #profileFor} 加一行，无需碰渲染路径。
 *
 * 语义：
 * - mode=off：恒等 (1,1,1,1)，行为与旧版完全一致。
 * - mode=auto（默认）：检测到光影包启用才补偿；未匹配到的包用保守通用档。
 * 通用档刻意温和（肉眼"更清晰"但不过曝），已知包按实测调参。
 */
public final class ShaderCompatPolicy {

	/** 恒等：不干预。 */
	public static final float[] IDENTITY = {1.0f, 1.0f, 1.0f, 1.0f};

	/**
	 * 光影包名关键字 → (brightness, saturation, contrast, gamma)。
	 * 顺序即优先级；名字用 zip 文件名小写去版本号后的稳定前缀。
	 */
	private static final String[][] PACK_TABLE = {
		// Complementary Reimagined/Regenerated：轻度 filmic，UI 中灰压暗最明显
		{"complementary", "1.07", "1.09", "1.04", "1.09"},
		// MakeUp UltraFast：轻调色，最小补偿
		{"makeup", "1.04", "1.06", "1.02", "1.05"},
		// 常见大户（预防性收录，参数=通用偏保守）
		{"bsl", "1.06", "1.07", "1.03", "1.07"},
		{"seus", "1.08", "1.08", "1.04", "1.10"},
		{"photon", "1.05", "1.07", "1.02", "1.06"},
		{"nostep", "1.05", "1.07", "1.02", "1.06"},
		{"electric", "1.07", "1.08", "1.03", "1.08"},
	};

	/** 未匹配光影包的通用保守档。 */
	private static final float[] GENERIC = {1.05f, 1.07f, 1.02f, 1.06f};

	private static volatile float[] current = IDENTITY;

	private ShaderCompatPolicy() {}

	/** 当前生效补偿系数（渲染热路径读取，无锁）。 */
	public static float[] current() {
		return current;
	}

	/**
	 * 纯函数：mode + 光影状态 → 系数。可单测、零副作用。
	 *
	 * @param mode         auto|off（未知值按 auto 处理——保守取"优化开"）
	 * @param shaderInUse  光影包是否启用
	 * @param packName     光影包名（可为 null：按 GENERIC）
	 */
	public static float[] decide(String mode, boolean shaderInUse, @Nullable String packName) {
		if (mode == null) mode = "auto";
		if (mode.equalsIgnoreCase("off")) return IDENTITY;
		if (!shaderInUse) return IDENTITY;
		return profileFor(packName);
	}

	/** 纯函数：光影包名 → profile（子串匹配，大小写不敏感；未匹配=GENERIC）。 */
	public static float[] profileFor(@Nullable String packName) {
		if (packName == null || packName.isEmpty()) return GENERIC.clone();
		String n = packName.toLowerCase(Locale.ROOT);
		for (String[] row : PACK_TABLE) {
			if (n.contains(row[0])) {
				return new float[]{
					Float.parseFloat(row[1]), Float.parseFloat(row[2]),
					Float.parseFloat(row[3]), Float.parseFloat(row[4]),
				};
			}
		}
		return GENERIC.clone();
	}

	/** 刷新生效值（tick 级调用；探测本身在 IrisCompat 里节流）。 */
	public static void refresh(String mode) {
		current = decide(mode, IrisCompat.isShaderPackInUse(), IrisCompat.shaderPackName());
	}
}
