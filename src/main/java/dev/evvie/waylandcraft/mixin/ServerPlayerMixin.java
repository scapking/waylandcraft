package dev.evvie.waylandcraft.mixin;

import java.util.HashSet;
import java.util.Set;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

import dev.evvie.waylandcraft.network.ImmerseServer;
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

	/**
	 * v1.5.0 沉浸 ghost 模式：对其他观察者隐身。
	 *
	 * vanilla 语义（26.1.2 字节码验证）：ChunkMap$TrackedEntity.updatePlayer 在
	 * broadcastToPlayer(observer)==false 时走 removePlayer(observer) 分支
	 * （seenBy 移除 + ServerEntity.removePairing → 下发 ClientboundRemoveEntitiesPacket）；
	 * updatePlayer 由 ChunkMap.tick 每 tick 驱动（另在 move/addEntity 处驱动），
	 * 故进入 ghost 后 1 tick 内自动消失、退出后恢复 vanilla 语义（含旁观者模式
	 * 原有判定）时同一路径自动重配对。HEAD 取消保证旁观者逻辑不被破坏。
	 */
	@Inject(method = "broadcastToPlayer", at = @At("HEAD"), cancellable = true)
	public void mySuppressGhostBroadcast(ServerPlayer observer, CallbackInfoReturnable<Boolean> cir) {
		if (ImmerseServer.shouldSuppressBroadcast((ServerPlayer) (Object) this, observer)) {
			cir.setReturnValue(false);
		}
	}

}
