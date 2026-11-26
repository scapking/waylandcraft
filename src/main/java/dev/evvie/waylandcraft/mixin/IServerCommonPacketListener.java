package dev.evvie.waylandcraft.mixin;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;

import net.minecraft.network.Connection;
import net.minecraft.server.network.ServerCommonPacketListenerImpl;

/**
 * P1-2：读取玩家连接的 netty Connection（字段 protected，外部包不可见），
 * 用于背压检查（channel.isWritable）与 WRITE_BUFFER_WATER_MARK 设置。
 */
@Mixin(ServerCommonPacketListenerImpl.class)
public interface IServerCommonPacketListener {

	@Accessor("connection")
	Connection waylandcraft_getNetworkConnection();
}
