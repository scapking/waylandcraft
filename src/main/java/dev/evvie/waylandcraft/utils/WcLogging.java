package dev.evvie.waylandcraft.utils;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.bridge.WaylandCraftBridge;

/**
 * 日志分级的 Java 侧生效档（单一事实源，与 Rust 侧 VERBOSE_LOG 对应）。
 *
 * <p>语义（完整分级表见 docs/LOGGING.md）：
 * <ul>
 *   <li><b>verbose=true（开发档）</b>：全量日志——逐键/逐事件/周期采样行
 *       照常写，跨层链路可对时诊断。</li>
 *   <li><b>verbose=false（发布档）</b>：只保留启动摘要、WARN/ERROR、
 *       error ring（status note_error）、IME 里程碑（spawn/bind/主路径
 *       激活/失败回退）、共享 start/stop 结果；细节行在写入点前短路，
 *       零格式化零 IO。</li>
 * </ul>
 *
 * <p>档位来源与优先级（高 → 低）：
 * <ol>
 *   <li>系统属性 {@code -Dwc.verbose=true|false}（存在即覆盖 settings，
 *       启动脚本/launcher 参数可用，不必改 settings.json）；</li>
 *   <li>settings 键 {@code verboseLogging}（默认 = FabricLoader
 *       isDevelopmentEnvironment，{@code /wl settings set verboseLogging
 *       true|false} 可改，responder 变更即下发）；</li>
 *   <li>两者都不可判定（无 Fabric 运行时的纯 JVM 环境，如单测）→ true，
 *       与 Rust 全局 AtomicBool 默认一致，测试环境行为不漂移。</li>
 * </ol>
 *
 * <p>变更时经 {@link WaylandCraftBridge#setVerboseLogging(boolean)} 同步
 * 下发 Rust（nativeAvailable 防护，服务端 jar 无 native 不炸——setModVersion
 * 同纪律）。本类静态初始化不触碰 Minecraft/FabricLoader 之外的运行时状态，
 * 服务端与单测环境可安全加载。
 */
public final class WcLogging {

	/** 覆盖用系统属性名：-Dwc.verbose=true|false。 */
	public static final String OVERRIDE_PROPERTY = "wc.verbose";

	/** 系统属性覆盖档（null=未设置，走 settings）。类加载时读一次。 */
	private static final Boolean OVERRIDE = readOverride();

	private static volatile boolean verbose = effective(OVERRIDE, defaultVerbose());

	private WcLogging() {}

	public static boolean verbose() {
		return verbose;
	}

	/** 当前是否被系统属性接管（诊断行/CLI 回执用）。 */
	public static boolean overrideActive() {
		return OVERRIDE != null;
	}

	/**
	 * settings 变更（含启动 responder 首次回放）入口：按优先级合成生效档，
	 * 更新本类并下发 Rust。系统属性在场时 settings 值仅持久化、不生效。
	 */
	public static void applySetting(boolean settingValue) {
		verbose = effective(OVERRIDE, settingValue);
		// sync5 纪律：native 缺失（server jar / 旧 .so）必须静默不致命。
		try {
			WaylandCraftBridge.setVerboseLogging(verbose);
		} catch (Throwable t) {
			WaylandCraftCommon.LOGGER.warn("[logging] setVerboseLogging unavailable: {}", t.toString());
		}
	}

	/** 生效档合成（纯函数，单测直打）：覆盖 > settings。 */
	public static boolean effective(Boolean override, boolean settingValue) {
		return override != null ? override : settingValue;
	}

	/**
	 * settings 无值可参考时的兜底（无 Fabric 运行时）：true——与 Rust
	 * VERBOSE_LOG 默认一致，保证 native 单测/无 Java 下发环境全开。
	 */
	public static boolean defaultVerbose() {
		try {
			return net.fabricmc.loader.api.FabricLoader.getInstance().isDevelopmentEnvironment();
		} catch (Throwable t) {
			// 纯 JVM 单测环境：FabricLoader 未初始化（getInstance() 为 null
			// 或抛错），保守取 true 对齐 Rust 默认，不赌 false。
			return true;
		}
	}

	/** 解析 -Dwc.verbose（纯函数，单测直打）。null=未设置/空/非法值。 */
	public static Boolean parseOverride(String raw) {
		if (raw == null) return null;
		String v = raw.trim();
		if (v.isEmpty()) return null;
		if (v.equalsIgnoreCase("true")) return Boolean.TRUE;
		if (v.equalsIgnoreCase("false")) return Boolean.FALSE;
		return null; // 非法值：视为未设置（宁可按 settings 走，不静默翻转用户预期）
	}

	private static Boolean readOverride() {
		try {
			return parseOverride(System.getProperty(OVERRIDE_PROPERTY));
		} catch (Throwable t) {
			return null;
		}
	}
}
