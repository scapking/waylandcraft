package dev.evvie.waylandcraft.render;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotEquals;

import org.junit.jupiter.api.Test;

/**
 * v1.4.7 光影兼容策略纯函数测试（decide/profileFor 零副作用、零 MC 依赖）。
 * 语义锚点：
 * - off / 无光影 → 恒等（窗口纹理绝不被动，兼容旧行为与截图一致性）；
 * - auto + 已知包 → 该包 profile（数据表可维护）；
 * - auto + 未知包 → GENERIC 保守档（有补偿但不过火）；
 * - 参数序：brightness, saturation, contrast, gamma（GLSL compat 分量一致）。
 */
class ShaderCompatPolicyTest {

	@Test
	void offIsAlwaysIdentity() {
		assertArrayEquals(ShaderCompatPolicy.IDENTITY,
				ShaderCompatPolicy.decide("off", true, "ComplementaryReimagined_r5.7.1.zip"), 0.0001f);
		assertArrayEquals(ShaderCompatPolicy.IDENTITY,
				ShaderCompatPolicy.decide("off", false, null), 0.0001f);
	}

	@Test
	void autoWithoutShaderIsActiveIdentity() {
		assertArrayEquals(ShaderCompatPolicy.IDENTITY,
				ShaderCompatPolicy.decide("auto", false, "ComplementaryReimagined_r5.7.1.zip"), 0.0001f);
	}

	@Test
	void autoWithKnownPackUsesItsProfile() {
		float[] comp = ShaderCompatPolicy.decide("auto", true, "ComplementaryReimagined_r5.7.1.zip");
		float[] makeup = ShaderCompatPolicy.decide("auto", true, "MakeUp-UltraFast-9.5a.zip");
		assertNotEquals(comp[0], makeup[0], "不同光影包应有不同亮度档");
		// 补偿方向：全部 >1（对抗压暗/去饱和），且温和（<1.2，防过曝）
		for (float[] p : new float[][]{comp, makeup}) {
			for (float v : p) {
				assertEquals(true, v >= 1.0f && v <= 1.2f, "参数必须温和且只向上补偿: " + v);
			}
		}
	}

	@Test
	void autoWithUnknownPackFallsBackToGeneric() {
		float[] unknown = ShaderCompatPolicy.decide("auto", true, "BrandNewShader_v1.zip");
		float[] generic = ShaderCompatPolicy.profileFor(null);
		assertArrayEquals(generic, unknown, 0.0001f);
	}

	@Test
	void nullModeTreatedAsAuto() {
		assertArrayEquals(ShaderCompatPolicy.decide("auto", true, "bsL_v11.zip"),
				ShaderCompatPolicy.decide(null, true, "bsL_v11.zip"), 0.0001f);
	}

	@Test
	void packMatchingIsCaseInsensitiveSubstring() {
		float[] a = ShaderCompatPolicy.profileFor("COMPLEMENTARY-REIMAGINED_r5.zip");
		float[] b = ShaderCompatPolicy.profileFor("ComplementaryReimagined_r5.7.1.zip");
		assertArrayEquals(a, b, 0.0001f);
	}

	@Test
	void returnedProfilesAreDefensiveCopies() {
		float[] p1 = ShaderCompatPolicy.profileFor("nonexistent.zip");
		p1[0] = 999f;
		float[] p2 = ShaderCompatPolicy.profileFor("nonexistent.zip");
		assertNotEquals(999f, p2[0], "profile 数组不得共享可变状态（渲染每帧读）");
	}
}
