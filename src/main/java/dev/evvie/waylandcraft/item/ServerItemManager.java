package dev.evvie.waylandcraft.item;

import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.stream.StreamSupport;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.network.ServerboundGiveItemsPayload;
import dev.evvie.waylandcraft.utils.IMyServerPlayer;
import dev.evvie.waylandcraft.utils.ServerWarnThrottle;
import dev.evvie.waylandcraft.utils.WaylandCraftUtils;
import net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking;
import net.minecraft.core.particles.ParticleTypes;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.entity.item.ItemEntity;
import net.minecraft.world.entity.player.Inventory;
import net.minecraft.world.item.ItemStack;

/**
 * 服务端窗口物品管理。
 *
 * P2-7（server-polish）：原实现挂在 START_LEVEL_TICK，每个维度每 tick 全扫
 * 玩家库存 + 全实体流（N 维度 × 每 tick）。改为单次 END_SERVER_TICK：
 * - 玩家库存清理与冷却递减合并为一个 pass；
 * - 掉落物实体扫描降频到每 16 tick（约 0.8s， burning 特效不必逐 tick）；
 * - early-out 缓存：自上次扫描后没有任何窗口物品被创建/持有 → 跳过实体流。
 *
 * P0-2：handleGiveItemsPayload 由 receiver 移入 ctx.server().execute 调用，
 * 本类内不再有 netty 线程入口；句柄去重用 LinkedHashSet（O(n) 替代 O(n²)），
 * 超限（>1024）整包丢弃 + 限频告警。
 */
public class ServerItemManager {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-item");

	/** P2-7：掉落物实体扫描间隔（tick） */
	static final int ENTITY_SCAN_INTERVAL_TICKS = 16;

	/** P0-2：单次 give_items 上报句柄上限 */
	static final int MAX_HANDLES_PER_PAYLOAD = 1024;

	/** 只在服务端线程读写 */
	private long tickCounter = 0;

	/**
	 * P2-7 early-out：是否可能存在需要清理的窗口物品（创建/持有时置位，
	 * 一轮实体扫描未发现任何窗口物品实体则清位）。初始为 true：
	 * 保守假设世界加载后可能已有存量物品。
	 */
	private static volatile boolean windowItemsPossible = true;

	/**
	 * P0-2：服务端线程调用（receiver 已包 execute）。
	 */
	public void handleGiveItemsPayload(ServerboundGiveItemsPayload payload, ServerPlayNetworking.Context ctx) {
		ServerPlayer player = ctx.player();
		IMyServerPlayer plr = (IMyServerPlayer) player;
		if(plr.getItemGiveCooldown() > 0) return;
		plr.setItemGiveCooldown(10);

		// P0-2：去重 + 限长（纯函数，见 sanitizeHandles）
		List<Long> handles = sanitizeHandles(payload.handles(), MAX_HANDLES_PER_PAYLOAD);
		if (handles == null) {
			if (ServerWarnThrottle.allow("give-oversize:" + player.getUUID(), 5000)) {
				LOGGER.warn("Player {} sent {} handles in one give_items payload (>{}), dropped",
					player.getUUID(), payload.handles().length, MAX_HANDLES_PER_PAYLOAD);
			}
			return;
		}

		if(payload.missingOnly()) giveItemsIfMissing(player, handles);
		else giveItems(player, handles);
	}

	/**
	 * P0-2：纯函数（可单测）：按首次出现顺序去重；超过 max 返回 null 表示整包丢弃。
	 * 原实现 handles.contains() 是 O(n²)。
	 */
	public static List<Long> sanitizeHandles(long[] handles, int max) {
		if (handles == null || handles.length == 0) return List.of();
		if (handles.length > max) return null;
		LinkedHashSet<Long> dedup = new LinkedHashSet<>(handles.length * 2);
		for (long handle : handles) {
			dedup.add(handle);
		}
		return new ArrayList<>(dedup);
	}

	public void giveItems(ServerPlayer player, List<Long> handles) {
		for(Long handle : handles) giveItem(player, handle);
	}

	public void giveItemsIfMissing(ServerPlayer player, List<Long> handles) {
		for(Long handle : handles) giveItemIfMissing(player, handle);
	}

	public void giveItem(ServerPlayer player, long handle) {
		ItemStack item = createItem(player, handle);
		player.addItem(item);
	}

	public void giveItemIfMissing(ServerPlayer player, long handle) {
		Inventory inv = player.getInventory();
		WindowHandle searched = WindowHandle.forPlayer(player, handle);

		boolean foundToplevel = false;
		for(int i = 0; i < inv.getContainerSize(); i++) {
			ItemStack item = inv.getItem(i);
			WindowHandle data = item.get(WindowItem.WINDOW_HANDLE);

			if(!item.is(WindowItem.WINDOW)) continue;
			if(data != null && data.equals(searched)) {
				foundToplevel = true;
				break;
			}
		}

		if(!foundToplevel) {
			ItemStack item = createItem(player, handle);
			player.addItem(item);
		}
	}

	public static ItemStack createItem(ServerPlayer player, long handle) {
		ItemStack stack = new ItemStack(WindowItem.WINDOW, 1);
		stack.set(WindowItem.WINDOW_HANDLE, WindowHandle.forPlayer(player, handle));
		windowItemsPossible = true; // P2-7：有新窗口物品诞生，恢复实体扫描
		return stack;
	}

	/**
	 * P2-7：单次 END_SERVER_TICK 覆盖所有维度。
	 * 每 tick：玩家库存无效句柄清理 + give 冷却递减（合并为同一 pass，
	 * 原来两段玩家循环也合并了）。
	 * 每 16 tick（且未 early-out）：掉落物实体扫描焚毁。
	 */
	public void onServerTick(MinecraftServer server) {
		tickCounter++;
		boolean scanEntities = windowItemsPossible && (tickCounter % ENTITY_SCAN_INTERVAL_TICKS == 0);

		boolean anyWindowItemHeld = false;
		for (ServerLevel level : server.getAllLevels()) {
			for (ServerPlayer player : level.players()) {
				Inventory inv = player.getInventory();
				for(int i = 0; i < inv.getContainerSize(); i++) {
					ItemStack item = inv.getItem(i);
					if(!item.is(WindowItem.WINDOW)) continue;
					anyWindowItemHeld = true;

					WindowHandle handle = item.get(WindowItem.WINDOW_HANDLE);
					if(!WaylandCraftUtils.isHandleValid(level, handle)) {
						inv.setItem(i, ItemStack.EMPTY);
					}
				}

				IMyServerPlayer plr = (IMyServerPlayer) player;
				int itemGiveCooldown = plr.getItemGiveCooldown();
				if(itemGiveCooldown > 0) {
					plr.setItemGiveCooldown(itemGiveCooldown - 1);
				}
			}

			if (!scanEntities) continue;

			final boolean[] found = { false };
			StreamSupport.stream(level.getAllEntities().spliterator(), false)
				.filter((e) -> e instanceof ItemEntity)
				.map((e) -> (ItemEntity) e)
				.filter((e) -> e.getItem().is(WindowItem.WINDOW))
				.forEach((e) -> {
					found[0] = true;
					// 有效句柄的掉落物也要持续跟踪（可能随时失效），保持扫描开启
					if (!WaylandCraftUtils.isHandleValid(level, e.getItem().get(WindowItem.WINDOW_HANDLE))
						&& e.getAge() > 10) {
						level.sendParticles(ParticleTypes.FLAME, false, false, e.getX(), e.getY(), e.getZ(), 10, 0.15, 0.2, 0.15, 0.1);
						e.discard();
					}
				});
			if (found[0]) anyWindowItemHeld = true;
		}

		// early-out：本轮既没人持有也没扫到掉落物，且期间没有新建（createItem 会重新置位）
		if (scanEntities && !anyWindowItemHeld) {
			windowItemsPossible = false;
		}
	}

}
