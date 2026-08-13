package dev.evvie.waylandcraft.command;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

import com.mojang.brigadier.CommandDispatcher;
import com.mojang.brigadier.arguments.DoubleArgumentType;
import com.mojang.brigadier.arguments.FloatArgumentType;
import com.mojang.brigadier.arguments.IntegerArgumentType;
import com.mojang.brigadier.arguments.StringArgumentType;
import com.mojang.brigadier.context.CommandContext;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WindowDisplay;
import dev.evvie.waylandcraft.WindowTemplateManager;
import dev.evvie.waylandcraft.bridge.WaylandCraftBridge;
import dev.evvie.waylandcraft.bridge.WLCToplevel;
import dev.evvie.waylandcraft.capture.PipeWireCaptureManager;
import dev.evvie.waylandcraft.desktop.DesktopEntry;
import dev.evvie.waylandcraft.grabs.WindowGrab;
import dev.evvie.waylandcraft.gui.WindowManagerScreen;
import dev.evvie.waylandcraft.network.PermissionCommandPayload;
import dev.evvie.waylandcraft.network.SharedWindowClientHandler;
import dev.evvie.waylandcraft.settings.WaylandCraftSettings;
import dev.evvie.waylandcraft.shared.ImageCapture;
import dev.evvie.waylandcraft.shared.WindowPermission;
import dev.evvie.waylandcraft.shared.WindowShareManager;
import dev.evvie.waylandcraft.utils.X11WindowLister;
import net.fabricmc.fabric.api.client.command.v2.ClientCommandRegistrationCallback;
import net.fabricmc.fabric.api.client.command.v2.FabricClientCommandSource;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking;
import net.minecraft.client.Camera;
import net.minecraft.client.Minecraft;
import net.fabricmc.fabric.api.client.command.v2.ClientCommands;
import net.minecraft.commands.CommandBuildContext;
import net.minecraft.network.chat.Component;

public class WaylandCraftCommand {

	private static final String SHORT_PREFIX = "0x";

	public static void register() {
		ClientCommandRegistrationCallback.EVENT.register(WaylandCraftCommand::registerCommands);
	}

	private static void registerCommands(CommandDispatcher<FabricClientCommandSource> dispatcher, CommandBuildContext registryAccess) {
		dispatcher.register(
			ClientCommands.literal("wl")
				.executes(WaylandCraftCommand::showHelp)
				.then(ClientCommands.literal("help")
					.executes(WaylandCraftCommand::showHelp)
				)
				.then(ClientCommands.literal("list")
					.executes(WaylandCraftCommand::listApps)
					.then(ClientCommands.literal("windows")
						.executes(WaylandCraftCommand::listWindows)
					)
					.then(ClientCommands.literal("apps")
						.executes(WaylandCraftCommand::listApps)
					)
					.then(ClientCommands.literal("desktop")
						.executes(WaylandCraftCommand::listDesktopWindows)
					)
				)
				.then(ClientCommands.literal("launch")
					.then(ClientCommands.argument("app_name", StringArgumentType.greedyString())
						.executes(WaylandCraftCommand::launchWindow)
					)
				)
				.then(ClientCommands.literal("give")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.executes(WaylandCraftCommand::giveWindowItem)
					)
				)
				.then(ClientCommands.literal("take")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.executes(WaylandCraftCommand::takeWindowItem)
					)
				)
				.then(ClientCommands.literal("capture")
					.executes(WaylandCraftCommand::captureWindow)
				)
				.then(ClientCommands.literal("grab")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.executes(WaylandCraftCommand::grabWindow)
					)
				)
				.then(ClientCommands.literal("show")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.executes(WaylandCraftCommand::showWindow)
					)
				)
				.then(ClientCommands.literal("hide")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.executes(WaylandCraftCommand::hideWindow)
					)
				)
				.then(ClientCommands.literal("pin")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.executes(WaylandCraftCommand::pinWindow)
					)
				)
				.then(ClientCommands.literal("unpin")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.executes(WaylandCraftCommand::unpinWindow)
					)
				)
				.then(ClientCommands.literal("close")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.executes(WaylandCraftCommand::closeWindow)
					)
				)
				.then(ClientCommands.literal("resize")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.then(ClientCommands.argument("width", IntegerArgumentType.integer(1, 10000))
							.then(ClientCommands.argument("height", IntegerArgumentType.integer(1, 10000))
								.executes(WaylandCraftCommand::resizeWindow)
							)
						)
					)
				)
				.then(ClientCommands.literal("settings")
					.then(ClientCommands.literal("list")
						.executes(WaylandCraftCommand::listSettings)
					)
					.then(ClientCommands.literal("set")
						.then(ClientCommands.argument("key", StringArgumentType.word())
							.then(ClientCommands.argument("value", StringArgumentType.word())
								.executes(WaylandCraftCommand::setSetting)
							)
						)
					)
				)
				.then(ClientCommands.literal("share")
					.then(ClientCommands.literal("start")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.executes(WaylandCraftCommand::shareWindow)
						)
					)
					.then(ClientCommands.literal("stop")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.executes(WaylandCraftCommand::unshareWindow)
						)
					)
					.then(ClientCommands.literal("quality")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.then(ClientCommands.argument("scale", FloatArgumentType.floatArg(0.1f, 1.0f))
								.then(ClientCommands.argument("quality", FloatArgumentType.floatArg(0.1f, 1.0f))
									.then(ClientCommands.argument("fps", IntegerArgumentType.integer(0, 240))
										.executes(WaylandCraftCommand::setShareQuality)
									)
								)
							)
						)
					)
					.then(ClientCommands.literal("preset")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.then(ClientCommands.argument("preset", StringArgumentType.word())
								.suggests((ctx, builder) -> {
									for (String p : new String[]{"performance", "quality", "balanced", "lowlatency"})
										builder.suggest(p);
									return builder.buildFuture();
								})
								.executes(WaylandCraftCommand::applySharePreset)
							)
						)
					)
					.then(ClientCommands.literal("config")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.then(ClientCommands.argument("param", StringArgumentType.word())
								.suggests((ctx, builder) -> {
									for (String p : new String[]{"scale", "quality", "fps", "diff", "bitrate", "buffer", "latency", "prediction", "compression", "diffThreshold"})
										builder.suggest(p);
									return builder.buildFuture();
								})
								.then(ClientCommands.argument("value", StringArgumentType.word())
									.executes(WaylandCraftCommand::setShareConfig)
								)
							)
						)
					)
					.then(ClientCommands.literal("reset")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.executes(WaylandCraftCommand::resetShareQuality)
						)
					)
					.then(ClientCommands.literal("info")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.executes(WaylandCraftCommand::showShareConfig)
						)
					)
					.then(ClientCommands.literal("resolution")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.then(ClientCommands.argument("width", IntegerArgumentType.integer(1, 3840))
								.then(ClientCommands.argument("height", IntegerArgumentType.integer(1, 2160))
									.executes(WaylandCraftCommand::setShareResolution)
								)
							)
						)
					)
					.then(ClientCommands.literal("stats")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.executes(WaylandCraftCommand::showShareStats)
						)
					)
				)
				// X11 窗口共享（微信等 X11-only 应用）
				.then(ClientCommands.literal("x11")
					.then(ClientCommands.literal("list")
						.executes(WaylandCraftCommand::x11List)
						.then(ClientCommands.argument("display", StringArgumentType.word())
							.executes(WaylandCraftCommand::x11List)
						)
					)
					.then(ClientCommands.literal("share")
						.then(ClientCommands.argument("index", IntegerArgumentType.integer(1))
							.executes(WaylandCraftCommand::x11Share)
						)
					)
					.then(ClientCommands.literal("stop")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.executes(WaylandCraftCommand::x11Stop)
						)
					)
				)
				// 权限管理 - 任意玩家可用
				.then(ClientCommands.literal("permission")
					.then(ClientCommands.literal("list")
						.executes(WaylandCraftCommand::permList)
					)
					.then(ClientCommands.literal("default")
						.then(ClientCommands.argument("permission", StringArgumentType.word())
							.suggests((ctx, builder) -> {
								for (WindowPermission p : WindowPermission.values()) builder.suggest(p.name());
								return builder.buildFuture();
							})
							.executes(WaylandCraftCommand::permDefault)
						)
					)
					.then(ClientCommands.literal("allow")
						.then(ClientCommands.argument("player", StringArgumentType.word())
							.then(ClientCommands.argument("permission", StringArgumentType.word())
								.suggests((ctx, builder) -> {
									for (WindowPermission p : WindowPermission.values()) builder.suggest(p.name());
									return builder.buildFuture();
								})
								.executes(WaylandCraftCommand::permAllow)
							)
						)
					)
					.then(ClientCommands.literal("deny")
						.then(ClientCommands.argument("player", StringArgumentType.word())
							.executes(WaylandCraftCommand::permDeny)
						)
					)
					.then(ClientCommands.literal("remove")
						.then(ClientCommands.argument("player", StringArgumentType.word())
							.executes(WaylandCraftCommand::permRemove)
						)
					)
				)
				.then(ClientCommands.literal("pos")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.executes(WaylandCraftCommand::posWindow)
					)
				)
				.then(ClientCommands.literal("move")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.then(ClientCommands.argument("x", StringArgumentType.word())
							.then(ClientCommands.argument("y", StringArgumentType.word())
								.then(ClientCommands.argument("z", StringArgumentType.word())
									.executes(WaylandCraftCommand::moveWindow)
								)
							)
						)
					)
				)
				.then(ClientCommands.literal("rotate")
					.then(ClientCommands.argument("handle", StringArgumentType.word())
						.then(ClientCommands.argument("angle", StringArgumentType.word())
							.executes(WaylandCraftCommand::rotateWindow)
						)
					)
				)
				.then(ClientCommands.literal("template")
					.then(ClientCommands.literal("save")
						.then(ClientCommands.argument("name", StringArgumentType.word())
							.executes(WaylandCraftCommand::templateSave)
						)
					)
					.then(ClientCommands.literal("savep")
						.then(ClientCommands.argument("name", StringArgumentType.word())
							.executes(WaylandCraftCommand::templateSavePermanent)
						)
					)
					.then(ClientCommands.literal("apply")
						.then(ClientCommands.argument("name", StringArgumentType.word())
							.executes(WaylandCraftCommand::templateApply)
						)
					)
					.then(ClientCommands.literal("applyp")
						.then(ClientCommands.argument("name", StringArgumentType.word())
							.executes(WaylandCraftCommand::templateApplyPermanent)
						)
					)
					.then(ClientCommands.literal("list")
						.executes(WaylandCraftCommand::templateList)
					)
					.then(ClientCommands.literal("remove")
						.then(ClientCommands.argument("name", StringArgumentType.word())
							.executes(WaylandCraftCommand::templateRemove)
						)
					)
					.then(ClientCommands.literal("removep")
						.then(ClientCommands.argument("name", StringArgumentType.word())
							.executes(WaylandCraftCommand::templateRemovePermanent)
						)
					)
				)
				.then(ClientCommands.literal("layout")
					.then(ClientCommands.literal("init")
						.executes(WaylandCraftCommand::layoutInit)
						.then(ClientCommands.argument("x", DoubleArgumentType.doubleArg())
							.then(ClientCommands.argument("y", DoubleArgumentType.doubleArg())
								.then(ClientCommands.argument("z", DoubleArgumentType.doubleArg())
									.executes(WaylandCraftCommand::layoutInit)
									.then(ClientCommands.argument("yaw", DoubleArgumentType.doubleArg())
										.executes(WaylandCraftCommand::layoutInit)
									)
								)
							)
						)
					)
					.then(ClientCommands.literal("cube")
						.executes(WaylandCraftCommand::layoutCube)
					)
					.then(ClientCommands.literal("sphere")
						.executes(WaylandCraftCommand::layoutSphere)
					)
					.then(ClientCommands.literal("on")
						.executes(WaylandCraftCommand::layoutOn)
					)
					.then(ClientCommands.literal("off")
						.executes(WaylandCraftCommand::layoutOff)
					)
					.then(ClientCommands.literal("toggle")
						.executes(WaylandCraftCommand::layoutToggle)
					)
					.then(ClientCommands.literal("status")
						.executes(WaylandCraftCommand::layoutStatus)
					)
					.then(ClientCommands.literal("list")
						.executes(WaylandCraftCommand::layoutList)
					)
					.then(ClientCommands.literal("add")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.executes(WaylandCraftCommand::layoutAdd)
						)
					)
					.then(ClientCommands.literal("remove")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.executes(WaylandCraftCommand::layoutRemove)
						)
					)
					.then(ClientCommands.literal("core")
						.then(ClientCommands.argument("handle", StringArgumentType.word())
							.executes(WaylandCraftCommand::layoutCore)
						)
					)
				)
		);
	}

	// ===== 帮助 =====

	/**
	 * 命令帮助 — 每个命令的语义说明，保证无歧义
	 * /wl help
	 */
	private static int showHelp(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 命令帮助§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" §e/wl list windows§7  — 列出合成器窗口§r"));
		source.sendFeedback(Component.literal(" §e/wl list apps§7     — 列出可启动应用§r"));
		source.sendFeedback(Component.literal(" §e/wl list desktop§7  — 列出可捕获的桌面窗口§r"));
		source.sendFeedback(Component.literal(" §e/wl launch <app>§7  — 启动应用§r"));
		source.sendFeedback(Component.literal(" §e/wl give <handle>§7 — 把窗口变为物品放入背包§r"));
		source.sendFeedback(Component.literal(" §e/wl take <handle>§7 — 从背包收回窗口物品§r"));
		source.sendFeedback(Component.literal(" §e/wl capture§7      — 弹出Portal选择，捕获桌面窗口§r"));
		source.sendFeedback(Component.literal(" §e/wl grab <handle>§7 — 抓取窗口，移动鼠标在世界中拖动§r"));
		source.sendFeedback(Component.literal(" §e/wl show <handle|all>§7 — 在世界中显示窗口（all 一键全部）§r"));
		source.sendFeedback(Component.literal(" §e/wl hide <handle|all>§7 — 从世界中隐藏窗口显示（all 一键全部）§r"));
		source.sendFeedback(Component.literal(" §e/wl pin <handle>§7  — 钉住窗口（世界中保持显示，不受隐藏/最小化影响）§r"));
		source.sendFeedback(Component.literal(" §e/wl unpin <handle>§7— 解除钉住§r"));
		source.sendFeedback(Component.literal(" §e/wl close <handle>§7— 终止应用进程（关闭窗口）§r"));
		source.sendFeedback(Component.literal(" §e/wl resize <handle> <w> <h>§7 — 调整窗口分辨率§r"));
		source.sendFeedback(Component.literal(" §e/wl settings list|set <key> <value>§7 — 查看/修改设置§r"));
		source.sendFeedback(Component.literal(" §e/wl share start|stop|quality|preset|config|reset|info|resolution|stats <handle> [...]§7 — 共享管理（start/stop 支持 all 一键全部）§r"));
		source.sendFeedback(Component.literal(" §e/wl permission list|default|allow|deny|remove§7 — 共享权限管理§r"));
		source.sendFeedback(Component.literal(" §e/wl pos <handle>§7 — 查看窗口位置/朝向/缩放/分辨率§r"));
		source.sendFeedback(Component.literal(" §e/wl move <handle> <x> <y> <z>§7 — 设置窗口坐标（绝对如 §e100.5§7 或相对如 §e~0.5§7 / §e~§7）§r"));
		source.sendFeedback(Component.literal(" §e/wl rotate <handle> <angle>§7 — 设置窗口朝向角（度，绝对如 §e90§7 或相对如 §e~15§7；0=朝+Z, 90=朝+X）§r"));
		source.sendFeedback(Component.literal(" §e/wl template save|savep <name>§7 — 保存当前区块窗口布局（临时/永久）§r"));
		source.sendFeedback(Component.literal(" §e/wl template apply|applyp <name>§7 — 恢复/复现布局§r"));
		source.sendFeedback(Component.literal(" §e/wl template list|remove|removep§7 — 管理模板§r"));
		source.sendFeedback(Component.literal(" §e/wl layout init [<x> <y> <z> [<yaw>]]§7 — 初始化布局坐标+朝向（无参=玩家位置）§r"));
		source.sendFeedback(Component.literal(" §e/wl layout cube|sphere§7 — 切换方块/圆球模板并开启（默认关闭）§r"));
		source.sendFeedback(Component.literal(" §e/wl layout on|off|toggle|status§7 — 布局开关/状态§r"));
		source.sendFeedback(Component.literal(" §e/wl layout list|add <handle>|remove <handle>|core <handle>§7 — 查看/手动指定布局内窗口与核心窗口§r"));
		source.sendFeedback(Component.literal(" §7Ctrl+方向键: 布局开启时切换核心窗口（核心标记可移动到任意窗口）；未开启时手动平移面前窗口§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" §7<handle> 支持 0x短句柄 / 完整句柄 / 实例别名（4位随机，wl list windows 显示）/ 应用别名（如 firefox_esr）§r"));
		return 1;
	}

	// ===== Handle & Alias =====

	private static String shortHex(long handle) {
		return SHORT_PREFIX + Long.toHexString(handle & 0xFFFF);
	}

	/**
	 * 生成窗口别名：小写+下划线，去除空格和特殊字符
	 * "Firefox ESR" → "firefox_esr"
	 * "Google Chrome" → "google_chrome"
	 */
	private static String getWindowAlias(WLCToplevel toplevel) {
		String name = getWindowDisplayName(toplevel);
		return name.toLowerCase()
			.replaceAll("[^a-z0-9\\s]", "") // 移除特殊字符
			.trim()
			.replaceAll("\\s+", "_"); // 空格→下划线
	}

	private static long parseWindowHandle(String handleStr) {
		handleStr = handleStr.trim();
		try {
			if(handleStr.toLowerCase().startsWith("0x")) {
				return Long.parseLong(handleStr.substring(2), 16);
			}
			return Long.parseLong(handleStr);
		} catch(NumberFormatException e) {
			return -1;
		}
	}

	/**
	 * 查找窗口 - 支持 hex handle、别名、后缀匹配
	 * 别名支持序号：别名:N 表示第 N 个同别名窗口（1 起），
	 * 解决多个同名窗口（如多个 firefox）只能操作第一个的问题。
	 */
	private static WLCToplevel findToplevelByHandle(FabricClientCommandSource source, String handleStr) {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return null;
		}

		WLCToplevel[] toplevels = wlc.bridge.getToplevels();

		// 1. 尝试 hex handle 解析
		long handle = parseWindowHandle(handleStr);
		if(handle >= 0) {
			WLCToplevel t = wlc.bridge.getToplevel(handle);
			if(t != null) return t;
		}

		// 1.2 实例别名（4 位随机如 k7xq，兼容旧格式 w1/w2 …，由 /wl list windows 获得，会话内唯一）
		if(handleStr.matches("w\\d+") || handleStr.matches("[a-z0-9]{4}")) {
			Long h = wlc.windowAliases.resolve(handleStr);
			if(h != null) {
				WLCToplevel t = wlc.bridge.getToplevel(h);
				if(t != null) return t;
			}
		}

		// 1.5 别名+序号：alias:N（如 firefox:2）
		int colonIdx = handleStr.lastIndexOf(':');
		if(colonIdx > 0) {
			String numPart = handleStr.substring(colonIdx + 1);
			String aliasPart = handleStr.substring(0, colonIdx).toLowerCase().replaceAll("[^a-z0-9_]", "");
			try {
				int n = Integer.parseInt(numPart);
				if(n >= 1) {
					int count = 0;
					for(WLCToplevel t : toplevels) {
						if(getWindowAlias(t).equals(aliasPart)) {
							count++;
							if(count == n) return t;
						}
					}
					if(count > 0) {
						source.sendError(Component.literal("§c✘ Window alias §e" + aliasPart + "§c has only " + count + " match(es), requested #" + n + "§r"));
						return null;
					}
				}
			} catch(NumberFormatException ignored) {
				// 不是序号语法，继续走别名匹配
			}
		}

		// 2. 后缀匹配（支持短handle如 0xABCD）
		String hex = handleStr.toLowerCase().replace("0x", "");
		for(WLCToplevel t : toplevels) {
			String fullHex = Long.toHexString(t.getHandle());
			if(fullHex.endsWith(hex)) {
				return t;
			}
		}

		// 3. 别名匹配（精确）
		String aliasInput = handleStr.toLowerCase().replaceAll("[^a-z0-9_]", "");
		for(WLCToplevel t : toplevels) {
			String alias = getWindowAlias(t);
			if(alias.equals(aliasInput)) {
				return t;
			}
		}

		// 4. 别名模糊匹配（包含）
		for(WLCToplevel t : toplevels) {
			String alias = getWindowAlias(t);
			if(alias.contains(aliasInput) || aliasInput.contains(alias)) {
				return t;
			}
		}

		source.sendError(Component.literal("§c✘ Window not found: " + handleStr + "§r"));
		return null;
	}

	private static String getWindowDisplayName(WLCToplevel toplevel) {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.xdgManager == null) {
			return toplevel.title != null ? toplevel.title : "Unknown";
		}

		DesktopEntry entry = wlc.xdgManager.forAppId(toplevel.appID);
		if(entry != null && entry.name != null) {
			return entry.name;
		}

		return toplevel.title != null ? toplevel.title : "Unknown";
	}

	private static boolean isWindowShared(long handle) {
		return SharedWindowClientHandler.getRemoteWindow(handle) != null;
	}

	// ===== 窗口命令 =====

	private static int listWindows(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		WLCToplevel[] toplevels = wlc.bridge.getToplevels();
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 Windows §7(" + toplevels.length + " total)§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));

		if(toplevels.length == 0) {
			source.sendFeedback(Component.literal(" §7No windows detected§r"));
		} else {
			// 统计别名出现次数，给同名窗口加序号（firefox:1, firefox:2 …）
			java.util.Map<String, Integer> aliasCounts = new java.util.HashMap<>();
			for(WLCToplevel toplevel : toplevels) {
				aliasCounts.merge(getWindowAlias(toplevel), 1, Integer::sum);
			}
			java.util.Map<String, Integer> aliasSeen = new java.util.HashMap<>();

			for(WLCToplevel toplevel : toplevels) {
				String hex = shortHex(toplevel.getHandle());
				String instAlias = wlc.windowAliases.getOrCreate(toplevel.getHandle());
				String appAlias = getWindowAlias(toplevel);
				String displayName = getWindowDisplayName(toplevel);
				int w = toplevel.geometry.width();
				int h = toplevel.geometry.height();
				boolean shared = isWindowShared(toplevel.getHandle());

				int n = aliasSeen.merge(appAlias, 1, Integer::sum);
				String line = " §e" + hex + "§r §b" + instAlias + "§r §a[" + appAlias + "]§r";
				if(aliasCounts.getOrDefault(appAlias, 0) > 1) {
					line += " §7#" + n + "§r";
				}
				line += " §f" + displayName + "§r §7" + w + "x" + h + "§r";
				if(shared) line += " §a✔§r";

				source.sendFeedback(Component.literal(line));
			}
		}

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" §7Use §e/wl resize <handle> <w> <h>§7 to resize§r"));
		source.sendFeedback(Component.literal(" §7Use §e/wl show <handle>§7 to show in world, §e/wl share start <handle>§7 to share§r"));
		return toplevels.length;
	}

	private static int listApps(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null || wlc.xdgManager == null) {
			source.sendError(Component.literal("§c✘ Desktop entries not loaded§r"));
			return 0;
		}

		List<DesktopEntry> entries = wlc.xdgManager.entries();
		List<DesktopEntry> visible = new ArrayList<>();
		for(DesktopEntry e : entries) {
			if(e.visible && e.name != null) visible.add(e);
		}

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 Apps §7(" + visible.size() + " total)§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));

		WaylandCraftBridge bridge = wlc.bridge;
		int launchable = 0;
		for(DesktopEntry entry : visible) {
			String name = entry.name;
			String desc = entry.genericName != null ? entry.genericName : "";
			String alias = slugify(name != null ? name : entry.appId);
			String status = (bridge != null) ? bridge.checkApp(entry.appId) : "no-exec";
			boolean ok = "ok".equals(status);
			if(ok) launchable++;

			String line = ok ? " §a✔ §r" : " §c✘ §r";
			line += "§b" + name + "§r";
			if(!alias.isEmpty()) line += " §7[§e" + alias + "§7]§r";
			if(!desc.isEmpty()) line += " §7- §8" + desc + "§r";
			if(!ok) line += " §c(" + status + ")§r";
			source.sendFeedback(Component.literal(line));
		}

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" §7" + launchable + "/" + visible.size() + " 可启动 · Use §e/wl launch <name|alias>§7 to launch§r"));
		return 1;
	}

	/**
	 * 生成应用别名：小写、非字母数字转下划线、连续下划线合并、去首尾下划线。
	 * 例: "CCC HHH" -> "ccc_hhh", "Mozilla Firefox" -> "mozilla_firefox"
	 */
	static String slugify(String s) {
		if(s == null) return "";
		String slug = s.toLowerCase()
			.replaceAll("[^a-z0-9]+", "_")
			.replaceAll("_+", "_")
			.replaceAll("^_+|_+$", "");
		return slug;
	}

	/**
	 * 启动应用（纯启动语义：只负责从桌面条目启动，不给物品）
	 * /wl launch <app>
	 */
	private static int launchWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String appName = StringArgumentType.getString(context, "app_name").trim();
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		if(wlc.xdgManager == null) {
			source.sendError(Component.literal("§c✘ Desktop entries not loaded§r"));
			return 0;
		}

		List<DesktopEntry> entries = wlc.xdgManager.entries();
		String appSlug = slugify(appName);
		String appLower = appName.toLowerCase();

		// 第一阶段：精确匹配（name/appId 完全相等，或 slug 完全相等）。
		// 修复：之前把"slug 相等"和"slug 包含"混在一起，导致 visual_studio_code 同时匹配
		// "Visual Studio Code"（相等）和 "Visual Studio Code - URL Handler"（包含）→ 永远歧义。
		// 现在精确匹配优先：只要存在精确命中，就不再用模糊匹配。
		List<DesktopEntry> matches = new ArrayList<>();
		for(DesktopEntry entry : entries) {
			if(entry.name != null && entry.name.toLowerCase().equals(appLower)) {
				matches.add(entry);
			} else if(entry.appId.toLowerCase().equals(appLower)) {
				matches.add(entry);
			} else if(!appSlug.isEmpty()) {
				String entrySlug = slugify(entry.name != null ? entry.name : entry.appId);
				if(entrySlug.equals(appSlug)) {
					matches.add(entry);
				}
			}
		}

		// 第二阶段：无精确命中时，才做模糊匹配（name/genericName/appId/slug 包含）
		if(matches.isEmpty()) {
			for(DesktopEntry entry : entries) {
				if(entry.name != null && entry.name.toLowerCase().contains(appLower)) {
					matches.add(entry);
				} else if(entry.genericName != null && entry.genericName.toLowerCase().contains(appLower)) {
					matches.add(entry);
				} else if(entry.appId.toLowerCase().contains(appLower)) {
					matches.add(entry);
				} else if(!appSlug.isEmpty()) {
					String entrySlug = slugify(entry.name != null ? entry.name : entry.appId);
					if(entrySlug.contains(appSlug)) {
						matches.add(entry);
					}
				}
			}
		}

		if(matches.isEmpty()) {
			source.sendError(Component.literal("§c✘ No application found: " + appName + "§r"));
			return 0;
		}

		if(matches.size() > 1) {
			source.sendFeedback(Component.literal("§eMultiple matches（用精确别名指定）:§r"));
			for(DesktopEntry entry : matches) {
				String alias = slugify(entry.name != null ? entry.name : entry.appId);
				source.sendFeedback(Component.literal("  §b- " + (entry.name != null ? entry.name : entry.appId) + "§r §7[§e" + alias + "§7]§r"));
			}
			return 0;
		}

		DesktopEntry entry = matches.get(0);
		boolean launched = launchApp(wlc, entry);
		if(launched) {
			source.sendFeedback(Component.literal("§a✔ Launched: §f" + (entry.name != null ? entry.name : entry.appId) + "§r"));
			source.sendFeedback(Component.literal(" §7窗口出现后将自动获得对应物品，右键长按放置到世界中§r"));
		} else {
			source.sendError(Component.literal("§c✘ Failed to launch: " + entry.appId + "§r"));
		}
		return launched ? 1 : 0;
	}

	/**
	 * 启动应用 - 使用原生execApp（Rust层正确设置WAYLAND_DISPLAY）
	 */
	private static boolean launchApp(WaylandCraft wlc, DesktopEntry entry) {
		return wlc.bridge.execApp(entry.appId);
	}

	/**
	 * 把指定窗口变为物品放入背包（原 WM 屏 Give Item 按钮）
	 * /wl give <handle>
	 */
	private static int giveWindowItem(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.itemManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		wlc.itemManager.giveItem(toplevel);
		source.sendFeedback(Component.literal("§a✔ Gave window item: §f" + getWindowDisplayName(toplevel) + "§r"));
		return 1;
	}

	/**
	 * 抓取窗口（原 WM 屏 Grab 按钮）：进入抓取模式，移动鼠标即可在世界中拖动窗口
	 * /wl grab <handle>
	 */
	private static int grabWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null || wlc.pointerGrabs == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		WindowDisplay display = wlc.getOrCreateDisplay(toplevel);
		wlc.pointerGrabs.startExclusive(new WindowGrab(display, 0));

		// 若当前在窗口管理屏中，退出屏幕进入世界抓取模式
		Minecraft mc = Minecraft.getInstance();
		if(mc.screen instanceof WindowManagerScreen) {
			mc.screen.onClose();
		}

		source.sendFeedback(Component.literal("§a✔ Grabbed: §f" + getWindowDisplayName(toplevel) + "§r"));
		source.sendFeedback(Component.literal(" §7移动鼠标拖动窗口，滚轮调整距离§r"));
		return 1;
	}

	/**
	 * 在世界中显示窗口（若未显示则创建显示并锚定到玩家面前）
	 * /wl show <handle>
	 */
	private static int showWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		// 一键显示全部窗口：/wl show all（或 *）
		if(handleStr.equalsIgnoreCase("all") || handleStr.equals("*")) {
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc == null || wlc.bridge == null) {
				source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
				return 0;
			}
			WLCToplevel[] toplevels = wlc.bridge.getToplevels();
			if(toplevels == null || toplevels.length == 0) {
				source.sendError(Component.literal("§c✘ 没有可显示的窗口（未捕获任何 Wayland 窗口）§r"));
				return 0;
			}
			Minecraft mc = Minecraft.getInstance();
			Camera camera = mc.gameRenderer.getMainCamera();
			int shown = 0, already = 0;
			for(WLCToplevel toplevel : toplevels) {
				boolean existed = wlc.hasDisplayFor(toplevel);
				WindowDisplay display = wlc.getOrCreateDisplay(toplevel);
				if(!existed) {
					display.anchorToCamera(camera);
					display.clampVertical();
					shown++;
				} else {
					already++;
				}
			}
			source.sendFeedback(Component.literal("§a✔ 已显示 §f" + shown + "§a 个窗口" + (already > 0 ? "（§7" + already + " 个原本已显示§a）" : "") + "§r"));
			return shown > 0 ? 1 : 0;
		}

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		boolean alreadyShown = wlc.hasDisplayFor(toplevel);
		WindowDisplay display = wlc.getOrCreateDisplay(toplevel);

		// 新显示时锚定到玩家面前，避免出现在世界原点
		if(!alreadyShown) {
			Minecraft mc = Minecraft.getInstance();
			Camera camera = mc.gameRenderer.getMainCamera();
			display.anchorToCamera(camera);
			display.clampVertical();
		}

		source.sendFeedback(Component.literal("§a✔ Shown in world: §f" + getWindowDisplayName(toplevel) + "§r"));
		if(!alreadyShown) {
			source.sendFeedback(Component.literal(" §7使用 §e/wl grab <handle>§7 调整位置§r"));
		}
		return 1;
	}

	/**
	 * 从世界中隐藏窗口显示（窗口仍在合成器中，窗口管理屏仍可见）
	 * 若窗口已钉住，先解除钉住再隐藏
	 * /wl hide <handle>
	 */
	private static int hideWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		// 一键隐藏全部窗口：/wl hide all（或 *）
		if(handleStr.equalsIgnoreCase("all") || handleStr.equals("*")) {
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc == null) {
				source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
				return 0;
			}
			int hidden = wlc.displays.size();
			if(hidden == 0) {
				source.sendError(Component.literal("§c✘ 当前没有任何在世界中显示的窗口§r"));
				return 0;
			}
			// 批量隐藏时一并解除钉住
			wlc.pinnedToplevel = null;
			wlc.displays.clear();
			source.sendFeedback(Component.literal("§a✔ 已隐藏全部 §f" + hidden + "§a 个窗口§7（钉住已一并解除，窗口管理屏中仍可见）§r"));
			return 1;
		}

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		boolean wasPinned = wlc.pinnedToplevel == toplevel;
		if(wasPinned) {
			wlc.pinnedToplevel = null;
		}

		boolean removed = wlc.displays.removeIf((w) -> w.window == toplevel);
		if(removed) {
			String pinNote = wasPinned ? "（已自动解除钉住）" : "";
			source.sendFeedback(Component.literal("§a✔ Hidden: §f" + getWindowDisplayName(toplevel) + "§r" + pinNote + " (窗口管理屏中仍可见)"));
		} else {
			if(wasPinned) {
				source.sendFeedback(Component.literal("§7" + getWindowDisplayName(toplevel) + " §7已解除钉住，但原本未在世界中显示§r"));
			} else {
				source.sendFeedback(Component.literal("§7" + getWindowDisplayName(toplevel) + " §7当前未在世界中显示§r"));
			}
		}
		return 1;
	}

	/**
	 * 钉住窗口：在世界中显示并保持显示（不受 hide/minimize 影响）
	 * /wl pin <handle>
	 */
	private static int pinWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		boolean alreadyPinned = wlc.pinnedToplevel == toplevel;
		if(!alreadyPinned) {
			// 未显示则先显示并锚定到玩家面前
			boolean alreadyShown = wlc.hasDisplayFor(toplevel);
			WindowDisplay display = wlc.getOrCreateDisplay(toplevel);
			if(!alreadyShown) {
				Minecraft mc = Minecraft.getInstance();
				Camera camera = mc.gameRenderer.getMainCamera();
				display.anchorToCamera(camera);
			}
			wlc.pinnedToplevel = toplevel;
		}

		source.sendFeedback(Component.literal("§a✔ Pinned: §f" + getWindowDisplayName(toplevel) + "§r §7（世界中保持显示，不受隐藏/最小化影响）§r"));
		return 1;
	}

	/**
	 * 解除钉住窗口
	 * /wl unpin <handle>
	 */
	private static int unpinWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		if(wlc.pinnedToplevel == toplevel) {
			wlc.pinnedToplevel = null;
			source.sendFeedback(Component.literal("§a✔ Unpinned: §f" + getWindowDisplayName(toplevel) + "§r"));
		} else {
			source.sendFeedback(Component.literal("§7" + getWindowDisplayName(toplevel) + " §7当前未处于钉住状态§r"));
		}
		return 1;
	}

	/**
	 * 列出全部设置（替代设置屏）
	 * /wl settings list
	 */
	private static int listSettings(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null || wlc.settingsManager == null) {
			source.sendError(Component.literal("§c✘ Settings not initialized§r"));
			return 0;
		}

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 Settings§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" §epixelsPerBlock§7 = §e" + wlc.settingsManager.getIntSetting(WaylandCraftSettings.PIXELS_PER_BLOCK) + "§r  §8(int, 窗口世界显示像素密度)§r"));
		source.sendFeedback(Component.literal(" §ewindowAntialiasing§7 = §e" + wlc.settingsManager.getBooleanSetting(WaylandCraftSettings.WINDOW_ANTIALIASING) + "§r  §8(bool)§r"));
		source.sendFeedback(Component.literal(" §efocusOnHover§7 = §e" + wlc.settingsManager.getBooleanSetting(WaylandCraftSettings.FOCUS_ON_HOVER) + "§r  §8(bool)§r"));
		source.sendFeedback(Component.literal(" §ehideCursor§7 = §e" + wlc.settingsManager.getBooleanSetting(WaylandCraftSettings.HIDE_CURSOR) + "§r  §8(bool, 控制窗口时隐藏虚拟鼠标，默认 H 键切换)§r"));
		source.sendFeedback(Component.literal(" §elayoutEnabled§7 = §e" + wlc.settingsManager.getBooleanSetting(WaylandCraftSettings.LAYOUT_ENABLED) + "§r  §8(bool, 自动布局开关，默认开启；未 init 时自动用玩家位置初始化)§r"));
		source.sendFeedback(Component.literal(" §elayoutTemplate§7 = §e" + wlc.settingsManager.getStringSetting(WaylandCraftSettings.LAYOUT_TEMPLATE) + "§r  §8(string, cube=方块 / sphere=圆球)§r"));
		source.sendFeedback(Component.literal(" §elayoutInitialized§7 = §e" + wlc.settingsManager.getBooleanSetting(WaylandCraftSettings.LAYOUT_INITIALIZED) + "§r  §8(bool, 已通过 /wl layout init 初始化)§r"));
		source.sendFeedback(Component.literal(" §elayoutInitX/Y/Z/Yaw§7 §8(double, 布局中心坐标与朝向)§r"));
		source.sendFeedback(Component.literal(" §elayoutAutoJoin§7 = §e" + wlc.settingsManager.getBooleanSetting(WaylandCraftSettings.LAYOUT_AUTO_JOIN) + "§r  §8(bool, 新窗口自动加入布局)§r"));
		source.sendFeedback(Component.literal(" §elayoutRadius§7 = §e" + wlc.settingsManager.getDoubleSetting(WaylandCraftSettings.LAYOUT_RADIUS) + "§r  §8(double, 布局半径格)§r"));
		source.sendFeedback(Component.literal(" §elayoutSpacing§7 = §e" + wlc.settingsManager.getDoubleSetting(WaylandCraftSettings.LAYOUT_SPACING) + "§r  §8(double, 同层窗口左右间距格)§r"));
		source.sendFeedback(Component.literal(" §elayoutStackSpacing§7 = §e" + wlc.settingsManager.getDoubleSetting(WaylandCraftSettings.LAYOUT_STACK_SPACING) + "§r  §8(double, 层间垂直间距格)§r"));
		source.sendFeedback(Component.literal(" §elayoutCubePerFace§7 = §e" + wlc.settingsManager.getIntSetting(WaylandCraftSettings.LAYOUT_CUBE_PER_FACE) + "§r  §8(int, 方块模板每面窗口数)§r"));
		source.sendFeedback(Component.literal(" §elayoutDefaultWidth/Height§7 = §e" + wlc.settingsManager.getIntSetting(WaylandCraftSettings.LAYOUT_DEFAULT_WIDTH) + "×" + wlc.settingsManager.getIntSetting(WaylandCraftSettings.LAYOUT_DEFAULT_HEIGHT) + "§r  §8(int, 新窗口自动分辨率)§r"));
		source.sendFeedback(Component.literal(" §egroundClearance§7 = §e" + wlc.settingsManager.getDoubleSetting(WaylandCraftSettings.GROUND_CLEARANCE) + "§r  §8(double, 窗口底部距地面最小净空格)§r"));
		source.sendFeedback(Component.literal(" §emoveStep§7 = §e" + wlc.settingsManager.getDoubleSetting(WaylandCraftSettings.MOVE_STEP) + "§r  §8(double, 手动 Ctrl+方向键平移步长格)§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" §7Use §e/wl settings set <key> <value>§7 to set§r"));
		return 1;
	}

	/**
	 * 设置单个设置项（替代设置屏）
	 * /wl settings set <key> <value>
	 */
	private static int setSetting(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String key = StringArgumentType.getString(context, "key");
		String value = StringArgumentType.getString(context, "value");

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.settingsManager == null) {
			source.sendError(Component.literal("§c✘ Settings not initialized§r"));
			return 0;
		}

		try {
			switch(key) {
				case "pixelsPerBlock" -> {
					wlc.settingsManager.setIntSetting(WaylandCraftSettings.PIXELS_PER_BLOCK, Integer.parseInt(value));
					source.sendFeedback(Component.literal("§a✔ §epixelsPerBlock§r = §e" + value + "§r"));
					return 1;
				}
				case "windowAntialiasing" -> {
					wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.WINDOW_ANTIALIASING, Boolean.parseBoolean(value));
					source.sendFeedback(Component.literal("§a✔ §ewindowAntialiasing§r = §e" + value + "§r"));
					return 1;
				}
				case "focusOnHover" -> {
					wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.FOCUS_ON_HOVER, Boolean.parseBoolean(value));
					source.sendFeedback(Component.literal("§a✔ §efocusOnHover§r = §e" + value + "§r"));
					return 1;
				}
				case "hideCursor" -> {
					wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.HIDE_CURSOR, Boolean.parseBoolean(value));
					source.sendFeedback(Component.literal("§a✔ §ehideCursor§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutEnabled" -> {
					boolean b = Boolean.parseBoolean(value);
					wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.LAYOUT_ENABLED, b);
					wlc.layoutManager.setEnabled(b);
					source.sendFeedback(Component.literal("§a✔ §elayoutEnabled§r = §e" + b + "§r"));
					return 1;
				}
				case "layoutTemplate" -> {
					String v = value.toLowerCase();
					if(!v.equals("cube") && !v.equals("sphere")) {
						source.sendError(Component.literal("§c✘ layoutTemplate 只能是 cube 或 sphere§r"));
						return 0;
					}
					wlc.settingsManager.setStringSetting(WaylandCraftSettings.LAYOUT_TEMPLATE, v);
					source.sendFeedback(Component.literal("§a✔ §elayoutTemplate§r = §e" + v + "§r"));
					return 1;
				}
				case "layoutInitialized" -> {
					wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.LAYOUT_INITIALIZED, Boolean.parseBoolean(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutInitialized§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutInitX" -> {
					wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_X, Double.parseDouble(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutInitX§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutInitY" -> {
					wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_Y, Double.parseDouble(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutInitY§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutInitZ" -> {
					wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_Z, Double.parseDouble(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutInitZ§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutInitYaw" -> {
					wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_YAW, Double.parseDouble(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutInitYaw§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutAutoJoin" -> {
					wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.LAYOUT_AUTO_JOIN, Boolean.parseBoolean(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutAutoJoin§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutRadius" -> {
					wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_RADIUS, Double.parseDouble(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutRadius§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutSpacing" -> {
					wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_SPACING, Double.parseDouble(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutSpacing§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutStackSpacing" -> {
					wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_STACK_SPACING, Double.parseDouble(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutStackSpacing§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutCubePerFace" -> {
					wlc.settingsManager.setIntSetting(WaylandCraftSettings.LAYOUT_CUBE_PER_FACE, Integer.parseInt(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutCubePerFace§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutDefaultWidth" -> {
					wlc.settingsManager.setIntSetting(WaylandCraftSettings.LAYOUT_DEFAULT_WIDTH, Integer.parseInt(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutDefaultWidth§r = §e" + value + "§r"));
					return 1;
				}
				case "layoutDefaultHeight" -> {
					wlc.settingsManager.setIntSetting(WaylandCraftSettings.LAYOUT_DEFAULT_HEIGHT, Integer.parseInt(value));
					source.sendFeedback(Component.literal("§a✔ §elayoutDefaultHeight§r = §e" + value + "§r"));
					return 1;
				}
				case "groundClearance" -> {
					wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.GROUND_CLEARANCE, Double.parseDouble(value));
					source.sendFeedback(Component.literal("§a✔ §egroundClearance§r = §e" + value + "§r"));
					return 1;
				}
				case "moveStep" -> {
					wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.MOVE_STEP, Double.parseDouble(value));
					source.sendFeedback(Component.literal("§a✔ §emoveStep§r = §e" + value + "§r"));
					return 1;
				}
				default -> {
					source.sendError(Component.literal("§c✘ Unknown setting: §f" + key + "§r"));
					source.sendFeedback(Component.literal(" §7Available: pixelsPerBlock, windowAntialiasing, focusOnHover, hideCursor, layoutEnabled, layoutAutoJoin, layoutRadius, layoutSpacing, layoutStackSpacing, moveStep§r"));
					return 0;
				}
			}
		} catch(NumberFormatException e) {
			source.sendError(Component.literal("§c✘ Invalid value: §f" + value + "§r"));
			return 0;
		}
	}

	// ===== 桌面窗口捕获 =====

	/**
	 * 列出可捕获的桌面窗口（通过 JNA X11）
	 */
	private static int listDesktopWindows(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;

		try {
			// 通过本地库获取桌面窗口（自动检测 wlr/GNOME//proc）
			List<X11WindowLister.WindowInfo> windowInfos = X11WindowLister.getDesktopWindows();

			source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
			source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 Desktop Windows §7(" + windowInfos.size() + " total)§r"));
			source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));

			if (windowInfos.isEmpty()) {
				source.sendFeedback(Component.literal(" §7No desktop windows detected§r"));
			} else {
			for (X11WindowLister.WindowInfo info : windowInfos) {
				// appId 可能为 null（X11 窗口没有 WM_CLASS 时），判空后再比较
				String desc = info.appId != null && !info.appId.isEmpty() && !info.appId.equals(info.title) ? " §7- §8" + info.appId + "§r" : "";
				source.sendFeedback(Component.literal(" §a[" + info.hash + "]§r §b" + info.title + "§r" + desc + " §7pid:" + info.pid + "§r"));
			}
			}

			source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
			source.sendFeedback(Component.literal(" §7Use §e/wl capture§7 to start capture§r"));
			return windowInfos.size();
		} catch (Exception e) {
			source.sendError(Component.literal("§c✘ Failed to list desktop windows: " + e.getMessage() + "§r"));
			return 0;
		}
	}

	/**
	 * 捕获桌面窗口（通过 XDG Desktop Portal ScreenCast）
	 * 会弹出窗口选择对话框，用户选择后自动开始捕获
	 */
	private static int captureWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;

		if (wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		source.sendFeedback(Component.literal("§e⏳ 正在启动 Portal 捕获...请在弹窗中选择要共享的窗口§r"));

		try {
			// 启动 Portal ScreenCast 捕获（会弹出确认对话框）
			PipeWireCaptureManager.CaptureSession session = wlc.captureManager.startCapture();

			if (session == null) {
				source.sendError(Component.literal("§c✘ Portal 捕获失败（可能被取消或超时）§r"));
				return 0;
			}

			// 注册虚拟 Toplevel 用于渲染
			session.registerToplevel("Portal Capture");

			source.sendFeedback(Component.literal("§a✔ Portal 捕获已启动§r"));
			source.sendFeedback(Component.literal(" §7窗口将在游戏世界中显示§r"));
			return 1;

		} catch (Exception e) {
			source.sendError(Component.literal("§c✘ 捕获失败: " + e.getMessage() + "§r"));
			return 0;
		}
	}

	/**
	 * 从背包收回窗口物品（give 的逆操作）
	 * /wl take <handle>
	 */
	private static int takeWindowItem(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		long handle = parseWindowHandle(handleStr);

		Minecraft mc = Minecraft.getInstance();
		if(mc.player == null) {
			source.sendError(Component.literal("§c✘ No player available§r"));
			return 0;
		}

		var inventory = mc.player.getInventory();
		boolean found = false;
		for(int i = 0; i < inventory.getContainerSize(); i++) {
			var stack = inventory.getItem(i);
			Long itemHandle = stack.get(dev.evvie.waylandcraft.item.WindowItem.WINDOW_HANDLE);
			if(itemHandle != null) {
				if(itemHandle == handle || Long.toHexString(itemHandle).endsWith(handleStr.toLowerCase().replace("0x", ""))) {
					inventory.removeItem(i, 1);
					found = true;
					break;
				}
			}
		}

		if(found) {
			source.sendFeedback(Component.literal("§a✔ Took back window item §e" + handleStr + "§r"));
		} else {
			source.sendError(Component.literal("§c✘ No window item in inventory: " + handleStr + "§r"));
		}
		return found ? 1 : 0;
	}

	private static int closeWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		String displayName = getWindowDisplayName(toplevel);
		if(toplevel.appID != null && !toplevel.appID.isEmpty()) {
			try {
				ProcessBuilder pb = new ProcessBuilder("pkill", "-f", toplevel.appID);
				pb.start();
				source.sendFeedback(Component.literal("§a✔ Closed: §f" + displayName + "§r"));
				return 1;
			} catch(Exception e) {
				source.sendError(Component.literal("§c✘ Failed: " + e.getMessage() + "§r"));
				return 0;
			}
		}
		source.sendError(Component.literal("§c✘ No app ID available§r"));
		return 0;
	}

	private static int resizeWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");
		int width = IntegerArgumentType.getInteger(context, "width");
		int height = IntegerArgumentType.getInteger(context, "height");

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		wlc.bridge.resizeToplevelInteractive(toplevel, width, height);
		// 分辨率变化后立即重新执行垂直钳制（底部不低于地面 0.4 格），
		// 不等下一帧 updateWorld 的 clampIfResized。
		WindowDisplay display = wlc.getDisplay(toplevel);
		if(display != null) {
			display.updateGeometry();
			display.clampVertical();
		}
		String alias = getWindowAlias(toplevel);
		source.sendFeedback(Component.literal("§a✔ Resized §f" + alias + "§r → §e" + width + "x" + height + "§r"));
		return 1;
	}

	// ===== 位置 & 模板 =====

	/**
	 * 查看窗口的世界坐标/朝向/缩放/分辨率
	 * /wl pos <handle>
	 */
	private static int posWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String instAlias = wlc.windowAliases.getOrCreate(toplevel.getHandle());
		WindowDisplay display = wlc.getDisplay(toplevel);

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" §b" + instAlias + "§r §f" + getWindowDisplayName(toplevel) + "§r §7(" + shortHex(toplevel.getHandle()) + ")§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));

		if(display == null) {
			source.sendFeedback(Component.literal(" §7尚未在世界中显示（/wl show " + instAlias + " 或 /wl grab " + instAlias + "）§r"));
			return 1;
		}

		net.minecraft.world.phys.Vec3 pivot = display.pivot;
		net.minecraft.world.phys.Vec3 normal = display.normal();
		source.sendFeedback(Component.literal(" §ex  §7" + fmt(pivot.x) + "   §ey  §7" + fmt(pivot.y) + "   §ez  §7" + fmt(pivot.z) + "§r"));
		source.sendFeedback(Component.literal(" §e朝向 §7(" + fmt(normal.x) + ", " + fmt(normal.y) + ", " + fmt(normal.z) + ")  §e角度 §7" + fmt(display.yawDegrees()) + "°§r"));
		source.sendFeedback(Component.literal(" §e分辨率 §7" + toplevel.geometry.width() + "x" + toplevel.geometry.height() + "§r  §e缩放 §7" + fmt(display.viewScale) + "§r"));
		return 1;
	}

	private static String fmt(double v) {
		return String.format(java.util.Locale.ROOT, "%.2f", v);
	}

	/**
	 * 设置窗口世界坐标（pivot，即窗口中心）
	 * /wl move <handle> <x> <y> <z>
	 * 每个轴支持两种写法：
	 *   - 绝对坐标：100.5  →  直接设为该值
	 *   - 相对偏移：~0.5 / ~-1 / ~  →  在当前值上增减（~ 表示 +0）
	 */
	private static int moveWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");
		String xs = StringArgumentType.getString(context, "x");
		String ys = StringArgumentType.getString(context, "y");
		String zs = StringArgumentType.getString(context, "z");

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		WindowDisplay display = wlc.getDisplay(toplevel);
		if(display == null) {
			// 窗口尚未在世界中显示：自动显示并放置
			display = wlc.getOrCreateDisplay(toplevel);
		}

		try {
			double curX = display.pivot.x;
			double curY = display.pivot.y;
			double curZ = display.pivot.z;

			boolean[] rel = new boolean[1];
			double vx = parseAxisValue(xs, curX, rel);
			double x = rel[0] ? curX + vx : vx;
			rel[0] = false;
			double vy = parseAxisValue(ys, curY, rel);
			double y = rel[0] ? curY + vy : vy;
			rel[0] = false;
			double vz = parseAxisValue(zs, curZ, rel);
			double z = rel[0] ? curZ + vz : vz;

			display.pivot = new net.minecraft.world.phys.Vec3(x, y, z);
		} catch(NumberFormatException e) {
			source.sendError(Component.literal("§c✘ 无效坐标§r §7(支持绝对坐标如 §e100.5§7，或相对偏移如 §e~0.5§7 / §e~-1§7 / §e~§7)§r"));
			return 0;
		}

		String alias = getWindowAlias(toplevel);
		source.sendFeedback(Component.literal("§a✔ Moved §f" + alias + "§r → §ex " + fmt(display.pivot.x) + "§7, §ey " + fmt(display.pivot.y) + "§7, §ez " + fmt(display.pivot.z) + "§r"));
		return 1;
	}

	/**
	 * 解析单个坐标轴：绝对数字或 ~ 相对偏移
	 */
	private static double parseAxisValue(String s, double current, boolean[] isRelative) throws NumberFormatException {
		if(s.startsWith("~")) {
			isRelative[0] = true;
			String numPart = s.substring(1).trim();
			if(numPart.isEmpty()) return 0;
			return Double.parseDouble(numPart);
		}
		return Double.parseDouble(s);
	}

	/**
	 * 设置窗口朝向角（度，绕世界 Y 轴）
	 * /wl rotate <handle> <angle>
	 * angle 支持绝对（90 = 朝 +X）或相对偏移（~15 = 当前 +15°）。
	 */
	private static int rotateWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");
		String angleStr = StringArgumentType.getString(context, "angle");

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		WindowDisplay display = wlc.getDisplay(toplevel);
		if(display == null) {
			display = wlc.getOrCreateDisplay(toplevel);
		}

		try {
			boolean[] rel = new boolean[1];
			double delta = parseAxisValue(angleStr, 0, rel);
			double yaw = rel[0] ? display.yawDegrees() + delta : delta;
			display.rotateToYawDegrees(yaw);
		} catch(NumberFormatException e) {
			source.sendError(Component.literal("§c✘ 无效角度§r §7(支持绝对如 §e90§7，或相对偏移如 §e~15§7 / §e~§7)§r"));
			return 0;
		}

		String alias = getWindowAlias(toplevel);
		source.sendFeedback(Component.literal("§a✔ Rotated §f" + alias + "§r → §e" + fmt(display.yawDegrees()) + "°§r §7(0=朝+Z, 90=朝+X)§r"));
		return 1;
	}

	/**
	 * 保存临时模板：记录玩家所在区块（16x16）内所有已显示窗口的位置
	 * /wl template save <name>   （内存，重启失效）
	 */
	private static int templateSave(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String name = StringArgumentType.getString(context, "name");
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}
		if(Minecraft.getInstance().player == null) {
			source.sendError(Component.literal("§c✘ 需要进入世界§r"));
			return 0;
		}

		WindowTemplateManager.WindowTemplate tpl = wlc.templateManager.saveTemporary(name, wlc);
		if(tpl == null) return 0;
		source.sendFeedback(Component.literal("§a✔ 临时模板已保存: §b" + name + "§r §7(" + tpl.entries.size() + " 个窗口)§r"));
		source.sendFeedback(Component.literal(" §7使用 §e/wl template apply " + name + "§7 恢复位置（重启后失效）§r"));
		return 1;
	}

	/**
	 * 保存永久模板：记录 appId + 位置 + 分辨率，写入磁盘
	 * /wl template savep <name>
	 */
	private static int templateSavePermanent(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String name = StringArgumentType.getString(context, "name");
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}
		if(Minecraft.getInstance().player == null) {
			source.sendError(Component.literal("§c✘ 需要进入世界§r"));
			return 0;
		}

		WindowTemplateManager.WindowTemplate tpl = wlc.templateManager.savePermanent(name, wlc);
		if(tpl == null) return 0;
		source.sendFeedback(Component.literal("§a✔ 永久模板已保存: §b" + name + "§r §7(" + tpl.entries.size() + " 个窗口)§r"));
		source.sendFeedback(Component.literal(" §7使用 §e/wl template applyp " + name + "§7 启动应用并复现布局（重启后仍可用）§r"));
		return 1;
	}

	/**
	 * 应用临时模板
	 * /wl template apply <name>
	 */
	private static int templateApply(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String name = StringArgumentType.getString(context, "name");
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		if(wlc.templateManager.applyTemporary(name, wlc)) {
			source.sendFeedback(Component.literal("§a✔ 临时模板已应用: §b" + name + "§r"));
			return 1;
		}
		source.sendError(Component.literal("§c✘ 临时模板不存在或窗口已失效: " + name + "§r"));
		return 0;
	}

	/**
	 * 应用永久模板：窗口已开直接放置，未开则启动应用并等待出现
	 * /wl template applyp <name>
	 */
	private static int templateApplyPermanent(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String name = StringArgumentType.getString(context, "name");
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null || wlc.bridge == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		if(wlc.templateManager.applyPermanent(name, wlc)) {
			source.sendFeedback(Component.literal("§a✔ 永久模板已应用: §b" + name + "§r"));
			if(wlc.templateManager.hasPending()) {
				source.sendFeedback(Component.literal(" §7正在启动应用，窗口出现后将自动放置…§r"));
			}
			return 1;
		}
		source.sendError(Component.literal("§c✘ 永久模板不存在: " + name + "§r"));
		return 0;
	}

	/**
	 * 列出所有模板
	 * /wl template list
	 */
	private static int templateList(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 Templates§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));

		List<WindowTemplateManager.WindowTemplate> temps = wlc.templateManager.listTemporary();
		if(temps.isEmpty()) {
			source.sendFeedback(Component.literal(" §7临时模板: 无§r"));
		} else {
			source.sendFeedback(Component.literal(" §7临时模板（重启失效）:§r"));
			for(WindowTemplateManager.WindowTemplate t : temps) {
				source.sendFeedback(Component.literal("  §b" + t.name + "§r §7(" + t.entries.size() + " 窗口, 可用 §eapply " + t.name + "§7)§r"));
			}
		}

		List<WindowTemplateManager.WindowTemplate> perms = wlc.templateManager.listPermanent();
		if(perms.isEmpty()) {
			source.sendFeedback(Component.literal(" §7永久模板: 无§r"));
		} else {
			source.sendFeedback(Component.literal(" §7永久模板（重启可用）:§r"));
			for(WindowTemplateManager.WindowTemplate t : perms) {
				source.sendFeedback(Component.literal("  §b" + t.name + "§r §7(" + t.entries.size() + " 窗口, 可用 §eapplyp " + t.name + "§7)§r"));
			}
		}
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		return temps.size() + perms.size();
	}

	/**
	 * 删除临时模板
	 * /wl template remove <name>
	 */
	private static int templateRemove(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String name = StringArgumentType.getString(context, "name");
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		if(wlc.templateManager.removeTemporary(name)) {
			source.sendFeedback(Component.literal("§a✔ 临时模板已删除: §b" + name + "§r"));
			return 1;
		}
		source.sendError(Component.literal("§c✘ 临时模板不存在: " + name + "§r"));
		return 0;
	}

	/**
	 * 删除永久模板
	 * /wl template removep <name>
	 */
	private static int templateRemovePermanent(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String name = StringArgumentType.getString(context, "name");
		WaylandCraft wlc = WaylandCraft.instance;

		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		if(wlc.templateManager.removePermanent(name)) {
			source.sendFeedback(Component.literal("§a✔ 永久模板已删除: §b" + name + "§r"));
			return 1;
		}
		source.sendError(Component.literal("§c✘ 永久模板不存在: " + name + "§r"));
		return 0;
	}

	// ===== 窗口自动布局命令（方块/圆球模板，围绕初始化坐标） =====

	/**
	 * /wl layout init [<x> <y> <z> [<yaw>]] — 初始化布局坐标与朝向
	 * 不带参数 = 用玩家当前位置与朝向。yaw 单位度（0=朝+Z，顺时针）。
	 */
	private static int layoutInit(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.settingsManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		double x, y, z, yaw;
		boolean hasArgs = false;
		try {
			x = DoubleArgumentType.getDouble(context, "x");
			y = DoubleArgumentType.getDouble(context, "y");
			z = DoubleArgumentType.getDouble(context, "z");
			hasArgs = true;
			try {
				yaw = DoubleArgumentType.getDouble(context, "yaw");
			} catch(IllegalArgumentException e) {
				yaw = 0.0;
			}
		} catch(IllegalArgumentException e) {
			x = 0;
			y = 0;
			z = 0;
			yaw = 0;
		}
		if(!hasArgs) {
			// 无参数：用玩家位置 + 朝向
			var player = source.getPlayer();
			if(player == null) {
				source.sendError(Component.literal("§c✘ 未提供坐标且找不到玩家§r"));
				return 0;
			}
			var pos = player.position();
			x = pos.x;
			// 存眼睛高度（脚部 + 1.62）：窗口第一层中心对齐眼睛高度，站在中心平视正对不斜
			y = pos.y + 1.62;
			z = pos.z;
			float yawDeg = player.getYRot();
			// MC yaw: 0=朝+Z? MC 的 yaw 0 朝 +Z 南，90 朝 -X 西（逆时针）。
			// 我们的布局约定 0=朝+Z、90=朝+X（顺时针）→ yaw = -yawDeg
			yaw = -yawDeg;
		}

		wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_X, x);
		wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_Y, y);
		wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_Z, z);
		wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_YAW, yaw);
		wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.LAYOUT_INITIALIZED, true);

		source.sendFeedback(Component.literal("§a✔ 布局坐标已初始化§r"));
		source.sendFeedback(Component.literal(" §7中心: §e" + trim(x) + " " + trim(y) + " " + trim(z) + "§r §7朝向: §e" + trim(yaw) + "°§r"));
		source.sendFeedback(Component.literal(" §7窗口将围绕该坐标排布（方块/圆球），不再跟随玩家§r"));
		return 1;
	}

	private static String trim(double v) {
		return String.format("%.2f", v);
	}

	private static int layoutCube(CommandContext<FabricClientCommandSource> context) {
		return setLayoutTemplate(context, "cube");
	}

	private static int layoutSphere(CommandContext<FabricClientCommandSource> context) {
		return setLayoutTemplate(context, "sphere");
	}

	private static int setLayoutTemplate(CommandContext<FabricClientCommandSource> context, String template) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.settingsManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		wlc.settingsManager.setStringSetting(WaylandCraftSettings.LAYOUT_TEMPLATE, template);
		if(!wlc.layoutManager.isEnabled()) {
			wlc.layoutManager.setEnabled(true);
			wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.LAYOUT_ENABLED, true);
		}
		if(!wlc.layoutManager.isInitialized()) {
			source.sendFeedback(Component.literal("§a✔ 已切换为 §e" + template + "§a 模板并开启布局§r"));
			source.sendFeedback(Component.literal(" §7但尚未初始化坐标，请先 §e/wl layout init§7（或用 /wl layout init 默认当前玩家位置）§r"));
		} else {
			source.sendFeedback(Component.literal("§a✔ 已切换为 §e" + template + "§a 模板并开启布局§r"));
		}
		return 1;
	}

	private static int layoutOn(CommandContext<FabricClientCommandSource> context) {
		return setLayoutEnabled(context, true);
	}

	private static int layoutOff(CommandContext<FabricClientCommandSource> context) {
		return setLayoutEnabled(context, false);
	}

	private static int layoutToggle(CommandContext<FabricClientCommandSource> context) {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.settingsManager == null) {
			context.getSource().sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}
		return setLayoutEnabled(context, !wlc.layoutManager.isEnabled());
	}

	private static int setLayoutEnabled(CommandContext<FabricClientCommandSource> context, boolean enabled) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.settingsManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		if(enabled && !wlc.layoutManager.isInitialized()) {
			source.sendError(Component.literal("§c✘ 布局未初始化，请先 §e/wl layout init§r"));
			return 0;
		}

		wlc.layoutManager.setEnabled(enabled);
		wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.LAYOUT_ENABLED, enabled);
		if(enabled) {
			source.sendFeedback(Component.literal("§a✔ 自动布局已开启§r"));
			source.sendFeedback(Component.literal(" §7模板: §e" + wlc.settings.getLayoutTemplate() + "§r §7· 半径: §e" + wlc.settings.getLayoutRadius() + "§7 格§r"));
		} else {
			source.sendFeedback(Component.literal("§c✔ 自动布局已关闭§r"));
		}
		return 1;
	}

	private static int layoutStatus(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 自动布局状态§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" 布局: §e" + (wlc.layoutManager.isEnabled() ? "§a开启" : "§c关闭") + "§r"));
		source.sendFeedback(Component.literal(" 模板: §e" + wlc.settings.getLayoutTemplate() + "§r  §8(cube=方块 / sphere=圆球)§r"));
		boolean inited = wlc.layoutManager.isInitialized();
		source.sendFeedback(Component.literal(" 初始化: §e" + (inited ? "§a是" : "§c否") + "§r"));
		if(inited) {
			source.sendFeedback(Component.literal(" 中心: §e" + trim(wlc.settings.getLayoutInitX()) + " " + trim(wlc.settings.getLayoutInitY()) + " " + trim(wlc.settings.getLayoutInitZ()) + "§r §7朝向 §e" + trim(wlc.settings.getLayoutInitYaw()) + "°§r"));
		}
		source.sendFeedback(Component.literal(" 自动加入: §e" + wlc.settings.getLayoutAutoJoin() + "§r  §8(新窗口自动进入布局)§r"));
		source.sendFeedback(Component.literal(" 半径: §e" + wlc.settings.getLayoutRadius() + "§7 格 · 间距: §e" + wlc.settings.getLayoutSpacing() + "§7 格 · 层距: §e" + wlc.settings.getLayoutStackSpacing() + "§7 格§r"));
		if("cube".equals(wlc.settings.getLayoutTemplate())) {
			source.sendFeedback(Component.literal(" 每面窗口: §e" + wlc.settings.getLayoutCubePerFace() + "§r  §8(4 面共 " + (wlc.settings.getLayoutCubePerFace() * 4) + " 个/层)§r"));
		}
		source.sendFeedback(Component.literal(" 默认分辨率: §e" + wlc.settings.getLayoutDefaultWidth() + "×" + wlc.settings.getLayoutDefaultHeight() + "§r"));
		source.sendFeedback(Component.literal(" 参与窗口: §e" + wlc.layoutManager.participatingDisplays().size() + "§r"));
		WindowDisplay core = wlc.findCoreDisplay();
		String coreName = "无";
		if(core != null && core.window instanceof WLCToplevel coreTl) {
			coreName = getWindowDisplayName(coreTl);
		}
		source.sendFeedback(Component.literal(" 核心窗口: §e" + coreName + "§r  §8(Ctrl+方向键: 切换核心窗口，任意窗口都可成为核心)§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		return 1;
	}

	private static int layoutList(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 布局内窗口§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		int count = 0;
		for(WindowDisplay d : wlc.layoutManager.participatingDisplays()) {
			if(!(d.window instanceof WLCToplevel t)) continue;
			count++;
			boolean core = t.getHandle() == wlc.layoutManager.getCoreHandle();
			source.sendFeedback(Component.literal((core ? " §a➤ " : " §7- ") + "§f" + getWindowDisplayName(t) + "§r §e" + shortHex(t.getHandle()) + "§r" + (core ? " §8[核心]§r" : "")));
		}
		if(count == 0) {
			source.sendFeedback(Component.literal(" §7无参与窗口§r"));
		}
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		return count;
	}

	private static int layoutAdd(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		wlc.layoutManager.addHandle(toplevel.getHandle());
		source.sendFeedback(Component.literal("§a✔ §f" + getWindowDisplayName(toplevel) + "§a 已加入自动布局§r"));
		return 1;
	}

	private static int layoutRemove(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		wlc.layoutManager.removeHandle(toplevel.getHandle());
		source.sendFeedback(Component.literal("§a✔ §f" + getWindowDisplayName(toplevel) + "§a 已移出自动布局§r"));
		return 1;
	}

	private static int layoutCore(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		wlc.layoutManager.setCoreHandle(toplevel.getHandle());
		source.sendFeedback(Component.literal("§a✔ 核心窗口已设为 §f" + getWindowDisplayName(toplevel) + "§r"));
		return 1;
	}

	private static int shareWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		// 一键共享全部窗口：/wl share start all（或 *）
		if(handleStr.equalsIgnoreCase("all") || handleStr.equals("*")) {
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc == null || wlc.bridge == null) {
				source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
				return 0;
			}
			WLCToplevel[] toplevels = wlc.bridge.getToplevels();
			if(toplevels == null || toplevels.length == 0) {
				source.sendError(Component.literal("§c✘ 没有可共享的窗口（未捕获任何 Wayland 窗口）§r"));
				return 0;
			}
			int shared = 0, skipped = 0;
			for(WLCToplevel toplevel : toplevels) {
				String displayName = getWindowDisplayName(toplevel);
				boolean ok;
				if(wlc.windowShareManager != null) {
					ok = wlc.windowShareManager.startSharing(toplevel.getHandle(), displayName);
				} else {
					SharedWindowClientHandler.requestWindowRegister(toplevel.getHandle(), displayName);
					ok = true;
				}
				if(ok) shared++; else skipped++;
			}
			source.sendFeedback(Component.literal("§a✔ 已共享 §f" + shared + "§a 个窗口" + (skipped > 0 ? "（§7跳过 " + skipped + " 个已共享/失败§a）" : "") + "§r"));
			return shared > 0 ? 1 : 0;
		}

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		String displayName = getWindowDisplayName(toplevel);
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc != null && wlc.windowShareManager != null) {
			wlc.windowShareManager.startSharing(toplevel.getHandle(), displayName);
		} else {
			SharedWindowClientHandler.requestWindowRegister(toplevel.getHandle(), displayName);
		}
		source.sendFeedback(Component.literal("§a✔ Shared: §f" + displayName + "§r §e" + shortHex(toplevel.getHandle()) + "§r"));
		return 1;
	}

	private static int unshareWindow(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String handleStr = StringArgumentType.getString(context, "handle");

		// 一键停止全部共享：/wl share stop all（或 *）
		if(handleStr.equalsIgnoreCase("all") || handleStr.equals("*")) {
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc == null || wlc.windowShareManager == null) {
				source.sendError(Component.literal("§c✘ WaylandCraft not initialized（服务器端不支持批量停止）§r"));
				return 0;
			}
			Map<Long, WindowShareManager.ShareState> states = wlc.windowShareManager.getAllShareStates();
			if(states.isEmpty()) {
				source.sendError(Component.literal("§c✘ 当前没有任何共享中的窗口§r"));
				return 0;
			}
			// getAllShareStates 返回不可变副本，遍历时安全移除
			for(long handle : states.keySet()) {
				wlc.windowShareManager.stopSharing(handle);
			}
			source.sendFeedback(Component.literal("§a✔ 已停止全部 §f" + states.size() + "§a 个窗口的共享§r"));
			return 1;
		}

		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc != null && wlc.windowShareManager != null) {
			wlc.windowShareManager.stopSharing(toplevel.getHandle());
		} else {
			SharedWindowClientHandler.requestWindowUnregister(toplevel.getHandle());
		}
		source.sendFeedback(Component.literal("§a✔ Unshared: §f" + getWindowDisplayName(toplevel) + "§r"));
		return 1;
	}

	private static int setShareQuality(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		long handle = toplevel.getHandle();
		float scale = FloatArgumentType.getFloat(context, "scale");
		float quality = FloatArgumentType.getFloat(context, "quality");
		int fps = IntegerArgumentType.getInteger(context, "fps");

		ImageCapture.CaptureConfig config = new ImageCapture.CaptureConfig(scale, quality, fps);
		wlc.windowShareManager.setPerWindowConfig(handle, config);

		source.sendFeedback(Component.literal("§a✔ Quality set for §f" + getWindowDisplayName(toplevel) + "§r"));
		source.sendFeedback(Component.literal(" §7Scale: §e" + scale + "§7 Quality: §e" + quality + "§7 FPS: §e" + (fps == 0 ? "unlimited" : String.valueOf(fps)) + "§r"));
		return 1;
	}

	private static int resetShareQuality(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		long handle = toplevel.getHandle();
		wlc.windowShareManager.clearPerWindowConfig(handle);

		source.sendFeedback(Component.literal("§a✔ Share config reset for §f" + getWindowDisplayName(toplevel) + "§r (using global config)"));
		return 1;
	}

	private static int setShareConfig(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		String param = StringArgumentType.getString(context, "param").toLowerCase();
		String value = StringArgumentType.getString(context, "value").toLowerCase();

		// 获取当前配置（如果没有则创建默认）
		WindowShareManager.ShareState state = wlc.windowShareManager.getShareState(toplevel.getHandle());
		ImageCapture.CaptureConfig config = state != null && state.perWindowConfig != null 
			? state.perWindowConfig 
			: ImageCapture.CaptureConfig.balanced();

		try {
			switch(param) {
				case "scale" -> config.scale = Float.parseFloat(value);
				case "quality" -> config.quality = Float.parseFloat(value);
				case "fps" -> {
					int fps = Integer.parseInt(value);
					if(fps < 0 || fps > 240) {
						source.sendError(Component.literal("§c✘ FPS must be 0 (unlimited) or 1-240§r"));
						return 0;
					}
					config.maxFps = fps;
				}
				case "diff" -> config.diffUpdate = Boolean.parseBoolean(value);
				case "bitrate" -> config.maxBitrate = Integer.parseInt(value);
				case "buffer" -> config.frameBuffer = Integer.parseInt(value);
				case "latency" -> config.latencyComp = Integer.parseInt(value);
				case "prediction" -> config.prediction = Boolean.parseBoolean(value);
				case "compression" -> config.compression = value;
				case "diffThreshold" -> config.diffThreshold = Float.parseFloat(value);
				default -> {
					source.sendError(Component.literal("§c✘ Unknown parameter: §f" + param + "§r"));
					source.sendFeedback(Component.literal(" §7Available: scale, quality, fps, diff, bitrate, buffer, latency, prediction, compression, diffThreshold§r"));
					return 0;
				}
			}
			wlc.windowShareManager.setPerWindowConfig(toplevel.getHandle(), config);
			source.sendFeedback(Component.literal("§a✔ §f" + param + "§r = §e" + value + "§r for §f" + getWindowDisplayName(toplevel) + "§r"));
		} catch(NumberFormatException e) {
			source.sendError(Component.literal("§c✘ Invalid value: §f" + value + "§r"));
			return 0;
		}
		return 1;
	}

	private static int applySharePreset(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		String preset = StringArgumentType.getString(context, "preset").toLowerCase();
		ImageCapture.CaptureConfig config;

		switch(preset) {
			case "performance" -> config = ImageCapture.CaptureConfig.highPerformance();
			case "quality" -> config = ImageCapture.CaptureConfig.highQuality();
			case "balanced" -> config = ImageCapture.CaptureConfig.balanced();
			case "lowlatency" -> config = ImageCapture.CaptureConfig.lowLatency();
			default -> {
				source.sendError(Component.literal("§c✘ Unknown preset: §f" + preset + "§r"));
				source.sendFeedback(Component.literal(" §7Available: performance, quality, balanced, lowlatency§r"));
				return 0;
			}
		}

		wlc.windowShareManager.setPerWindowConfig(toplevel.getHandle(), config);
		source.sendFeedback(Component.literal("§a✔ Applied preset §e" + preset + "§r to §f" + getWindowDisplayName(toplevel) + "§r"));
		source.sendFeedback(Component.literal(" §7" + config.getSummary() + "§r"));
		return 1;
	}

	private static int showShareConfig(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WindowShareManager.ShareState state = wlc.windowShareManager.getShareState(toplevel.getHandle());
		ImageCapture.CaptureConfig config = state != null && state.perWindowConfig != null 
			? state.perWindowConfig 
			: null;

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 Share Config: §f" + getWindowDisplayName(toplevel) + "§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));

		if(config != null) {
			source.sendFeedback(Component.literal(" §7Scale: §e" + config.scale + "§r"));
			source.sendFeedback(Component.literal(" §7Quality: §e" + config.quality + "§r"));
			source.sendFeedback(Component.literal(" §7FPS: §e" + (config.maxFps == 0 ? "unlimited" : String.valueOf(config.maxFps)) + "§r"));
			source.sendFeedback(Component.literal(" §7Diff Update: §e" + config.diffUpdate + "§r"));
			source.sendFeedback(Component.literal(" §7Bitrate: §e" + (config.maxBitrate > 0 ? config.maxBitrate + "kbps" : "unlimited") + "§r"));
			source.sendFeedback(Component.literal(" §7Buffer: §e" + config.frameBuffer + " frames§r"));
			source.sendFeedback(Component.literal(" §7Latency Comp: §e" + config.latencyComp + "ms§r"));
			source.sendFeedback(Component.literal(" §7Prediction: §e" + config.prediction + "§r"));
			source.sendFeedback(Component.literal(" §7Compression: §e" + config.compression + "§r"));
			source.sendFeedback(Component.literal(" §7Diff Threshold: §e" + String.format("%.3f", config.diffThreshold) + "§r"));
		} else {
			source.sendFeedback(Component.literal(" §7Using global config§r"));
		}

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" §7Presets: §eperformance§7, §equality§7, §ebalanced§7, §elowlatency§r"));
		source.sendFeedback(Component.literal(" §7Use §e/wl share config <handle> <param> <value>§7 to set§r"));
		source.sendFeedback(Component.literal(" §7Use §e/wl share reset <handle>§7 to restore global defaults§r"));
		return 1;
	}

	/**
	 * 设置共享窗口的捕获目标分辨率
	 * /wl share resolution <handle> <width> <height>
	 */
	private static int setShareResolution(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		int targetW = IntegerArgumentType.getInteger(context, "width");
		int targetH = IntegerArgumentType.getInteger(context, "height");

		int srcW = toplevel.geometry.width();
		int srcH = toplevel.geometry.height();
		if(srcW <= 0 || srcH <= 0) {
			source.sendError(Component.literal("§c✘ Window has no geometry§r"));
			return 0;
		}

		float scale = Math.min(1.0f, Math.min((float)targetW / srcW, (float)targetH / srcH));
		scale = Math.max(0.1f, scale);

		ImageCapture.CaptureConfig config = new ImageCapture.CaptureConfig(scale, 0.7f, 20);
		wlc.windowShareManager.setPerWindowConfig(toplevel.getHandle(), config);

		int actualW = (int)(srcW * scale);
		int actualH = (int)(srcH * scale);

		source.sendFeedback(Component.literal("§a✔ Resolution set to §e" + actualW + "x" + actualH + "§a (scale=" + String.format("%.2f", scale) + ")§r"));
		return 1;
	}

	/**
	 * 显示共享窗口统计信息
	 * /wl share stats <handle>
	 */
	private static int showShareStats(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		WLCToplevel toplevel = findToplevelByHandle(source, handleStr);
		if(toplevel == null) return 0;

		WindowShareManager.ShareState state = wlc.windowShareManager.getShareState(toplevel.getHandle());
		if(state == null) {
			source.sendError(Component.literal("§c✘ Window §e" + handleStr + "§c is not being shared§r"));
			return 0;
		}

		long uptime = (System.currentTimeMillis() - state.startTime) / 1000;
		float avgFps = uptime > 0 ? (float)state.frameCount / uptime : 0;
		String avgSize = state.frameCount > 0 ? (state.totalBytes / state.frameCount / 1024) + "KB" : "N/A";

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lWaylandCraft §r§7 Share Stats: §f" + getWindowDisplayName(toplevel) + "§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal(" §7Frames: §e" + state.frameCount + "§7 (skipped: §e" + state.skippedFrames + "§7, rate-limited: §e" + state.rateLimitedFrames + "§7)§r"));
		source.sendFeedback(Component.literal(" §7Total: §e" + (state.totalBytes / 1024) + "KB§7 in §e" + uptime + "s§r"));
		source.sendFeedback(Component.literal(" §7Avg Frame: §e" + avgSize + "§7, Avg FPS: §e" + String.format("%.1f", avgFps) + "§r"));
		source.sendFeedback(Component.literal(" §7Current FPS: §e" + state.currentFps + "§7, Bitrate: §e" + state.currentBitrate + "kbps§r"));
		source.sendFeedback(Component.literal(" §7Adaptive FPS Factor: §e" + String.format("%.2f", wlc.windowShareManager.getAdaptiveFpsFactor()) + "§r"));
		source.sendFeedback(Component.literal(" §7Bandwidth Util: §e" + String.format("%.1f%%", wlc.windowShareManager.getBitrateUtilization() * 100) + "§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		return 1;
	}

	// ===== X11 窗口共享命令（微信等 X11-only 应用） =====

	/** 默认使用 satellite X display（微信跑在上面）；获取不到时用 null（进程默认 DISPLAY） */
	private static String getSatelliteDisplayOrDefault() {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc != null && wlc.bridge != null) {
			try {
				String d = wlc.bridge.getSatelliteDisplay();
				if(d != null && !d.isEmpty()) return d;
			} catch(Throwable ignored) {}
		}
		return null;
	}

	private static String getX11DisplayArg(CommandContext<FabricClientCommandSource> context, String fallback) {
		try {
			String d = StringArgumentType.getString(context, "display");
			if(d != null && !d.isEmpty()) return d;
		} catch(IllegalArgumentException ignored) {}
		return fallback;
	}

	/**
	 * /wl x11 list [display]
	 * 列出 X11 显示上的顶层窗口（satellite display 默认）
	 */
	private static int x11List(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		String display = getX11DisplayArg(context, getSatelliteDisplayOrDefault());

		java.util.List<dev.evvie.waylandcraft.utils.X11WindowLister.WindowInfo> windows =
			dev.evvie.waylandcraft.utils.X11WindowLister.getDesktopWindows(display);

		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		source.sendFeedback(Component.literal("§6 §lX11 Windows §r§7(display: §e" + (display != null ? display : "default") + "§7)§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));

		if(windows.isEmpty()) {
			source.sendFeedback(Component.literal(" §7No X11 windows found (is satellite running?)§r"));
			source.sendFeedback(Component.literal(" §7Use §e/wl x11 list <display>§7 to check another display§r"));
			return 0;
		}

		int i = 1;
		for(dev.evvie.waylandcraft.utils.X11WindowLister.WindowInfo w : windows) {
			source.sendFeedback(Component.literal(" §e" + i + "§7. §f" + w.title + "§r §7[§e0x" + w.hash + "§7] pid=§e" + w.pid + "§7 app=§e" + (w.appId != null ? w.appId : "?") + "§r"));
			i++;
		}
		source.sendFeedback(Component.literal(" §7Use §e/wl x11 share <index>§7 to share§r"));
		source.sendFeedback(Component.literal("§6▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬"));
		return 1;
	}

	/**
	 * /wl x11 share <index> [display]
	 * 共享列表中的第 index 个 X11 窗口
	 */
	private static int x11Share(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		int index = IntegerArgumentType.getInteger(context, "index");
		String display = getX11DisplayArg(context, getSatelliteDisplayOrDefault());

		java.util.List<dev.evvie.waylandcraft.utils.X11WindowLister.WindowInfo> windows =
			dev.evvie.waylandcraft.utils.X11WindowLister.getDesktopWindows(display);
		if(index < 1 || index > windows.size()) {
			source.sendError(Component.literal("§c✘ Index out of range (1-" + windows.size() + ")§r"));
			return 0;
		}

		dev.evvie.waylandcraft.utils.X11WindowLister.WindowInfo info = windows.get(index - 1);
		long xid;
		try {
			xid = Long.parseUnsignedLong(info.hash, 16);
		} catch(NumberFormatException e) {
			source.sendError(Component.literal("§c✘ Invalid window id: " + info.hash + "§r"));
			return 0;
		}

		boolean ok = wlc.windowShareManager.startX11Sharing(xid, info.title, display, info.appId, info.pid);
		if(ok) {
			source.sendFeedback(Component.literal("§a✔ Sharing X11 window §f" + info.title + "§a (§e0x" + info.hash + "§a)§r"));
			source.sendFeedback(Component.literal(" §7Remote players can see it in world (origin). Audio follows window PID §e" + info.pid + "§7 if available§r"));
		} else {
			source.sendError(Component.literal("§c✘ Failed to share X11 window (already shared or inaccessible?)§r"));
		}
		return ok ? 1 : 0;
	}

	/**
	 * /wl x11 stop <handle>
	 * 停止共享 X11 窗口
	 */
	private static int x11Stop(CommandContext<FabricClientCommandSource> context) {
		FabricClientCommandSource source = context.getSource();
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			source.sendError(Component.literal("§c✘ WaylandCraft not initialized§r"));
			return 0;
		}

		String handleStr = StringArgumentType.getString(context, "handle");
		long xid = parseWindowHandle(handleStr);
		if(xid < 0) {
			source.sendError(Component.literal("§c✘ Invalid handle: " + handleStr + "§r"));
			return 0;
		}

		WindowShareManager.ShareState state = wlc.windowShareManager.getShareState(xid);
		if(state == null || state.source != WindowShareManager.ShareState.Source.X11) {
			source.sendError(Component.literal("§c✘ 0x" + Long.toHexString(xid) + " is not an actively shared X11 window§r"));
			return 0;
		}

		wlc.windowShareManager.stopSharing(xid);
		source.sendFeedback(Component.literal("§a✔ Stopped sharing X11 window §f" + state.windowTitle + "§r"));
		return 1;
	}

	// ===== 权限命令 =====

	private static int permList(CommandContext<FabricClientCommandSource> context) {
		ClientPlayNetworking.send(new PermissionCommandPayload(
			PermissionCommandPayload.ACTION_LIST, "", (byte) 0));
		return 1;
	}

	private static int permDefault(CommandContext<FabricClientCommandSource> context) {
		String permStr = StringArgumentType.getString(context, "permission").toUpperCase();
		WindowPermission perm = parsePerm(permStr, context.getSource());
		if(perm == null) return 0;
		ClientPlayNetworking.send(new PermissionCommandPayload(
			PermissionCommandPayload.ACTION_SET_DEFAULT, "", (byte) perm.ordinal()));
		return 1;
	}

	private static int permAllow(CommandContext<FabricClientCommandSource> context) {
		String playerName = StringArgumentType.getString(context, "player");
		String permStr = StringArgumentType.getString(context, "permission").toUpperCase();
		WindowPermission perm = parsePerm(permStr, context.getSource());
		if(perm == null) return 0;
		ClientPlayNetworking.send(new PermissionCommandPayload(
			PermissionCommandPayload.ACTION_ALLOW, playerName, (byte) perm.ordinal()));
		return 1;
	}

	private static int permDeny(CommandContext<FabricClientCommandSource> context) {
		String playerName = StringArgumentType.getString(context, "player");
		ClientPlayNetworking.send(new PermissionCommandPayload(
			PermissionCommandPayload.ACTION_DENY, playerName, (byte) 0));
		return 1;
	}

	private static int permRemove(CommandContext<FabricClientCommandSource> context) {
		String playerName = StringArgumentType.getString(context, "player");
		ClientPlayNetworking.send(new PermissionCommandPayload(
			PermissionCommandPayload.ACTION_REMOVE, playerName, (byte) 0));
		return 1;
	}

	private static WindowPermission parsePerm(String permStr, FabricClientCommandSource source) {
		try {
			return WindowPermission.valueOf(permStr);
		} catch(IllegalArgumentException e) {
			source.sendError(Component.literal("§c✘ Invalid permission: " + permStr + " (NONE/VIEW/INTERACT/CONTROL)§r"));
			return null;
		}
	}
}
