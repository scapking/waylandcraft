package dev.evvie.waylandcraft.settings;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

import com.google.gson.Gson;

import dev.evvie.waylandcraft.utils.WcLogging;

/**
 * 日志分级开关 verboseLogging 的设置面单测（需求④）：
 * - settings 往返：Gson 序列化/反序列化（与 WaylandCraftSettingsManager
 *   的 settings.json 存储路径同格式同 API）值不丢；
 * - 缺省兼容：旧 settings.json（无该键）加载后保持"dev 环境默认"语义；
 * - WcLogging 纯函数：-Dwc.verbose 解析与生效档合成（覆盖 > settings）。
 *
 * 注意：不触碰 WcLogging.applySetting / WaylandCraftBridge（native 加载面），
 * 与既有测试一样保持纯 JVM。
 */
class VerboseLoggingSettingsTest {

	private final Gson gson = new Gson();

	/** 无 Fabric 运行时的纯 JVM 单测环境：FabricLoader.getInstance() 抛
	 * "Accessed FabricLoader too early!" → defaultVerbose() 兜底 true
	 * （对齐 Rust VERBOSE_LOG 默认，测试/开发环境行为不漂移）。 */
	private static final boolean TEST_ENV_DEFAULT = WcLogging.defaultVerbose();

	@Test
	void pureJvmFallbackIsTrue() {
		assertTrue(TEST_ENV_DEFAULT,
				"单测环境（FabricLoader 未初始化）兜底档必须为 true，与 Rust 默认一致");
	}

	@Test
	void freshSettingsUsesDefault() {
		WaylandCraftSettings s = new WaylandCraftSettings();
		assertEquals(TEST_ENV_DEFAULT, s.getVerboseLogging());
	}

	@Test
	void verboseLoggingRoundTripsThroughJson() {
		WaylandCraftSettings s = new WaylandCraftSettings();

		s.setBooleanSetting(WaylandCraftSettings.VERBOSE_LOGGING, false);
		String json = gson.toJson(s, WaylandCraftSettings.class);
		assertTrue(json.contains("\"verboseLogging\":false"),
				"新键必须按字段名写入 settings.json（Gson 反射序列化）: " + json);
		WaylandCraftSettings back = gson.fromJson(json, WaylandCraftSettings.class);
		assertFalse(back.getVerboseLogging(), "写入 false → 读回 false");

		back.setBooleanSetting(WaylandCraftSettings.VERBOSE_LOGGING, true);
		WaylandCraftSettings back2 = gson.fromJson(gson.toJson(back, WaylandCraftSettings.class),
				WaylandCraftSettings.class);
		assertTrue(back2.getVerboseLogging(), "再切 true → 读回 true（双向往返）");
	}

	@Test
	void oldSettingsWithoutKeyKeepsDevDefault() {
		// 升级兼容：老版本 settings.json 没有 verboseLogging 键——Gson
		// 反序列化不会覆盖字段初始化器，缺省语义（dev 环境判定）必须保持。
		WaylandCraftSettings back = gson.fromJson(
				"{\"pixelsPerBlock\":500,\"imeSource\":\"auto\"}", WaylandCraftSettings.class);
		assertEquals(TEST_ENV_DEFAULT, back.getVerboseLogging());
		// 其余既有键不受影响（防序列化面回归）。
		assertEquals(500, (int) back.getIntSetting(WaylandCraftSettings.PIXELS_PER_BLOCK));
		assertEquals("auto", back.getStringSetting(WaylandCraftSettings.IME_SOURCE));
	}

	@Test
	void getterMatchesFieldDefault() {
		WaylandCraftSettings s = new WaylandCraftSettings();
		assertNotNull(s);
		assertEquals(TEST_ENV_DEFAULT, s.getVerboseLogging(),
				"getter 与字段初始化器同源（避免双默认漂移）");
	}

	@Test
	void overrideParsingAcceptsOnlyBooleans() {
		// -Dwc.verbose 与 CLI 共用同一解析规则（大小写/空白宽容，脏值拒收）。
		assertNull(WcLogging.parseOverride(null));
		assertNull(WcLogging.parseOverride(""));
		assertNull(WcLogging.parseOverride("   "));
		assertNull(WcLogging.parseOverride("yes"));
		assertNull(WcLogging.parseOverride("1"));
		assertNull(WcLogging.parseOverride("falsey"));
		assertEquals(Boolean.TRUE, WcLogging.parseOverride("true"));
		assertEquals(Boolean.TRUE, WcLogging.parseOverride(" TRUE "));
		assertEquals(Boolean.FALSE, WcLogging.parseOverride("false"));
		assertEquals(Boolean.FALSE, WcLogging.parseOverride("False"));
	}

	@Test
	void effectivePrefersOverrideOverSetting() {
		// 无覆盖：settings 直通。
		assertTrue(WcLogging.effective(null, true));
		assertFalse(WcLogging.effective(null, false));
		// 覆盖在场：settings 被压制（含 dev 环境强关发布档调试）。
		assertTrue(WcLogging.effective(Boolean.TRUE, false));
		assertFalse(WcLogging.effective(Boolean.FALSE, true));
	}
}
