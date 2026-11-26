package dev.evvie.waylandcraft.network;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.function.Function;
import java.util.function.Predicate;

import org.jetbrains.annotations.Nullable;

import dev.evvie.waylandcraft.mixin.IConnection;
import dev.evvie.waylandcraft.mixin.IServerCommonPacketListener;
import io.netty.channel.Channel;
import io.netty.channel.WriteBufferWaterMark;
import net.minecraft.network.Connection;
import net.minecraft.server.level.ServerPlayer;

/**
 * P1-1/P1-2：转发出口的公共门控逻辑。
 *
 * - selectCapable：把"按 canSend 过滤 + 本轮结果缓存"抽成纯泛型函数（MC 依赖
 *   只存在于调用方传入的 probe lambda），可无 mixin 单元测试。
 * - isChannelWritable：发送前背压检查。vanilla（26.1.2 实测）不对 play channel
 *   设置 WRITE_BUFFER_WATER_MARK（net/minecraft/network、server/network 全部类
 *   均无 WriteBufferWaterMark 引用），默认水位 32K/64K 对 MB 级帧包过敏感，
 *   首次触达时把该 channel 调到 1MB/4MB：isWritable 真正反映"积压超约一帧预算"。
 */
public final class RelaySinks {

	/** netty ChannelConfig 的默认水位；仍等于它说明 vanilla 没自定义，需要我们调高 */
	private static final int NETTY_DEFAULT_LOW = 32 * 1024;
	private static final int NETTY_DEFAULT_HIGH = 64 * 1024;

	/** 共享窗口是 MB 级帧流：低水位 1MB、高水位 4MB（约 0.2~0.5s 积压即判慢） */
	private static final int WC_WATERMARK_LOW = 1 << 20;
	private static final int WC_WATERMARK_HIGH = 4 << 20;

	private RelaySinks() {}

	/**
	 * 纯逻辑过滤：对每个候选者用 cache（本轮共享）查 canSend 结果，未命中则
	 * probe 一次并回填。返回通过过滤的列表。probe 的 MC 依赖由调用方注入。
	 */
	public static <T> List<T> selectCapable(
			List<T> candidates,
			Function<T, UUID> idFn,
			Map<UUID, Boolean> roundCache,
			Predicate<T> probe) {
		List<T> capable = new ArrayList<>(candidates.size());
		for (T candidate : candidates) {
			UUID id = idFn.apply(candidate);
			Boolean ok = roundCache.get(id);
			if (ok == null) {
				ok = probe.test(candidate);
				roundCache.put(id, ok);
			}
			if (ok) capable.add(candidate);
		}
		return capable;
	}

	/**
	 * mixin 访问器取玩家的 netty Channel；未加载 mixin（单测）或断连竞态时返回 null，
	 * 调用方按"未知=放行"处理（ServerPlayNetworking.send 自身对死连接是 no-op）。
	 */
	@Nullable
	public static Channel channelOf(ServerPlayer player) {
		try {
			if (!(player.connection instanceof IServerCommonPacketListener listener)) return null;
			Connection connection = listener.waylandcraft_getNetworkConnection();
			if (!(connection instanceof IConnection conn)) return null;
			return conn.waylandcraft_getChannel();
		} catch (Throwable t) {
			// 访问器缺失（异常环境）不应把转发崩掉：退化为"可写"
			return null;
		}
	}

	/**
	 * 背压检查：channel 不可写（发送队列已越过水位的 high）时跳过本帧给该玩家。
	 * 顺带完成一次性水位校准（仅当仍是 netty 默认水位时覆盖）。
	 */
	public static boolean isChannelWritable(ServerPlayer player) {
		Channel channel = channelOf(player);
		if (channel == null) return true;
		if (!channel.isActive()) return false;
		try {
			WriteBufferWaterMark mark = channel.config().getWriteBufferWaterMark();
			if (mark != null && mark.low() == NETTY_DEFAULT_LOW && mark.high() == NETTY_DEFAULT_HIGH) {
				channel.config().setWriteBufferWaterMark(new WriteBufferWaterMark(WC_WATERMARK_LOW, WC_WATERMARK_HIGH));
			}
		} catch (Throwable t) {
			// config 访问异常（channel 已关闭等）不阻断判定
		}
		return channel.isWritable();
	}
}
