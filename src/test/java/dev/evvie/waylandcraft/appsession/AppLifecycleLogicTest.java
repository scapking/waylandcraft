package dev.evvie.waylandcraft.appsession;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.Arrays;
import java.util.List;
import java.util.Map;
import java.util.Set;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.Adopted;
import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.DisplayProbe;
import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.LaunchVerdict;
import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.ScanCounts;
import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.StaleVerdict;

/**
 * 单实例应用生命周期判定的防回归锁（fix/app-launcher-lifecycle）。
 *
 * <p>用户实测坐实的缺陷：上一游戏会话 spawn 的 VS Code 跨会话存活（绑死旧合成器
 * socket），新会话 {@code /wl launch code} 被单实例交接吞掉——mapped=0 永远无窗口。
 * 本测试锁定修复的判定核心：**什么能杀、什么绝不能杀、什么要报警**。
 * 红线用例（宿主保护）优先级最高：任何 classify 输入组合都不得把指向活跃会话
 * （CONNECTED）或证据不全（UNKNOWN/无 display/宿主 display 异常）的进程判为可杀。
 */
class AppLifecycleLogicTest {

	// ═══ environ 解析（/proc/&lt;pid&gt;/environ 原始文本 → 映射）═══

	@Test
	@DisplayName("parseEnviron：NUL 分隔键值对，残段/空段忽略，重复键后者覆盖，空值合法")
	void parsesEnvironRobustly() {
		Map<String, String> env = AppLifecycleLogic.parseEnviron(
				"PATH=/usr/bin\0WAYLAND_DISPLAY=wayland-1\0\0garbage-no-eq\0XDG_RUNTIME_DIR=/run/user/1000\0A=B");
		assertEquals("wayland-1", env.get("WAYLAND_DISPLAY"));
		assertEquals("/run/user/1000", env.get("XDG_RUNTIME_DIR"));
		assertEquals("B", env.get("A"));
		assertFalse(env.containsKey("garbage-no-eq"));
		assertEquals("2", AppLifecycleLogic.parseEnviron("A=1\0A=2").get("A")); // 重复键后覆盖
		assertEquals("", AppLifecycleLogic.parseEnviron("A=").get("A"));         // 空值合法
		assertTrue(AppLifecycleLogic.parseEnviron(null).isEmpty());
		assertTrue(AppLifecycleLogic.parseEnviron("").isEmpty());
	}

	// ═══ 会话标记公式（必须与 Rust session_marker_for 逐字符一致）═══

	@Test
	@DisplayName("sessionMarker 公式 = \"<pid>-<display>\"（Rust 侧同式，跨语言锚点）")
	void markerFormulaMatchesRust() {
		assertEquals("707801-wayland-1", AppLifecycleLogic.sessionMarker(707801L, "wayland-1"));
		// 新旧会话即使 socket 名撞车（旧文件删除后被 new_auto 复用），jvmPid 不同 → 标记不同
		assertFalse(AppLifecycleLogic.sessionMarker(100L, "wayland-1")
				.equals(AppLifecycleLogic.sessionMarker(200L, "wayland-1")));
	}

	// ═══ socket 路径与命名模式 ═══

	@Test
	@DisplayName("socketPath：相对名拼 runtimeDir；绝对 display 直用；缺 runtimeDir 返回 null（不可定位≠已死）")
	void socketPathRules() {
		assertEquals("/run/user/1000/wayland-1",
				AppLifecycleLogic.socketPath("/run/user/1000", "wayland-1"));
		assertEquals("/run/user/1000/wayland-1",
				AppLifecycleLogic.socketPath("/run/user/1000/", "wayland-1")); // 尾斜杠容忍
		assertEquals("/tmp/x/wayland-9", AppLifecycleLogic.socketPath(null, "/tmp/x/wayland-9"));
		assertNull(AppLifecycleLogic.socketPath("", "wayland-1"));
		assertNull(AppLifecycleLogic.socketPath("/run/user/1000", null));
	}

	@Test
	@DisplayName("looksLikeWcSocketName：只认 wayland-<数字≤4位>（new_auto 的命名域）")
	void wcSocketNaming() {
		assertTrue(AppLifecycleLogic.looksLikeWcSocketName("wayland-0"));
		assertTrue(AppLifecycleLogic.looksLikeWcSocketName("wayland-123"));
		assertFalse(AppLifecycleLogic.looksLikeWcSocketName("wayland-"));
		assertFalse(AppLifecycleLogic.looksLikeWcSocketName("wayland-abc"));
		assertFalse(AppLifecycleLogic.looksLikeWcSocketName("wl-1"));
		assertFalse(AppLifecycleLogic.looksLikeWcSocketName("/tmp/wayland-1"));
		assertFalse(AppLifecycleLogic.looksLikeWcSocketName(null));
	}

	// ═══ 归属判定 classify：红线所在 ═══

	@Test
	@DisplayName("OWN_SESSION：标记等于本会话——自己 spawn 的实例，绝不进清理名单")
	void ownSessionByMarker() {
		Map<String, String> env = AppLifecycleLogic.parseEnviron(
				"WAYLAND_DISPLAY=wayland-1\0WAYLANDCRAFT_SESSION=500-wayland-1\0");
		assertEquals(StaleVerdict.OWN_SESSION,
				AppLifecycleLogic.classify(env, "500-wayland-1", "wayland-0", DisplayProbe.CONNECTED));
		// 标记相等时即使探测异常也归 OWN（自己的连接状态不是清理决策面，交给 /wl close）
		assertEquals(StaleVerdict.OWN_SESSION,
				AppLifecycleLogic.classify(env, "500-wayland-1", "wayland-0", DisplayProbe.REFUSED));
	}

	@Test
	@DisplayName("红线：CONNECTED 一律 FOREIGN_ALIVE——宿主桌面/另一活跃会话实例绝不误杀")
	void connectedIsNeverKillable() {
		String hostEnv = "WAYLAND_DISPLAY=wayland-0\0DISPLAY=:0\0"; // 宿主桌面 VS Code
		Map<String, String> noMarker = AppLifecycleLogic.parseEnviron(hostEnv);
		assertEquals(StaleVerdict.FOREIGN_ALIVE,
				AppLifecycleLogic.classify(noMarker, "500-wayland-1", "wayland-0", DisplayProbe.CONNECTED));

		// 带别的 wc 标记但 socket 活着 = 另一局在跑的游戏的会话——同样不碰
		Map<String, String> otherWc = AppLifecycleLogic.parseEnviron(
				"WAYLAND_DISPLAY=wayland-2\0WAYLANDCRAFT_SESSION=999-wayland-2\0");
		assertEquals(StaleVerdict.FOREIGN_ALIVE,
				AppLifecycleLogic.classify(otherWc, "500-wayland-1", "wayland-0", DisplayProbe.CONNECTED));
	}

	@Test
	@DisplayName("STALE_CONFIRMED（带标记）：旧 wc 会话孤儿，socket REFUSED/MISSING 确证死亡")
	void staleByMarkerDeadSocket() {
		Map<String, String> env = AppLifecycleLogic.parseEnviron(
				"WAYLAND_DISPLAY=wayland-3\0WAYLANDCRAFT_SESSION=411-wayland-3\0");
		assertEquals(StaleVerdict.STALE_CONFIRMED,
				AppLifecycleLogic.classify(env, "500-wayland-1", "wayland-0", DisplayProbe.REFUSED));
		assertEquals(StaleVerdict.STALE_CONFIRMED,
				AppLifecycleLogic.classify(env, "500-wayland-1", "wayland-0", DisplayProbe.MISSING));
	}

	@Test
	@DisplayName("STALE_CONFIRMED（无标记遗留孤儿）：用户 1.4.1 实测场景——旧版 spawn 无标记，"
			+ "WAYLAND_DISPLAY 是 wayland-N 惯例名且 socket 已死且非宿主 display")
	void staleBySocketDeathWithoutMarker() {
		// 现场还原：15:36 新会话 launch code，13:49 旧会话的 code 还在（无标记、wayland-1 已死）
		Map<String, String> legacyOrphan = AppLifecycleLogic.parseEnviron(
				"WAYLAND_DISPLAY=wayland-1\0XDG_RUNTIME_DIR=/run/user/1000\0");
		assertEquals(StaleVerdict.STALE_CONFIRMED,
				AppLifecycleLogic.classify(legacyOrphan, "500-wayland-2", "wayland-0", DisplayProbe.REFUSED));
		assertEquals(StaleVerdict.STALE_CONFIRMED,
				AppLifecycleLogic.classify(legacyOrphan, "500-wayland-2", "wayland-0", DisplayProbe.MISSING));
	}

	@Test
	@DisplayName("红线：NOT_PROVABLE 全家桶——证据不足一律不清理")
	void insufficientEvidenceNeverKills() {
		// ① 没有 WAYLAND_DISPLAY（纯 X11 应用）
		Map<String, String> x11Only = AppLifecycleLogic.parseEnviron("DISPLAY=:1\0");
		assertEquals(StaleVerdict.NOT_PROVABLE,
				AppLifecycleLogic.classify(x11Only, "500-wayland-1", "wayland-0", DisplayProbe.REFUSED));
		// ② display 有但探测 UNKNOWN（权限/异常）
		Map<String, String> deadUnknown = AppLifecycleLogic.parseEnviron("WAYLAND_DISPLAY=wayland-1\0");
		assertEquals(StaleVerdict.NOT_PROVABLE,
				AppLifecycleLogic.classify(deadUnknown, "500-wayland-1", "wayland-0", DisplayProbe.UNKNOWN));
		// ③ 指向宿主 display 本身却"探测已死"= 异常状态（我们还跑在上面），宁可不杀
		Map<String, String> hostDisplayDead = AppLifecycleLogic.parseEnviron("WAYLAND_DISPLAY=wayland-0\0");
		assertEquals(StaleVerdict.NOT_PROVABLE,
				AppLifecycleLogic.classify(hostDisplayDead, "500-wayland-1", "wayland-0", DisplayProbe.REFUSED));
		// ④ 无标记 + 非 wayland-N 惯例名（宿主自定义 socket 名）→ 不是确证的 wc 孤儿
		Map<String, String> oddName = AppLifecycleLogic.parseEnviron("WAYLAND_DISPLAY=gnome-shell-abc\0");
		assertEquals(StaleVerdict.NOT_PROVABLE,
				AppLifecycleLogic.classify(oddName, "500-wayland-1", "wayland-0", DisplayProbe.MISSING));
		// ⑤ env 为 null（environ 不可读，调用方已跳过；防御双保险）
		assertEquals(StaleVerdict.NOT_PROVABLE,
				AppLifecycleLogic.classify(null, "500-wayland-1", "wayland-0", DisplayProbe.REFUSED));
		// ⑥ native 不可用（ourMarker 空串）时不得把任何进程判成 OWN（退化安全）
		Map<String, String> anyMarker = AppLifecycleLogic.parseEnviron(
				"WAYLAND_DISPLAY=wayland-1\0WAYLANDCRAFT_SESSION=500-wayland-1\0");
		assertEquals(StaleVerdict.FOREIGN_ALIVE,
				AppLifecycleLogic.classify(anyMarker, "", "wayland-0", DisplayProbe.CONNECTED));
	}

	// ═══ desktop entry → 匹配 token ═══

	@Test
	@DisplayName("execToken：跳过 %码/选项/env 赋值与包装器，取真程序 basename")
	void execTokenUnwraps() {
		assertEquals("code", AppLifecycleLogic.execToken("code --ozone-platform-hint=auto %U"));
		assertEquals("code", AppLifecycleLogic.execToken("/usr/share/code/code --unity-launch %U"));
		assertEquals("firefox", AppLifecycleLogic.execToken("env MOZ_ENABLE_WAYLAND=1 firefox %u"));
		assertEquals("com.tencent.WeChat",
				AppLifecycleLogic.execToken("flatpak run com.tencent.WeChat --test-addon %U"));
		assertEquals("", AppLifecycleLogic.execToken(null));
		assertEquals("", AppLifecycleLogic.execToken("%f %U"));
	}

	@Test
	@DisplayName("tokensFor：exec 主程序 + appId 去 .desktop，去空")
	void tokensUnion() {
		Set<String> t = AppLifecycleLogic.tokensFor("code --ozone-platform-hint=auto %U", "code.desktop");
		assertTrue(t.contains("code"));
		assertEquals(1, t.size()); // exec 与 appId 同名去重

		Set<String> fp = AppLifecycleLogic.tokensFor("flatpak run com.tencent.WeChat", "com.tencent.WeChat.desktop");
		assertTrue(fp.contains("com.tencent.WeChat"));

		Set<String> onlyApp = AppLifecycleLogic.tokensFor(null, "OnlyApp.desktop");
		assertEquals(Set.of("OnlyApp"), onlyApp);
	}

	@Test
	@DisplayName("matchesApp：exe/路径段/参数词形命中；无关联进程不误配")
	void matchingWordShapes() {
		Set<String> tokens = Set.of("code");
		assertTrue(AppLifecycleLogic.matchesApp(tokens, "code", "/usr/share/code/code --type=zygote"));
		assertTrue(AppLifecycleLogic.matchesApp(tokens, "code_helper", "/usr/share/code/code")); // exe "code" 变体名走 startsWith("code-")
		assertTrue(AppLifecycleLogic.matchesApp(tokens, "electron", "/usr/share/code/code --unity-launch")); // 路径段 /code
		assertTrue(AppLifecycleLogic.matchesApp(tokens, "cli", "code --install-extension x"));   // 行首 token
		assertFalse(AppLifecycleLogic.matchesApp(tokens, "xcodebuild", "/usr/bin/xcodebuild -list"));
		assertFalse(AppLifecycleLogic.matchesApp(tokens, "node", "/usr/bin/node scripts/code.js".replace("code.js", "xyz.js")));
		assertTrue(AppLifecycleLogic.matchesApp(Set.of("com.tencent.WeChat"), "flatpak",
				"flatpak run com.tencent.WeChat"));
		assertFalse(AppLifecycleLogic.matchesApp(Set.of(), "code", "code"));
	}

	// ═══ 交接失败裁决 ═══

	@Test
	@DisplayName("decideLaunch：toplevel 增加即正常，不论进程状态")
	void displayedWins() {
		assertEquals(LaunchVerdict.DISPLAYED, AppLifecycleLogic.decideLaunch(2, 1, true, false));
	}

	@Test
	@DisplayName("decideLaunch：无新 toplevel + CLI 已退出 = HANDOFF_SUSPECT（报警线）")
	void handoffSuspect() {
		assertEquals(LaunchVerdict.HANDOFF_SUSPECT, AppLifecycleLogic.decideLaunch(1, 1, true, false));
	}

	@Test
	@DisplayName("decideLaunch：进程还活着是慢启动/其它故障，不诬陷交接；无 PID 静默")
	void stillRunningAndUntracked() {
		assertEquals(LaunchVerdict.APP_STILL_RUNNING, AppLifecycleLogic.decideLaunch(0, 0, true, true));
		assertEquals(LaunchVerdict.UNTRACKED, AppLifecycleLogic.decideLaunch(0, 0, false, false));
	}

	// ═══ /proc/&lt;pid&gt;/stat PGID 解析（comm 可含空格/括号）═══

	@Test
	@DisplayName("parsePgid：锚定最后一个 ')'，索引 2 = pgrp；坏输入 null")
	void pgidParsing() {
		// 正常："pid (comm) state ppid pgrp session tty_nr ..."
		assertEquals(Integer.valueOf(4243),
				AppLifecycleLogic.parsePgid("4243 (code) S 4200 4243 4242 0 -1 4194304 100"));
		// comm 含空格与右括号：必须锚定**最后**一个 ')'
		assertEquals(Integer.valueOf(700), AppLifecycleLogic.parsePgid(
				"700 (my (weird) app) R 690 700 690 0 -1 4194304 1"));
		assertNull(AppLifecycleLogic.parsePgid("garbage"));
		assertNull(AppLifecycleLogic.parsePgid(null));
		assertNull(AppLifecycleLogic.parsePgid("1 (a)"));
		assertNull(AppLifecycleLogic.parsePgid("1 (a) S 2"));
		assertNull(AppLifecycleLogic.parsePgid("1 (a) S 2 notanumber"));
	}

	// ═══ 收养表解析与 toplevel→收养条目映射 ═══

	@Test
	@DisplayName("parseAdopted：坏行跳过、尾段竖线保留（limit 4）、pid<=1 剔除、短格式降级")
	void adoptedParsing() {
		List<Adopted> apps = AppLifecycleLogic.parseAdopted(new String[] {
				"100|99|/usr/bin/code|code.desktop",
				"101|101|a|b|c",          // cmd 含竖线：limit 4 → appId 段整体保留原文
				"102|102|only-cmd",       // 短格式（理论不该发）：appId 降级 ""
				"1|1|init-ish|x",         // pid<=1 防御剔除
				"junk",                    // 无竖线跳过
				"x|y|z|w",                 // 非数字跳过
				null,
		});
		assertEquals(3, apps.size());
		assertEquals(new Adopted(100, 99, "/usr/bin/code", "code.desktop"), apps.get(0));
		assertEquals(new Adopted(101, 101, "a", "b|c"), apps.get(1));
		assertEquals(new Adopted(102, 102, "only-cmd", ""), apps.get(2));
		assertTrue(AppLifecycleLogic.parseAdopted(null).isEmpty());
	}

	@Test
	@DisplayName("selectAdopted：先精确 PID，再同 PGID 组归属（helper 子进程），都不中 null")
	void adoptedSelection() {
		List<Adopted> apps = List.of(
				new Adopted(100, 90, "/usr/bin/code", "code.desktop"),
				new Adopted(200, 90, "/usr/bin/code", "code.desktop")); // 同组两条（理论罕见，先 PID 精确命中）
		assertEquals(apps.get(0), AppLifecycleLogic.selectAdopted(apps, 100, null));     // 精确
		assertEquals(apps.get(0), AppLifecycleLogic.selectAdopted(apps, 123, 90));       // PGID 归属
		assertNull(AppLifecycleLogic.selectAdopted(apps, 123, 555));                     // 组不匹配
		assertNull(AppLifecycleLogic.selectAdopted(apps, 123, null));                    // 无 PGID 且 PID 不中
		assertNull(AppLifecycleLogic.selectAdopted(apps, 123, 1));                       // pgid<=1 无效
		assertNull(AppLifecycleLogic.selectAdopted(List.of(), 100, 90));
	}

	@Test
	@DisplayName("selectAdoptedByApp：satellite 窗口按登记 appId 找回；flatpak 运行器 argv0 不再是障碍；仅收养表内匹配")
	void adoptedTokenFallback() {
		List<Adopted> apps = List.of(
				new Adopted(300, 290, "/usr/bin/flatpak", "com.tencent.WeChat"),
				new Adopted(400, 395, "/usr/share/code/code", "code.desktop"),
				new Adopted(500, 490, "/usr/bin/legacy", "")); // 无 appId 的短格式条目
		// X11-only via satellite：凭据 PID 是 satellite 的；flatpak argv0 认不出来路——appId 兜底
		assertEquals(apps.get(0), AppLifecycleLogic.selectAdoptedByApp(apps, "com.tencent.WeChat"));
		assertEquals(apps.get(1), AppLifecycleLogic.selectAdoptedByApp(apps, "code.desktop"));
		// 短格式条目退化到 cmd 词形匹配
		assertEquals(apps.get(2), AppLifecycleLogic.selectAdoptedByApp(apps, "legacy.desktop"));
		// 不在收养表的宿主应用：表里没有它的条目，兜底也返回 null（红线结构保证）
		assertNull(AppLifecycleLogic.selectAdoptedByApp(apps, "org.mozilla.firefox"));
		assertNull(AppLifecycleLogic.selectAdoptedByApp(apps, null));
		assertNull(AppLifecycleLogic.selectAdoptedByApp(List.of(), "code"));
	}

	@Test
	@DisplayName("tally：四态计数（命令面提示的原料）")
	void tallyCounts() {
		ScanCounts c = AppLifecycleLogic.tally(Arrays.asList(
				StaleVerdict.STALE_CONFIRMED, StaleVerdict.STALE_CONFIRMED,
				StaleVerdict.FOREIGN_ALIVE, StaleVerdict.OWN_SESSION, StaleVerdict.NOT_PROVABLE));
		assertEquals(2, c.staleConfirmed());
		assertEquals(1, c.foreignAlive());
		assertEquals(1, c.own());
		assertEquals(1, c.notProvable());
		assertEquals(0, AppLifecycleLogic.tally(List.of()).staleConfirmed());
	}
}
