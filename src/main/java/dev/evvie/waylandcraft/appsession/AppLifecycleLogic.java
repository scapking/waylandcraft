package dev.evvie.waylandcraft.appsession;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;

/**
 * 单实例应用生命周期的纯判定层（fix/app-launcher-lifecycle）。
 *
 * <p>背景（用户实测坐实的缺陷）：上一个游戏会话 spawn 的 VS Code 实例跨会话存活，
 * 它的 Wayland 连接绑在已随旧 JVM 死去的合成器 socket 上；新会话
 * {@code /wl launch code} 时 CLI 把请求"交接"给这个僵尸实例后自行退出——
 * 窗口永远出不来（status 显示 toplevels 增而 mapped=0 或干脆 0）。
 *
 * <p>本类只回答三个"能不能杀/该不该报"的问题，全部输入是已采集的事实
 * （environ 映射、socket 探测结果、计数快照），零 IO、零 Minecraft 依赖，
 * 单测直打。真实采集在 {@link AppLifecycle}。
 *
 * <p>安全红线：**无法确证的宿主同名进程绝不误杀**。保护链三层：
 * <ol>
 *   <li>WAYLANDCRAFT_SESSION 标记等于本会话 → OWN_SESSION（自己人，交给
 *       {@code /wl close} 显式管理）；</li>
 *   <li>socket 探测 CONNECTED → 它连着某个活着的合成器（宿主桌面或另一局
 *       游戏的会话）→ FOREIGN_ALIVE，绝不碰；</li>
 *   <li>探测 UNKNOWN / environ 不可读 / 显示名是游戏自己的宿主 display →
 *       NOT_PROVABLE，绝不碰。STALE_CONFIRMED 只在"确证 socket 已死且非宿主
 *       display"时给出。</li>
 * </ol>
 */
public final class AppLifecycleLogic {

	private AppLifecycleLogic() {}

	/** 候选进程 environ 里 WAYLAND_DISPLAY 指向的 socket 的连通性探测结果。 */
	public enum DisplayProbe {
		/** connect() 成功——有合成器活着在监听（宿主桌面或另一活跃 wc 会话）。 */
		CONNECTED,
		/** socket 文件不存在（ENOENT）。 */
		MISSING,
		/** 文件残留在但无人监听（ECONNREFUSED）——会话进程已死，socket 没来得及清理。 */
		REFUSED,
		/** 权限不足/路径无法解析/其它 IO 异常——不可确证。 */
		UNKNOWN
	}

	/** 对一个候选同名进程的归属判定。 */
	public enum StaleVerdict {
		/** 本会话 spawn 的进程（标记相等）——正常单实例复用，不清理。 */
		OWN_SESSION,
		/** 指向活跃 Wayland 会话的进程（宿主桌面应用/另一局游戏）——绝不触碰。 */
		FOREIGN_ALIVE,
		/** 确证的陈旧实例：其 Wayland 会话 socket 已死且不是宿主桌面 → 可清理。 */
		STALE_CONFIRMED,
		/** 证据不足（无 WAYLAND_DISPLAY、探测不确定、宿主 display 异常等）——不清理。 */
		NOT_PROVABLE
	}

	/** launch 后 N 秒的一次性交接裁决（watchdog 到期时调用）。 */
	public enum LaunchVerdict {
		/** toplevel 数增加了——窗口正常出现。 */
		DISPLAYED,
		/** 无新 toplevel 且 CLI 进程已退出——典型"交接给陈旧实例后自灭"，要报警。 */
		HANDOFF_SUSPECT,
		/** 无新 toplevel 但 CLI 进程还活着——慢启动/其它故障，不是交接问题。 */
		APP_STILL_RUNNING,
		/** spawn 的 PID 没拿到（pipe 不可用/旧 native）——无从判定，静默。 */
		UNTRACKED
	}

	/** 收养登记条目（Rust ADOPTED 表的 Java 投影，线格式 "pid|pgid|cmd|appId"）。 */
	public record Adopted(int pid, int pgid, String cmd, String appId) {}

	/**
	 * 会话标记公式——与 Rust {@code process::session_marker_for} 逐字符一致：
	 * {@code <jvmPid>-<wayland-N>}。改一处必须同步改另一处（本函数是其单测锚点）。
	 */
	public static String sessionMarker(long jvmPid, String waylandDisplay) {
		return jvmPid + "-" + waylandDisplay;
	}

	/**
	 * 解析 /proc/&lt;pid&gt;/environ 的原始字节解码文本（NUL 分隔 KEY=VALUE）。
	 * 无 '=' 的残段与空段忽略；重复键后者覆盖前者（与内核 exec 语义一致）。
	 */
	public static Map<String, String> parseEnviron(String raw) {
		Map<String, String> out = new LinkedHashMap<String, String>();
		if (raw == null || raw.isEmpty()) return Collections.emptyMap();
		for (String entry : raw.split("\0")) {
			if (entry.isEmpty()) continue;
			int eq = entry.indexOf('=');
			if (eq <= 0) continue; // 无值/空键：忽略（environ 损坏不容崩溃）
			out.put(entry.substring(0, eq), entry.substring(eq + 1));
		}
		return out;
	}

	/**
	 * Wayland socket 路径：display 以 '/' 开头即绝对路径直接用；否则拼
	 * runtimeDir。runtimeDir 缺失（null/空）无法定位 → 返回 null（调用方按
	 * UNKNOWN 处理，绝不误判 MISSING）。
	 */
	public static String socketPath(String runtimeDir, String display) {
		if (display == null || display.isEmpty()) return null;
		if (display.startsWith("/")) return display;
		if (runtimeDir == null || runtimeDir.isEmpty()) return null;
		String dir = runtimeDir.endsWith("/") ? runtimeDir.substring(0, runtimeDir.length() - 1) : runtimeDir;
		return dir + "/" + display;
	}

	/** wc 合成器 socket 命名模式：ListeningSocketSource::new_auto 只会生成 wayland-&lt;数字&gt;。 */
	public static boolean looksLikeWcSocketName(String display) {
		if (display == null) return false;
		if (!display.startsWith("wayland-")) return false;
		String suffix = display.substring("wayland-".length());
		if (suffix.isEmpty() || suffix.length() > 4) return false;
		for (int i = 0; i < suffix.length(); i++) {
			if (!Character.isDigit(suffix.charAt(i))) return false;
		}
		return true;
	}

	/**
	 * 归属判定（本类核心，红线所在）。输入全是已采集事实：
	 *
	 * @param env         候选进程 environ（{@link #parseEnviron}）
	 * @param ourMarker   本会话标记（空串=native 不可用，判定退化不报 OWN）
	 * @param hostDisplay 游戏进程自己的 WAYLAND_DISPLAY（宿主桌面会话名，可能为 null——
	 *                    游戏跑在 X11 下时）
	 * @param probe       候选进程 WAYLAND_DISPLAY 的 socket 连通性探测
	 */
	public static StaleVerdict classify(Map<String, String> env, String ourMarker,
			String hostDisplay, DisplayProbe probe) {
		if (env == null) return StaleVerdict.NOT_PROVABLE;
		String display = env.get("WAYLAND_DISPLAY");
		if (display == null || display.isEmpty()) {
			// 纯 X11 应用（走 satellite 或宿主 X）：wayland 孤儿判定无从谈起。
			// 这类进程不吞 wc 的单实例交接（Electron 的 socket 锁与 DISPLAY 无关？——
			// 保守原则：不确定就不碰，交给交接失败诊断提示用户）。
			return StaleVerdict.NOT_PROVABLE;
		}
		String marker = env.get("WAYLANDCRAFT_SESSION");
		boolean marked = marker != null && !marker.isEmpty();
		if (marked && !ourMarker.isEmpty() && marker.equals(ourMarker)) {
			return StaleVerdict.OWN_SESSION;
		}
		switch (probe) {
			case CONNECTED:
				// 它连着活合成器：宿主桌面实例或另一局游戏的会话，永远不碰。
				return StaleVerdict.FOREIGN_ALIVE;
			case REFUSED:
			case MISSING: {
				if (display.equals(hostDisplay)) {
					// 宿主 display"已死"而我们还跑在上面 = 异常状态（权限/路径问题），
					// 不可靠——宁可不杀。
					return StaleVerdict.NOT_PROVABLE;
				}
				if (marked) {
					// 有 wc 标记、≠ 本会话、socket 已死：确证的旧 wc 会话孤儿。
					return StaleVerdict.STALE_CONFIRMED;
				}
				// 无标记（1.4.1 及更早 spawn 的遗留孤儿，或宿主程序）：只有名字符合
				// wc 的 wayland-N 惯例才认陈旧——用户复测场景（旧会话 code 孤儿）走这条。
				return looksLikeWcSocketName(display) ? StaleVerdict.STALE_CONFIRMED
						: StaleVerdict.NOT_PROVABLE;
			}
			default: // UNKNOWN
				return StaleVerdict.NOT_PROVABLE;
		}
	}

	/**
	 * desktop entry Exec 行的主程序 token：跳过 %f 字段码、-选项与 env/flatpak
	 * 类包装器，取第一个"像可执行程序"的 token 的 basename。
	 * 例：{@code env WAYLAND_DEBUG=1 code --user-data-…} → code；
	 * {@code /usr/bin/flatpak run --branch=stable com.x.Y} → com.x.y（由 appId token 兜底）。
	 */
	public static String execToken(String execLine) {
		if (execLine == null) return "";
		String[] parts = execLine.trim().split("\\s+");
		for (int i = 0; i < parts.length; i++) {
			String t = parts[i];
			if (t.isEmpty() || t.startsWith("%") || t.startsWith("-") || t.startsWith("$")) continue;
			if (t.contains("=")) continue; // env VAR=VAL 形式的赋值段
			String base = basename(t);
			// 常见"再 exec 真身"的包装器：跳过继续找真正的应用名
			if (base.equals("env") || base.equals("sh") || base.equals("bash")
					|| base.equals("flatpak") || base.equals("snap") || base.equals("run")
					|| base.equals("wrap") || base.equals("bwrap")) {
				continue;
			}
			return base;
		}
		return "";
	}

	public static String basename(String path) {
		String p = path.endsWith("/") ? path.substring(0, path.length() - 1) : path;
		int idx = p.lastIndexOf('/');
		return idx >= 0 ? p.substring(idx + 1) : p;
	}

	/** appId → 匹配 token：去掉 .desktop 后缀（"code.desktop"→"code"，"com.x.Y.desktop"→"com.x.Y"）。 */
	public static String appToken(String appId) {
		if (appId == null) return "";
		String a = appId.trim();
		if (a.toLowerCase(Locale.ROOT).endsWith(".desktop")) {
			a = a.substring(0, a.length() - ".desktop".length());
		}
		return a;
	}

	/**
	 * 一个候选进程是否值得进 classify 检查（宽松筛、严格判——这里宁可多送几个
	 * 候选进判定层，红线由 classify 的三态守护）。词形规则：
	 * exe basename 相等或以 "token-" 开头（helper 变体）；cmdline 含 "/token"
	 * （路径段）、" token"（参数段）、以 "token " 开头或整行等于 token。
	 */
	public static boolean matchesApp(Set<String> tokens, String procExeBase, String procCmdline) {
		if (tokens == null || tokens.isEmpty()) return false;
		String cl = procCmdline == null ? "" : procCmdline.toLowerCase(Locale.ROOT);
		String exe = procExeBase == null ? "" : procExeBase.toLowerCase(Locale.ROOT);
		for (String raw : tokens) {
			if (raw == null || raw.isEmpty()) continue;
			String tok = raw.toLowerCase(Locale.ROOT);
			if (!exe.isEmpty() && (exe.equals(tok) || exe.startsWith(tok + "-"))) return true;
			if (!cl.isEmpty() && (cl.contains("/" + tok) || cl.contains(" " + tok)
					|| cl.startsWith(tok + " ") || cl.equals(tok))) return true;
		}
		return false;
	}

	/**
	 * 交接失败裁决（watchdog 到期时调用一次）。
	 *
	 * @param toplevelsNow      当前 toplevel 总数
	 * @param toplevelsAtLaunch launch 时刻快照
	 * @param pidTracked        spawn 时是否拿到 CLI 主进程 PID
	 * @param pidAlive          该 PID 现在是否存活（ProcessHandle 实测）
	 */
	public static LaunchVerdict decideLaunch(int toplevelsNow, int toplevelsAtLaunch,
			boolean pidTracked, boolean pidAlive) {
		if (toplevelsNow > toplevelsAtLaunch) return LaunchVerdict.DISPLAYED;
		if (!pidTracked) return LaunchVerdict.UNTRACKED;
		return pidAlive ? LaunchVerdict.APP_STILL_RUNNING : LaunchVerdict.HANDOFF_SUSPECT;
	}

	/**
	 * 解析 /proc/&lt;pid&gt;/stat 的 PGID 字段。格式 "pid (comm) state ppid pgrp …"，
	 * comm 可含空格/括号 → 锚定**最后一个** ')' 之后的字段（索引 2 = pgrp）。
	 * 非法输入返回 null（调用方保守处理）。
	 */
	public static Integer parsePgid(String statLine) {
		if (statLine == null) return null;
		int close = statLine.lastIndexOf(')');
		if (close < 0 || close + 1 >= statLine.length()) return null;
		String[] rest = statLine.substring(close + 1).trim().split("\\s+");
		if (rest.length < 3) return null; // state ppid pgrp
		try {
			return Integer.parseInt(rest[2]);
		} catch (NumberFormatException e) {
			return null;
		}
	}

	/**
	 * 解析 bridge.getAdoptedApps() 的 "pid|pgid|cmd|appId" 数组。
	 * limit 4：cmd/appId 含竖线时尾段整体保留，不破坏结构；缺 appId 的短格式
	 * （理论不该出现，格式双端各改各的旧数据）降级 appId=""。坏行静默跳过。
	 */
	public static List<Adopted> parseAdopted(String[] entries) {
		if (entries == null) return Collections.emptyList();
		List<Adopted> out = new ArrayList<Adopted>(entries.length);
		for (String e : entries) {
			if (e == null) continue;
			String[] parts = e.split("\\|", 4);
			if (parts.length < 2) continue;
			try {
				int pid = Integer.parseInt(parts[0].trim());
				int pgid = Integer.parseInt(parts[1].trim());
				if (pid <= 1) continue; // Rust 表不可能有，防御性剔除
				String cmd = parts.length > 2 ? parts[2] : "";
				String appId = parts.length > 3 ? parts[3] : "";
				out.add(new Adopted(pid, pgid, cmd, appId));
			} catch (NumberFormatException ignored) {
				// 格式破坏（理论不可达）：跳过该条而非抛错——诊断面不反噬命令面
			}
		}
		return out;
	}

	/**
	 * {@code /wl close} 用：把 toplevel 的 wayland 凭据 PID（可能不是收养主进程，
	 * 而是同组子进程）映射到收养条目。先精确 PID，再按 PGID 同组归属；都不中返回 null
	 * （调用方据此拒绝 pkill 式盲杀）。
	 */
	public static Adopted selectAdopted(List<Adopted> apps, int toplevelPid, Integer toplevelPgid) {
		if (apps == null || apps.isEmpty()) return null;
		for (Adopted a : apps) {
			if (a.pid() == toplevelPid) return a;
		}
		if (toplevelPgid != null && toplevelPgid > 1) {
			for (Adopted a : apps) {
				if (a.pgid() == toplevelPgid.intValue()) return a;
			}
		}
		return null;
	}

	/**
	 * PID 匹配失败时的兜底归属（仅限收养表内！）：X11-only 应用经 xwayland-satellite
	 * 连入合成器，其凭据 PID 是 satellite 而非收养的应用进程；另外 flatpak 应用的
	 * argv0 只是运行器——两条都靠登记的 appId 认来路，退化路径才用 cmd 词形匹配。
	 * 安全边界：候选只有**本会话 spawn 过**的进程（收养表），宿主同名进程根本不在
	 * 表里，不存在经此路径被误杀的可能。
	 */
	public static Adopted selectAdoptedByApp(List<Adopted> apps, String appId) {
		if (apps == null || apps.isEmpty()) return null;
		String tok = appToken(appId);
		if (tok.isEmpty()) return null;
		for (Adopted a : apps) {
			if (!a.appId().isEmpty() && a.appId().equalsIgnoreCase(tok)) return a;
		}
		// 短格式/无登记 appId 的条目：按 cmd 词形兜底
		Set<String> tokens = Collections.singleton(tok);
		for (Adopted a : apps) {
			if (matchesApp(tokens, basename(a.cmd()), a.cmd())) return a;
		}
		return null;
	}

	/** launch 前置扫描的分类汇总（IO 层按此展示计数，纯逻辑便于单测）。 */
	public record ScanCounts(int own, int foreignAlive, int staleConfirmed, int notProvable) {
		public static ScanCounts zero() {
			return new ScanCounts(0, 0, 0, 0);
		}
	}

	/** 对候选集计数（候选 = matchesApp 通过后的 (environ, probe) 列表）。 */
	public static ScanCounts tally(Iterable<StaleVerdict> verdicts) {
		int own = 0, foreign = 0, stale = 0, unknown = 0;
		for (StaleVerdict v : verdicts) {
			switch (v) {
				case OWN_SESSION -> own++;
				case FOREIGN_ALIVE -> foreign++;
				case STALE_CONFIRMED -> stale++;
				case NOT_PROVABLE -> unknown++;
			}
		}
		return new ScanCounts(own, foreign, stale, unknown);
	}

	/** desktop entry → 候选匹配 token 集（exec 主程序名 + appId 去后缀，空值剔除）。 */
	public static Set<String> tokensFor(String execLine, String appId) {
		Set<String> t = new LinkedHashSet<String>();
		String exec = execToken(execLine);
		if (!exec.isEmpty()) t.add(exec);
		String app = appToken(appId);
		if (!app.isEmpty()) t.add(app);
		return Collections.unmodifiableSet(t);
	}
}
