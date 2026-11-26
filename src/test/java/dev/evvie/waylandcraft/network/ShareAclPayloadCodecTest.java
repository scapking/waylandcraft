package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

import java.util.UUID;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.shared.ShareAcl;
import dev.evvie.waylandcraft.shared.WindowPermission;
import io.netty.buffer.Unpooled;
import net.minecraft.core.RegistryAccess;
import net.minecraft.network.RegistryFriendlyByteBuf;

/**
 * ACL-1/2 payload 编解码 round-trip 与入站防御测试（codec 直测，不启 MC 运行时）：
 * - perm_command：level/duration round-trip；旧三字段字节流宽容解析（兼容）。
 * - invite_request/response：round-trip；非法 decision 字节 → DENY；
 *   超大 duration 钳位；恶意长名截断上限。
 */
class ShareAclPayloadCodecTest {

	private static RegistryFriendlyByteBuf buf() {
		return new RegistryFriendlyByteBuf(Unpooled.buffer(), RegistryAccess.EMPTY);
	}

	@Test
	@DisplayName("perm_command round-trip：handle/action/target/level/duration 全字段")
	void permCommandRoundTrip() {
		var payload = new SharedWindowPermCommandPayload(0xfeedL, SharedWindowPermCommandPayload.ACTION_GRANT,
			"Alice", (byte) WindowPermission.INTERACT.getLevel(), 600);
		var out = buf();
		SharedWindowPermCommandPayload.CODEC.encode(out, payload);
		var decoded = assertDoesNotThrow(() -> SharedWindowPermCommandPayload.CODEC.decode(out));
		assertEquals(0xfeedL, decoded.windowHandle());
		assertEquals(SharedWindowPermCommandPayload.ACTION_GRANT, decoded.action());
		assertEquals("Alice", decoded.targetName());
		assertEquals((byte) WindowPermission.INTERACT.getLevel(), decoded.permissionLevel());
		assertEquals(600, decoded.durationSeconds());
	}

	@Test
	@DisplayName("perm_command：旧客户端三字段字节流 → level=VIEW、duration=0（永久），不 EOF")
	void permCommandOldClientBytesLenient() {
		var out = buf();
		// 模拟旧版本写端：handle + action + targetName，无 level/duration
		out.writeLong(42L);
		out.writeByte(SharedWindowPermCommandPayload.ACTION_GRANT);
		out.writeUtf("Bob");
		var decoded = assertDoesNotThrow(() -> SharedWindowPermCommandPayload.CODEC.decode(out));
		assertEquals(42L, decoded.windowHandle());
		assertEquals("Bob", decoded.targetName());
		assertEquals((byte) WindowPermission.VIEW.getLevel(), decoded.permissionLevel(),
			"缺字段按历史行为 = VIEW");
		assertEquals(0, decoded.durationSeconds(), "缺字段 = 永久");
	}

	@Test
	@DisplayName("perm_command：负/零 duration 读入钳到 0（永久），拒绝制造过期于过去的记录")
	void permCommandDurationFloored() {
		var out = buf();
		out.writeLong(1L);
		out.writeByte(SharedWindowPermCommandPayload.ACTION_GRANT);
		out.writeUtf("X");
		out.writeByte((byte) 1);
		out.writeVarLong(-5L);
		var decoded = SharedWindowPermCommandPayload.CODEC.decode(out);
		assertEquals(0, decoded.durationSeconds());
	}

	@Test
	@DisplayName("invite_request round-trip：handle + requesterUUID + name")
	void inviteRequestRoundTrip() {
		UUID requester = UUID.randomUUID();
		var payload = new ShareInviteRequestPayload(0xabcL, requester, "Notch");
		var out = buf();
		ShareInviteRequestPayload.CODEC.encode(out, payload);
		var decoded = assertDoesNotThrow(() -> ShareInviteRequestPayload.CODEC.decode(out));
		assertEquals(0xabcL, decoded.windowHandle());
		assertEquals(requester, decoded.requesterUUID());
		assertEquals("Notch", decoded.requesterName());
	}

	@Test
	@DisplayName("invite_response round-trip + 非法 decision 字节回退 DENY + 超大时长钳位")
	void inviteResponseGuards() {
		UUID requester = UUID.randomUUID();
		var payload = new ShareInviteResponsePayload(0x1L, requester, ShareAcl.INVITE_ALLOW_TIMED,
			(byte) WindowPermission.VIEW.getLevel(), 600);
		var out = buf();
		ShareInviteResponsePayload.CODEC.encode(out, payload);
		var decoded = ShareInviteResponsePayload.CODEC.decode(out);
		assertEquals(ShareAcl.INVITE_ALLOW_TIMED, decoded.decision());
		assertEquals(600, decoded.durationSeconds());
		assertEquals(requester, decoded.requesterUUID());

		// decision=99（越界）→ 必须降级为 DENY（保守），绝不能当成"允许"
		var hostile = buf();
		hostile.writeLong(2L);
		hostile.writeUUID(UUID.randomUUID());
		hostile.writeByte((byte) 99);
		hostile.writeByte((byte) 127);
		hostile.writeVarLong(Long.MAX_VALUE);
		var h = assertDoesNotThrow(() -> ShareInviteResponsePayload.CODEC.decode(hostile));
		assertEquals(ShareAcl.INVITE_DENY, h.decision());
		assertEquals(ShareInviteResponsePayload.MAX_DURATION_SECONDS, h.durationSeconds(),
			"超大时长钳到 30d 上限");
	}

	@Test
	@DisplayName("buildInviteResponse 接缝：过期/句柄不匹配的邀请应答构造失败（防陈年按钮刷授权）")
	void inviteResponseSeam() {
		SharedWindowClientHandler.resetPendingInvitesForTest();
		UUID requester = UUID.randomUUID();
		long now = System.currentTimeMillis();
		SharedWindowClientHandler.putPendingInviteForTest("Alice", 0x9L, requester, now - 30_000);

		var ok = SharedWindowClientHandler.buildInviteResponse(0x9L, "alice",
			ShareAcl.INVITE_ALLOW_ALWAYS, (byte) 1, 0, now);
		assertEquals(requester, ok.requesterUUID(), "名字大小写不敏感（表键小写）");

		assertNull(SharedWindowClientHandler.buildInviteResponse(0x1234L, "Alice",
			ShareAcl.INVITE_DENY, (byte) 1, 0, now), "句柄不匹配 → 拒绝");
		assertNull(SharedWindowClientHandler.buildInviteResponse(0x9L, "Bob",
			ShareAcl.INVITE_DENY, (byte) 1, 0, now), "未知邀请 → 拒绝");
		assertNull(SharedWindowClientHandler.buildInviteResponse(0x9L, "Alice",
			ShareAcl.INVITE_DENY, (byte) 1, 0, now + 11L * 60_000), "超过 10 分钟 TTL → 拒绝");
		SharedWindowClientHandler.resetPendingInvitesForTest();
	}
}
