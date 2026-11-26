package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;

import java.util.List;
import java.util.UUID;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.shared.WindowPermission;
import io.netty.buffer.Unpooled;
import net.minecraft.core.RegistryAccess;
import net.minecraft.network.RegistryFriendlyByteBuf;

/**
 * P1-6 修复证明（codec 直测，不启 MC 运行时）：
 * 入站权限等级解码不再 `WindowPermission.values()[varInt]` 越界 AIOOBE——
 * 查表 fromLevel：合法 0..3 精确映射，越界/负数回退 NONE（拒绝，绝不提权）。
 */
class ClientPermDecodeTest {

	private static RegistryFriendlyByteBuf buf() {
		return new RegistryFriendlyByteBuf(Unpooled.buffer(), RegistryAccess.EMPTY);
	}

	@Test
	@DisplayName("fromLevel：0..3 精确查表，越界/负数一律 NONE（不得钳位提升为 CONTROL）")
	void fromLevelTableAndFallback() {
		assertEquals(WindowPermission.NONE, WindowPermission.fromLevel(0));
		assertEquals(WindowPermission.VIEW, WindowPermission.fromLevel(1));
		assertEquals(WindowPermission.INTERACT, WindowPermission.fromLevel(2));
		assertEquals(WindowPermission.CONTROL, WindowPermission.fromLevel(3));
		assertEquals(WindowPermission.NONE, WindowPermission.fromLevel(-1));
		assertEquals(WindowPermission.NONE, WindowPermission.fromLevel(Integer.MIN_VALUE));
		assertEquals(WindowPermission.NONE, WindowPermission.fromLevel(4));
		assertEquals(WindowPermission.NONE, WindowPermission.fromLevel(Integer.MAX_VALUE),
			"恶意大数绝不能落到 CONTROL（对比 fromLevelClamped 的钳位语义）");
	}

	@Test
	@DisplayName("list codec：合法条目 round-trip 不受守卫影响")
	void listCodecRoundTrip() {
		UUID owner = UUID.randomUUID();
		var payload = new SharedWindowListPayload(List.of(
			new SharedWindowListPayload.WindowInfo(0x1234L, owner, "Firefox", WindowPermission.INTERACT)));
		var out = buf();
		SharedWindowListPayload.CODEC.encode(out, payload);
		var decoded = assertDoesNotThrow(() -> SharedWindowListPayload.CODEC.decode(out));
		assertEquals(1, decoded.windows().size());
		assertEquals(WindowPermission.INTERACT, decoded.windows().get(0).permission());
	}

	@Test
	@DisplayName("P1-6：list codec 对越界权限等级回退 NONE 且不抛 AIOOBE")
	void listCodecRejectsOutOfRangeLevel() {
		var out = buf();
		out.writeVarInt(2);
		out.writeLong(1L);
		out.writeUUID(UUID.randomUUID());
		out.writeUtf("A");
		out.writeVarInt(99);        // 越界：旧实现 values()[99] → ArrayIndexOutOfBounds
		out.writeLong(2L);
		out.writeUUID(UUID.randomUUID());
		out.writeUtf("B");
		out.writeVarInt(-7);        // 负数：旧实现同样越界

		var decoded = assertDoesNotThrow(() -> SharedWindowListPayload.CODEC.decode(out));
		assertEquals(2, decoded.windows().size());
		assertEquals(WindowPermission.NONE, decoded.windows().get(0).permission());
		assertEquals(WindowPermission.NONE, decoded.windows().get(1).permission());
	}

	@Test
	@DisplayName("P1-6：permission payload codec 对越界等级同样回退 NONE（不崩解码线程）")
	void permissionCodecRejectsOutOfRangeLevel() {
		var out = buf();
		out.writeLong(7L);
		out.writeUUID(UUID.randomUUID());
		out.writeVarInt(42);

		var decoded = assertDoesNotThrow(() -> SharedWindowPermissionPayload.CODEC.decode(out));
		assertEquals(WindowPermission.NONE, decoded.permission());
	}
}
