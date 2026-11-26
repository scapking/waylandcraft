package dev.evvie.waylandcraft;

import org.jetbrains.annotations.Nullable;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.item.ServerItemManager;
import dev.evvie.waylandcraft.item.WindowItem;
import dev.evvie.waylandcraft.item.WindowItemInteractionProvider;
import dev.evvie.waylandcraft.network.ServerEventHandler;
import dev.evvie.waylandcraft.network.WaylandCraftNetworking;
import dev.evvie.waylandcraft.shared.SharedWindowManager;
import dev.evvie.waylandcraft.shared.PermissionManager;
import net.fabricmc.api.ModInitializer;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerTickEvents;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerLifecycleEvents;
import net.minecraft.server.MinecraftServer;

public class WaylandCraftCommon implements ModInitializer {
	
	public static final String MOD_ID = "waylandcraft";
	public static final Logger LOGGER = LoggerFactory.getLogger(MOD_ID);
	public static WaylandCraftCommon instance;
	
	public @Nullable WindowItemInteractionProvider windowItemInteractionProvider = null;
	public ServerItemManager serverItemManager = new ServerItemManager();
	
	// 多人显示功能（ACL-3：permissionManager 先建并绑定给 sharedWindowManager，
	// 权威合成判定才能消费黑白名单/default 表）
	public PermissionManager permissionManager = new PermissionManager();
	public SharedWindowManager sharedWindowManager = new SharedWindowManager(permissionManager);
	public @Nullable MinecraftServer server = null;
	
	@Override
	public void onInitialize() {
		instance = this;
		WindowItem.register();

		// ⑥ 真实 mod 版本注入 native（status.log / getStatusReport 不再谎报硬编码值）
		dev.evvie.waylandcraft.bridge.WaylandCraftBridge.setModVersion(
			net.fabricmc.loader.api.FabricLoader.getInstance()
				.getModContainer(MOD_ID)
				.map(c -> c.getMetadata().getVersion().getFriendlyString())
				.orElse("unknown"));
		WaylandCraftNetworking.register();
		
		// 注册服务器事件处理器
		ServerEventHandler.register();

		// P2-7：原 START_LEVEL_TICK 每维度每 tick 全扫；改单次 END_SERVER_TICK
		ServerTickEvents.END_SERVER_TICK.register(serverItemManager::onServerTick);

		ServerLifecycleEvents.SERVER_STARTING.register(server -> { this.server = server; });
		ServerLifecycleEvents.SERVER_STOPPING.register(server -> { this.server = null; });

		// P2-5：会话完全结束后清空跨会话残留状态——
		// 单人存档退出/换存档时，旧窗口的权限、relay 缓存、限流计数若残留，
		// 会把上一世界的共享状态带进新世界（窗口 handle 可复用、列表幽灵项）。
		ServerLifecycleEvents.SERVER_STOPPED.register(server -> {
			sharedWindowManager.clear();
			permissionManager.clear();
			WaylandCraftNetworking.clearSessionState();
		});
	}
	
}
