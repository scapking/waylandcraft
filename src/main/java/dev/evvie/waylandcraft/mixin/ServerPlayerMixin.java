package dev.evvie.waylandcraft.mixin;

import java.util.HashSet;
import java.util.Set;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

import dev.evvie.waylandcraft.utils.IMyServerPlayer;
import net.minecraft.server.level.ServerPlayer;

@Mixin(ServerPlayer.class)
public class ServerPlayerMixin implements IMyServerPlayer {

	private int itemGiveCooldown = 0;

	/**
	 * P1-5：ArrayList → HashSet：
	 * - isHandleValid 每 tick 每槽位 contains()，O(n) → O(1)；
	 * - 集合只在服务端线程读写（receiver 移入 execute 后成立）；
	 * - 去重语义匹配"存活窗口集合"。
	 */
	private Set<Long> aliveWindows = new HashSet<Long>();

	@Override
	public int getItemGiveCooldown() {
		return itemGiveCooldown;
	}

	@Override
	public void setItemGiveCooldown(int cooldown) {
		itemGiveCooldown = cooldown;
	}

	@Override
	public Set<Long> getAliveWindows() {
		return aliveWindows;
	}

	@Inject(method = "restoreFrom", at = @At("TAIL"))
	public void myRestoreFrom(ServerPlayer oldPlayer, boolean restoreAll, CallbackInfo info) {
		aliveWindows = ((IMyServerPlayer) oldPlayer).getAliveWindows();
	}

}
