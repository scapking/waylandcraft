package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicInteger;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P1-1 修复证明：canSend 过滤 + 本轮结果缓存（纯泛型层，Mixin-free）。
 * ServerPlayNetworking.canSend 的 MC 依赖由调用方 probe lambda 注入；
 * 这里验证语义：无能力者被剔除、每个玩家每轮只 probe 一次。
 */
class RelaySinksSelectCapableTest {

	private record FakePlayer(UUID id, boolean capable) {}

	@Test
	@DisplayName("过滤掉 canSend=false 的玩家，保留 true")
	void dropsIncapableViewers() {
		FakePlayer a = new FakePlayer(UUID.randomUUID(), true);
		FakePlayer b = new FakePlayer(UUID.randomUUID(), false);
		FakePlayer c = new FakePlayer(UUID.randomUUID(), true);

		Map<UUID, Boolean> cache = new HashMap<>();
		List<FakePlayer> out = RelaySinks.selectCapable(
			List.of(a, b, c), FakePlayer::id, cache, FakePlayer::capable);

		assertEquals(List.of(a, c), out);
	}

	@Test
	@DisplayName("结果缓存本轮复用：同一玩家多帧转发只 probe 一次（5 帧 × 3 玩家 → 3 次）")
	void probesEachPlayerOncePerRound() {
		FakePlayer p1 = new FakePlayer(UUID.randomUUID(), true);
		FakePlayer p2 = new FakePlayer(UUID.randomUUID(), false);
		AtomicInteger probeCalls = new AtomicInteger();

		Map<UUID, Boolean> cache = new HashMap<>();
		List<FakePlayer> snapshot = List.of(p1, p2);

		for (int frame = 0; frame < 5; frame++) {
			List<FakePlayer> out = RelaySinks.selectCapable(
				snapshot, FakePlayer::id, cache,
				fp -> {
					probeCalls.incrementAndGet();
					return fp.capable();
				});
			assertEquals(List.of(p1), out, "每帧结果一致");
		}
		assertEquals(2, probeCalls.get(), "5 轮收集只对 2 个玩家各 probe 一次");
	}

	@Test
	@DisplayName("全不可信（空快照）返回空列表不抛异常")
	void emptySnapshotYieldsEmptyList() {
		Map<UUID, Boolean> cache = new HashMap<>();
		List<FakePlayer> out = RelaySinks.selectCapable(
			List.of(), FakePlayer::id, cache, fp -> true);
		assertEquals(0, out.size());
	}
}
