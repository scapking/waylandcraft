package dev.evvie.waylandcraft.appsession;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.concurrent.atomic.AtomicBoolean;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.Adopted;
import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.DisplayProbe;
import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.LaunchVerdict;
import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.StaleVerdict;
import dev.evvie.waylandcraft.bridge.WaylandCraftBridge;

/**
 * 应用会话生命周期的 IO/状态层（判定全部在 {@link AppLifecycleLogic}，本类只采集与执行）。
 *
 * <p>职责：
 * <ul>
 *   <li><b>启动前陈旧检测</b>：{@link #cleanupStaleBeforeLaunch} 扫描同名进程，
 *       读 /proc/&lt;pid&gt;/environ，探 socket 连通性，按判定层清理 STALE_CONFIRMED
 *       孤儿（SIGTERM→有界等待→SIGKILL），根治"新会话 launch 被旧实例吞掉"；</li>
 *   <li><b>交接失败诊断</b>：launch 后 10s（200 tick，与 give 回查同节奏）无新
 *       toplevel 且 CLI 进程已退出 → 聊天可操作提示 + 写入 status error ring；</li>
 *   <li><b>会话收养管理</b>：{@code /wl close} 的精确 PID 树终止、
 *       DISCONNECT/进程退出时终止全部收养树。</li>
 * </ul>
 *
 * <p>线程模型：命令在客户端主线程执行（扫描是纯 /proc 读，毫秒级；杀进程有界等待
 * ≤1.5s 只在命中陈旧实例的罕见路径发生）；tick 每帧 O(挂起观察数≤8)。
 */
public final class AppLifecycle {

	/** 交接观察时限：10s（20 tick/s），与 armGiveVerification 的 watchdog 节奏一致。 */
	private static final int LAUNCH_WATCH_TICKS = 200;
	/** 同时挂起的 launch 观察上限（多开不同应用；同应用重 launch 覆盖旧条目，不刷屏）。 */
	private static final int MAX_WATCHES = 8;
	/** 杀陈旧树时命令线程的有界等待（SIGTERM 落地时间窗）。 */
	private static final long STALE_KILL_WAIT_MS = 1200;
	/** 关机钩子里等待 SIGTERM 自然收尾的上限，超时转 force。 */
	private static final long SHUTDOWN_WAIT_MS = 2000;

	private static final AtomicBoolean SHUTDOWN_HOOK_ARMED = new AtomicBoolean(false);

	private final WaylandCraft wlc;
	private final List<Watch> watches = new ArrayList<Watch>();

	private static final class Watch {
		final String displayName;
		final Integer cliPid; // null = spawn PID 未取得（pipe 失败/旧 native）
		final int toplevelsAtLaunch;
		int ticksLeft;

		Watch(String displayName, Integer cliPid, int toplevelsAtLaunch) {
			this.displayName = displayName;
			this.cliPid = cliPid;
			this.toplevelsAtLaunch = toplevelsAtLaunch;
			this.ticksLeft = LAUNCH_WATCH_TICKS;
		}
	}

	/** 陈旧清理的统计结果（命令面据此输出提示）。 */
	public record CleanupResult(int staleCleaned, int foreignAlive, int ownInstances) {}

	public AppLifecycle(WaylandCraft wlc) {
		this.wlc = wlc;
	}

	// ═══ 收养登记读取 ═══

	/** 当前收养（且存活）的应用进程列表。native 不可用时恒空。 */
	public List<Adopted> adopted() {
		if (wlc == null || wlc.bridge == null) return List.of();
		return AppLifecycleLogic.parseAdopted(wlc.bridge.getAdoptedApps());
	}

	public int adoptedCount() {
		return adopted().size();
	}

	private Set<Integer> adoptedPids() {
		Set<Integer> out = new HashSet<Integer>();
		for (Adopted a : adopted()) out.add(a.pid());
		return out;
	}

	// ═══ ① 启动前陈旧检测 ═══

	/**
	 * 扫描与 entry 同名、确证绑定在**已死 Wayland 会话**上的孤儿进程并终止其进程树。
	 * 宿主桌面实例（socket CONNECTED）与无确证进程绝不误杀（判定见 AppLifecycleLogic）。
	 * 命令线程调用；native 不可用时静默返回全 0（行为退化为修复前，不新增风险）。
	 */
	public CleanupResult cleanupStaleBeforeLaunch(String execLine, String appId) {
		if (wlc == null || wlc.bridge == null) return new CleanupResult(0, 0, 0);
		String ourMarker = wlc.bridge.getSessionMarker();
		String hostDisplay = System.getenv("WAYLAND_DISPLAY");
		Set<String> tokens = AppLifecycleLogic.tokensFor(execLine, appId);
		if (tokens.isEmpty()) return new CleanupResult(0, 0, 0);

		List<Long> stale = new ArrayList<Long>();
		int foreign = 0, own = 0;
		long selfPid = ProcessHandle.current().pid();

		for (ProcessHandle p : ProcessHandle.allProcesses().toList()) {
			long pid = p.pid();
			if (pid <= 1 || pid == selfPid) continue;
			String exe = p.info().command().orElse("");
			String cmdline = p.info().commandLine().orElse("");
			String exeBase = exe.isEmpty() ? "" : AppLifecycleLogic.basename(exe);
			if (!AppLifecycleLogic.matchesApp(tokens, exeBase, cmdline)) continue;

			Map<String, String> env = readEnviron(pid);
			if (env == null) continue; // EPERM/竞态消亡：不可确证 → 不碰
			String display = env.get("WAYLAND_DISPLAY");
			if (display == null || display.isEmpty()) continue;
			String runtimeDir = env.get("XDG_RUNTIME_DIR");
			if (runtimeDir == null || runtimeDir.isEmpty()) {
				runtimeDir = System.getenv("XDG_RUNTIME_DIR"); // 候选没带就用我们的（同用户惯例）
			}
			DisplayProbe probe = probeSocket(AppLifecycleLogic.socketPath(runtimeDir, display));
			StaleVerdict v = AppLifecycleLogic.classify(env, ourMarker, hostDisplay, probe);
			if (v == StaleVerdict.STALE_CONFIRMED) {
				stale.add(pid);
			} else if (v == StaleVerdict.FOREIGN_ALIVE) {
				foreign++;
			} else if (v == StaleVerdict.OWN_SESSION) {
				own++;
			}
		}

		int cleaned = 0;
		// 有界等待是总预算（命令线程在客户端主线程，冻结时间必须封顶）：N 个陈旧
		// 实例均分 ≤1.2s，单实例场景仍是完整窗口。
		long perPid = Math.max(200L, STALE_KILL_WAIT_MS / Math.max(1, stale.size()));
		for (long pid : stale) {
			if (killProcessTree(pid, perPid)) cleaned++;
		}
		if (cleaned > 0) {
			WaylandCraftBridge.noteError("warn", "launch " + appId + ": terminated " + cleaned
					+ " stale app instance(s) bound to a dead Wayland session");
		}
		return new CleanupResult(cleaned, foreign, own);
	}

	/**
	 * 终止一棵外部进程树（陈旧孤儿）：先 SIGTERM 全部（主进程+后代快照），
	 * 有界等待后对幸存者 SIGKILL。返回是否确认主进程消亡。
	 */
	private boolean killProcessTree(long pid, long waitMs) {
		Optional<ProcessHandle> maybe = ProcessHandle.of(pid);
		if (maybe.isEmpty() || !maybe.get().isAlive()) return true;
		List<ProcessHandle> tree = new ArrayList<ProcessHandle>();
		tree.add(maybe.get());
		try {
			maybe.get().descendants().forEach(tree::add); // 快照先于击杀（流是懒的）
		} catch(RuntimeException ignored) {
			// descendants 不可用（权限/竞态）：只杀主进程，子进程由会话收养面兜底
		}
		for (ProcessHandle ph : tree) {
			try { ph.destroy(); } catch(RuntimeException ignored) {}
		}
		long deadline = System.currentTimeMillis() + waitMs;
		while (System.currentTimeMillis() < deadline && maybe.get().isAlive()) {
			try { Thread.sleep(100); } catch (InterruptedException e) {
				Thread.currentThread().interrupt();
				break;
			}
		}
		for (ProcessHandle ph : tree) {
			if (ph.isAlive()) {
				try { ph.destroyForcibly(); } catch(RuntimeException ignored) {}
			}
		}
		return !maybe.get().isAlive();
	}

	private static Map<String, String> readEnviron(long pid) {
		try {
			byte[] raw = Files.readAllBytes(Path.of("/proc/" + pid + "/environ"));
			return AppLifecycleLogic.parseEnviron(new String(raw, StandardCharsets.UTF_8));
		} catch (IOException | RuntimeException e) {
			return null; // EPERM/进程已亡/environ 不可读：交回调用方按"不确证"处理
		}
	}

	// socket 探测抽到 AppSocketProbe（无 Minecraft 依赖，可被真机单测直接验证三条
	// 真实失败路径；JDK 非阻塞 unix connect 的异常形态以该类的实测为准）。
	private static DisplayProbe probeSocket(String path) {
		return AppSocketProbe.probe(path);
	}

	// ═══ ③ 交接失败诊断 ═══

	/**
	 * launch 成功后调用（命令面先 execApp 再 armLaunch 前后各取一次收养快照，
	 * 新出现且匹配 token 的条目即本次 spawn 的 CLI 主进程）。
	 *
	 * @param displayName        聊天回显用应用名
	 * @param beforePids         execApp **之前**的收养 PID 快照
	 * @param toplevelsAtLaunch  execApp 之前的 toplevel 计数
	 * @param execLine/appId     用于在"前快照差集"里认出本次 spawn 的条目
	 */
	public void armLaunchWatch(String displayName, Set<Integer> beforePids, int toplevelsAtLaunch,
			String execLine, String appId) {
		List<Adopted> after = adopted();
		Set<String> tokens = AppLifecycleLogic.tokensFor(execLine, appId);
		Integer cliPid = null;
		for (Adopted a : after) {
			if (beforePids.contains(a.pid())) continue;
			// 新收养条目：优先 cmd/appId 匹配 token 的（并发的另一 launch 不误配）
			if (AppLifecycleLogic.matchesApp(tokens, AppLifecycleLogic.basename(a.cmd()),
					a.cmd() + " " + a.appId())) {
				cliPid = a.pid();
				break;
			}
			if (cliPid == null) cliPid = a.pid(); // 无匹配条目时退化：取任一新增
		}
		// cliPid 可为 null：旧 native/pipe 失败时无 PID 可跟踪，裁决层给 UNTRACKED（静默）
		Watch w = new Watch(displayName, cliPid, toplevelsAtLaunch);
		// 同应用重 launch：覆盖旧观察，不叠加提示
		watches.removeIf((o) -> o.displayName.equals(displayName));
		if (watches.size() >= MAX_WATCHES) watches.remove(0);
		watches.add(w);
	}

	/** 客户端 tick 入口（WaylandCraft.onClientTick 挂接）。 */
	public void tick() {
		if (watches.isEmpty()) return;
		if (wlc == null || wlc.bridge == null) return;
		Watch due = null;
		for (Watch w : watches) {
			if (--w.ticksLeft <= 0) due = w;
		}
		if (due == null) return;
		watches.remove(due);
		evaluate(due);
	}

	private void evaluate(Watch w) {
		int toplevelsNow = wlc.bridge.getToplevels().length;
		boolean tracked = w.cliPid != null && w.cliPid > 0;
		boolean alive = tracked && ProcessHandle.of(w.cliPid.longValue())
				.map(ProcessHandle::isAlive).orElse(false);
		LaunchVerdict v = AppLifecycleLogic.decideLaunch(toplevelsNow, w.toplevelsAtLaunch, tracked, alive);
		if (v != LaunchVerdict.HANDOFF_SUSPECT) return; // 其余裁决保持静默（成功/慢启动/不可判）
		String detail = "launch " + w.displayName + " 后 10 秒无新 toplevel 且 CLI 进程已退出"
				+ "（pid " + w.cliPid + "）：疑似单实例应用把启动请求交接给了绑定死会话的旧实例";
		WaylandCraftBridge.noteError("error", detail);
		try {
			net.minecraft.client.Minecraft.getInstance().getChatListener().handleSystemMessage(
				net.minecraft.network.chat.Component.literal("§eWaylandCraft: " + detail), false);
			net.minecraft.client.Minecraft.getInstance().getChatListener().handleSystemMessage(
				net.minecraft.network.chat.Component.literal(" §7处置：重跑 §e/wl launch " + w.displayName
					+ " §7（会自动清理陈旧实例），或 §e/wl list apps §7 查看本会话收养进程；明细进 §e/wl status §7的 errors 行"),
				false);
		} catch (RuntimeException e) {
			WaylandCraftCommon.LOGGER.warn("[adopt] handoff notice failed: {}", e.toString());
		}
	}

	// ═══ ④ 会话结束：终止全部收养树 ═══

	/** DISCONNECT 路径：非阻塞终止收养树（升级在 native 后台线程，tick 不陪跑）。 */
	public void onSessionEnd() {
		watches.clear();
		if (wlc != null && wlc.bridge != null) {
			wlc.bridge.terminateAdoptedAll();
		}
	}

	/**
	 * 进程退出路径（shutdown hook，只装一次）。JVM 一退 native 线程陪葬，升级 kill
	 * 必须在 hook 里走完：先请求优雅终止，轮询登记表最多 2s，对幸存者逐个 force。
	 */
	public static void installShutdownHookOnce() {
		if (!SHUTDOWN_HOOK_ARMED.compareAndSet(false, true)) return;
		try {
			Runtime.getRuntime().addShutdownHook(new Thread(() -> {
				try {
					WaylandCraft w = WaylandCraft.instance;
					WaylandCraftBridge bridge = w != null ? w.bridge : null;
					if (bridge == null) return;
					bridge.terminateAdoptedAll();
					long deadline = System.currentTimeMillis() + SHUTDOWN_WAIT_MS;
					List<Adopted> remaining = AppLifecycleLogic.parseAdopted(bridge.getAdoptedApps());
					while (!remaining.isEmpty() && System.currentTimeMillis() < deadline) {
						Thread.sleep(100);
						remaining = AppLifecycleLogic.parseAdopted(bridge.getAdoptedApps());
					}
					for (Adopted a : remaining) {
						bridge.killAdopted(a.pid(), true); // 宽限耗尽：直接 SIGKILL 树
					}
				} catch (Throwable t) {
					// 关机路径绝不抛（hook 异常只会污染退出日志，救不回来）
					WaylandCraftCommon.LOGGER.warn("[adopt] shutdown cleanup failed: {}", t.toString());
				}
			}, "waylandcraft-adopted-cleanup"));
		} catch (Throwable t) {
			WaylandCraftCommon.LOGGER.warn("[adopt] shutdown hook install failed: {}", t.toString());
		}
	}

	/**
	 * /wl close 用：toplevel → 收养条目。两级归属，全部限定在收养表内（宿主进程
	 * 不在表中，不存在经此误杀的路径）：
	 * ① wayland 凭据 PID 精确匹配 + /proc PGID 同组匹配（native 直连的 wayland 应用）；
	 * ② appId token ↔ 收养 cmd 词形匹配（X11-only 应用经 satellite 接入，凭据 PID
	 *    是 satellite 的，只能按来路找回本会话 spawn 的那棵树）。
	 */
	public Adopted adoptedForToplevel(int toplevelPid, String appId) {
		Integer pgid = null;
		if (toplevelPid > 1) {
			try {
				String stat = Files.readString(Path.of("/proc/" + toplevelPid + "/stat"));
				pgid = AppLifecycleLogic.parsePgid(stat);
			} catch (IOException | RuntimeException e) {
				pgid = null; // 读不到就只按精确 PID 匹配（selectAdopted 内部降级）
			}
		}
		List<Adopted> apps = adopted();
		Adopted exact = AppLifecycleLogic.selectAdopted(apps, toplevelPid, pgid);
		if (exact != null) return exact;
		return AppLifecycleLogic.selectAdoptedByApp(apps, appId);
	}
}
