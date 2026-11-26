package dev.evvie.waylandcraft.item;

import java.util.Arrays;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.bridge.WLCToplevel;
import dev.evvie.waylandcraft.desktop.DesktopEntry;
import dev.evvie.waylandcraft.localmode.LocalModePolicy;
import dev.evvie.waylandcraft.network.ServerboundAliveWindowsPayload;
import dev.evvie.waylandcraft.network.ServerboundGiveItemsPayload;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientTickEvents;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking;
import net.minecraft.client.Minecraft;
import net.minecraft.client.player.LocalPlayer;
import net.minecraft.network.chat.Component;
import net.minecraft.world.entity.player.Inventory;
import net.minecraft.world.item.ItemStack;

public class WindowItemManager implements WindowItemInteractionProvider, ClientTickEvents.StartTick {
	
	private static Component UNKNOWN_WINDOW_TEXT = Component.literal("Unknown Window");
	
	// Toplevel handles last sent to the server for synchronization
	private long[] syncedToplevels = new long[0];
	
	// ⑩ vanilla 服务端下这些 payload 会被静默丢弃：直接不发，
	// 避免每 tick 每窗口往连接里灌无意义流量（窗口显示走 WaylandCraft 本地直显路径）。
	// 反作弊红线：物品/存活窗口 payload 的发送门**严格**跟随服务端 mod 存在性，
	// 任何 localItems 档位都不影响此处（items 档走下方客户端本地物品路径，不发包）。
	// instance==null（初始化前）按不放行处理——所有调用点都在客户端就绪后。
	private boolean serverCanTrackItems() {
		return WaylandCraft.instance != null && LocalModePolicy.itemPayloadSendAllowed(WaylandCraft.instance.serverHasMod);
	}

	/** 只读背包查询（零写入）：该 toplevel 是否已有窗口物品。give 回查/items 档去重共用。 */
	public boolean hasWindowItemFor(long toplevelHandle) {
		LocalPlayer player = Minecraft.getInstance().player;
		if(player == null) return false;

		Inventory inv = player.getInventory();
		WindowHandle searched = WindowHandle.forPlayer(player, toplevelHandle);
		for(int i = 0; i < inv.getContainerSize(); i++) {
			ItemStack stack = inv.getItem(i);
			if(!stack.is(WindowItem.WINDOW)) continue;
			WindowHandle data = stack.get(WindowItem.WINDOW_HANDLE);
			if(data != null && data.equals(searched)) return true;
		}
		return false;
	}

	/**
	 * 显式 items 档（服务端无 mod、用户手动开启）：客户端本地创建窗口物品。
	 * 这是客户端背包写入的唯一合法路径——调用方必须先经
	 * LocalModePolicy.clientItemWriteAllowed 判定（无 mod 服 + 显式档）。
	 * 反作弊服开启此项可能被判刷物品：用户自担风险，默认 auto 绝不走到这里。
	 * 服务端每次背包整包回同步后，updateWorld 的 items 档循环会自动补回。
	 */
	public boolean giveItemLocally(WLCToplevel toplevel) {
		if(toplevel == null) return false;

		// 纵深防御：即使调用侧的条件被将来重构破坏，这里也独立复核红线
		// （无 mod 服 + 显式 items 档），不放行即拒写背包并留日志线索。
		WaylandCraft gate = WaylandCraft.instance;
		if(gate == null || gate.serverHasMod
				|| !LocalModePolicy.clientItemWriteAllowed(false, gate.effectiveLocalItems())) {
			return false;
		}

		LocalPlayer player = Minecraft.getInstance().player;
		if(player == null) return false;

		if(hasWindowItemFor(toplevel.getHandle())) return true;

		ItemStack stack = new ItemStack(WindowItem.WINDOW, 1);
		stack.set(WindowItem.WINDOW_HANDLE, WindowHandle.forPlayer(player, toplevel.getHandle()));
		return player.getInventory().add(stack);
	}

	/** items 档每 tick 幂等补给（与标准模式 giveItemsIfMissing 同形，纯客户端）。 */
	public void giveItemsLocallyIfMissing(WLCToplevel[] toplevels) {
		for(WLCToplevel t : toplevels) giveItemLocally(t);
	}

	public void giveItem(WLCToplevel toplevel) {
		if(!serverCanTrackItems()) return;
		ClientPlayNetworking.send(new ServerboundGiveItemsPayload(new long[] {toplevel.getHandle()}, false));
	}
	
	public void giveItemsIfMissing(WLCToplevel... toplevels) {
		if(!serverCanTrackItems()) return;
		if(toplevels.length < 1) return;
		
		long[] handles = new long[toplevels.length];
		for(int i = 0; i < toplevels.length; i++) handles[i] = toplevels[i].getHandle();
		
		syncToplevels();
		ClientPlayNetworking.send(new ServerboundGiveItemsPayload(handles, true));
	}
	
	@Override
	public void onStartTick(Minecraft client) {
		if(client.level == null) return;
		if(WaylandCraft.instance.bridge == null) return;
		
		syncToplevels();
	}
	
	public void syncToplevels() {
		if(!serverCanTrackItems()) return;
		long[] handles = Arrays.stream(WaylandCraft.instance.bridge.getToplevels()).mapToLong((t) -> t.getHandle()).toArray();
		if(Arrays.equals(handles, syncedToplevels)) return;
		
		ClientPlayNetworking.send(new ServerboundAliveWindowsPayload(handles));
		syncedToplevels = handles;
	}
	
	public void reset() {
		syncedToplevels = new long[0];
	}
	
	@Override
	public boolean isValid(ItemStack itemStack) {
		WLCToplevel toplevel = WaylandCraft.getToplevel(itemStack);
		return toplevel != null;
	}
	
	@Override
	public Component getName(ItemStack itemStack) {
		WLCToplevel toplevel = WaylandCraft.getToplevel(itemStack);
		if(toplevel == null) return UNKNOWN_WINDOW_TEXT;
		
		DesktopEntry entry = WaylandCraft.instance.xdgManager.forAppId(toplevel.appID);
		if(entry == null) return UNKNOWN_WINDOW_TEXT;
		
		String name = entry.name;
		if(name == null) return UNKNOWN_WINDOW_TEXT;
		
		return Component.literal(name);
	}
	
}
