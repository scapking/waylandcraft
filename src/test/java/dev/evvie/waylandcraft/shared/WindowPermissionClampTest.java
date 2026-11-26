package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P0-1 修复证明：网络传入的 permissionLevel（byte，可为负/>3）
 * 必须钳位到合法枚举范围，杜绝 values()[越界] ArrayIndexOutOfBounds 崩服。
 */
class WindowPermissionClampTest {

	@Test
	@DisplayName("fromLevelClamped: 负值钳位到 NONE")
	void negativeLevelsClampToNone() {
		assertEquals(WindowPermission.NONE, WindowPermission.fromLevelClamped(-1));
		assertEquals(WindowPermission.NONE, WindowPermission.fromLevelClamped(-128));
		assertEquals(WindowPermission.NONE, WindowPermission.fromLevelClamped(Integer.MIN_VALUE));
	}

	@Test
	@DisplayName("fromLevelClamped: 超出上界钳位到 CONTROL（含 byte 能到的 3/127）")
	void highLevelsClampToControl() {
		assertEquals(WindowPermission.CONTROL, WindowPermission.fromLevelClamped(3));
		assertEquals(WindowPermission.CONTROL, WindowPermission.fromLevelClamped(4));
		assertEquals(WindowPermission.CONTROL, WindowPermission.fromLevelClamped(127));
		assertEquals(WindowPermission.CONTROL, WindowPermission.fromLevelClamped(Integer.MAX_VALUE));
	}

	@Test
	@DisplayName("fromLevelClamped: 合法 ordinal 原样映射")
	void validLevelsMapIdentity() {
		assertEquals(WindowPermission.NONE, WindowPermission.fromLevelClamped(0));
		assertEquals(WindowPermission.VIEW, WindowPermission.fromLevelClamped(1));
		assertEquals(WindowPermission.INTERACT, WindowPermission.fromLevelClamped(2));
		assertEquals(WindowPermission.CONTROL, WindowPermission.fromLevelClamped(3));
	}
}
