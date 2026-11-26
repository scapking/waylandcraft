package dev.evvie.waylandcraft.mixin;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;

import io.netty.channel.Channel;
import net.minecraft.network.Connection;

/**
 * P1-2：读取 Connection 内部的 netty Channel（private 字段）。
 * Connection.isWritable / WRITE_BUFFER_WATER_MARK 都挂在 Channel.config() 上。
 */
@Mixin(Connection.class)
public interface IConnection {

	@Accessor("channel")
	Channel waylandcraft_getChannel();
}
