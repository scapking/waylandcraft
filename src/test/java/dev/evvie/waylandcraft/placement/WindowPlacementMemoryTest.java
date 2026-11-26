package dev.evvie.waylandcraft.placement;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.placement.WindowPlacementMemory.Placement;

/**
 * 缺陷 3：本地直显摆位按 (serverHost, 窗口别名) 记忆——序列化/键派生/编号纯逻辑。
 * 存储走 WaylandCraftSettings 既有 Gson 持久化模式（Map<String,String>），
 * 本类锁定编码可逆性与脏数据容忍，单人存档与多人服共用同一键规则。
 */
class WindowPlacementMemoryTest {

	private static final Placement P = new Placement(12.5, 70.25, -8.0, 90.0, 2.0);

	@Test
	@DisplayName("encode/decode 精确往返（3 位定点，Locale 无关）")
	void roundTrip() {
		String enc = WindowPlacementMemory.encode(P);
		Placement dec = WindowPlacementMemory.decode(enc);
		assertNotNull(dec);
		assertEquals(12.5, dec.x(), 1e-3);
		assertEquals(70.25, dec.y(), 1e-3);
		assertEquals(-8.0, dec.z(), 1e-3);
		assertEquals(90.0, dec.yawDeg(), 1e-3);
		assertEquals(2.0, dec.distance(), 1e-3);
	}

	@Test
	@DisplayName("脏数据全部解码为 null：不得让坏记录抛异常或产生 NaN 摆位")
	void decodeRejectsGarbage() {
		assertNull(WindowPlacementMemory.decode(null));
		assertNull(WindowPlacementMemory.decode(""));
		assertNull(WindowPlacementMemory.decode("1,2,3"));
		assertNull(WindowPlacementMemory.decode("1,2,3,4,x"));
		assertNull(WindowPlacementMemory.decode("1,2,3,4,NaN"));
		assertNull(WindowPlacementMemory.decode("1,2,3,4,Infinity"));
		assertNull(WindowPlacementMemory.decode("a,b,c,d,e"));
	}

	@Test
	@DisplayName("serverKey：多人=IP 小写；单人=singleplayer:世界名；两者皆缺=\"\"（不持久化）")
	void serverKeyDerivation() {
		assertEquals("3c3u.org", WindowPlacementMemory.serverKey("3C3U.org", null));
		assertEquals("singleplayer:新世界", WindowPlacementMemory.serverKey(null, "新世界"));
		assertEquals("3c3u.org", WindowPlacementMemory.serverKey("3c3u.org", "忽略我"), "IP 优先于世界名");
		assertEquals("", WindowPlacementMemory.serverKey(null, null));
		assertEquals("", WindowPlacementMemory.serverKey("  ", " "));
	}

	@Test
	@DisplayName("storageKey 分隔符唯一且注入安全：段内 '|' 被替换，键不串位")
	void storageKeySanitized() {
		String k = WindowPlacementMemory.storageKey("a|b.com", "qq|x");
		assertEquals("a_b.com|qq_x", k);
		assertFalse(k.indexOf('|') != k.lastIndexOf('|'), "恰一个真实分隔符");
	}

	@Test
	@DisplayName("remember/lookup 往返；未知 server/别名查无返回 null")
	void storeLookup() {
		Map<String, String> store = new HashMap<String, String>();
		WindowPlacementMemory.remember(store, "3c3u.org", "qq", P);
		Placement got = WindowPlacementMemory.lookup(store, "3c3u.org", "qq");
		assertNotNull(got);
		assertEquals(90.0, got.yawDeg(), 1e-3);

		assertNull(WindowPlacementMemory.lookup(store, "other.server", "qq"), "跨服不得串位");
		assertNull(WindowPlacementMemory.lookup(store, "3c3u.org", "firefox"));

		// 覆盖写：同键二次 remember 更新而非追加
		WindowPlacementMemory.remember(store, "3c3u.org", "qq", new Placement(1, 2, 3, 4, 5));
		assertEquals(1.0, WindowPlacementMemory.lookup(store, "3c3u.org", "qq").x(), 1e-3);
		assertEquals(1, store.size());

		// 空参数静默忽略（不抛、不写垃圾键）
		WindowPlacementMemory.remember(store, "", "qq", P);
		WindowPlacementMemory.remember(store, "3c3u.org", "", P);
		WindowPlacementMemory.remember(store, "3c3u.org", "xterm", null);
		assertEquals(1, store.size());
	}

	@Test
	@DisplayName("同名别名编号：首个不变、后续 #N；保存与恢复共用同一枚举顺序即稳定")
	void indexedAliasesDuplicateHandling() {
		List<String> in = List.of("qq", "xterm", "qq", "qq", "firefox");
		List<String> out = WindowPlacementMemory.indexedAliases(in);
		assertEquals(List.of("qq", "xterm", "qq#2", "qq#3", "firefox"), out);

		// 编号后互不冲突：两份记忆分别落位
		Map<String, String> store = new HashMap<String, String>();
		WindowPlacementMemory.remember(store, "s", out.get(0), new Placement(1, 1, 1, 0, 2));
		WindowPlacementMemory.remember(store, "s", out.get(2), new Placement(2, 2, 2, 0, 2));
		assertEquals(1.0, WindowPlacementMemory.lookup(store, "s", "qq").x(), 1e-3);
		assertEquals(2.0, WindowPlacementMemory.lookup(store, "s", "qq#2").x(), 1e-3);
	}

	@Test
	@DisplayName("handle→别名映射：slug 化标题、空标题归 window、大小写无关合并编号")
	void memoryAliasesByHandle() {
		Map<Long, String> m = WindowPlacementMemory.memoryAliasesByHandle(
			List.of(1L, 2L, 3L), List.of("QQ", "unknown ", "qq 中文"));
		assertEquals("qq", m.get(1L));
		assertEquals("unknown", m.get(2L));
		assertEquals("qq#2", m.get(3L), "slug 相同必须编号区分");
	}

	@Test
	@DisplayName("标题含特殊字符时 slug 稳定（跨会话键不变）")
	void slugStability() {
		assertEquals("firefox_esr", WindowPlacementMemory.slugify("Firefox ESR"));
		assertEquals("google_chrome_beta", WindowPlacementMemory.slugify("Google   Chrome (Beta)"));
		assertEquals("", WindowPlacementMemory.slugify("!!!"));
		assertEquals("", WindowPlacementMemory.slugify(null));
	}

	@Test
	@DisplayName("handle/title 数量不一致直接拒绝（编号规则的前提被破坏时必须显式失败）")
	void mismatchRejected() {
		try {
			WindowPlacementMemory.memoryAliasesByHandle(List.of(1L, 2L), List.of("a"));
			org.junit.jupiter.api.Assertions.fail("应抛 IllegalArgumentException");
		} catch(IllegalArgumentException expected) {
			// 显式失败优于静默错位
		}
	}

	@Test
	@DisplayName("localItems 按服解析：服专用记录优先，缺省回落全局；一切值归一化")
	void resolveLocalItems() {
		Map<String, String> per = new HashMap<String, String>();
		per.put("3c3u.org", "items");
		assertEquals("items", WindowPlacementMemory.resolveLocalItems("3c3u.org", per, "auto"));
		assertEquals("auto", WindowPlacementMemory.resolveLocalItems("other.org", per, "auto"), "其它服不受影响");
		assertEquals("auto", WindowPlacementMemory.resolveLocalItems("", per, "auto"), "无标识 → 全局");
		assertEquals("items", WindowPlacementMemory.resolveLocalItems("3c3u.org", per, null), "按服记录优先，全局为 null 不影响");
		assertEquals("auto", WindowPlacementMemory.resolveLocalItems("fresh.org", per, null), "无按服记录且全局 null → auto");
		per.put("dirty.com", "ITEMS ");
		assertEquals("items", WindowPlacementMemory.resolveLocalItems("dirty.com", per, "auto"));
		per.put("garbage.com", "cheat_me");
		assertEquals("auto", WindowPlacementMemory.resolveLocalItems("garbage.com", per, "auto"), "非法记录按 auto 处理");
	}

	@Test
	@DisplayName("教学'已展示'标记：按服去重、幂等添加、空键不写")
	void tutorialMarks() {
		List<String> marks = new ArrayList<String>();
		assertFalse(WindowPlacementMemory.isMarked(marks, "3c3u.org"));
		WindowPlacementMemory.mark(marks, "3c3u.org");
		assertTrue(WindowPlacementMemory.isMarked(marks, "3c3u.org"));
		WindowPlacementMemory.mark(marks, "3c3u.org");
		assertEquals(1, marks.size(), "幂等");
		WindowPlacementMemory.mark(marks, "");
		assertEquals(1, marks.size());
		assertFalse(WindowPlacementMemory.isMarked(marks, ""), "空键永不视为已展示");
		assertFalse(WindowPlacementMemory.isMarked(null, "x"));
	}
}
