package dev.evvie.waylandcraft.utils;

import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

/**
 * P1-6/P1-7：服务端告警限频器。
 *
 * 恶意/异常客户端可以高频触发"被拒"路径（交互越权、注册超限、超大帧……），
 * 逐包 WARN 会形成日志洪泛。同一 key 在 intervalMs 内只放行第一条，
 * 其余静默计数由调用方统计。
 *
 * 线程安全（netty 线程与服务端线程都会调用），会话结束 reset()。
 */
public final class ServerWarnThrottle {

	private static final Map<String, Long> lastAllowedAt = new ConcurrentHashMap<>();

	private ServerWarnThrottle() {}

	/**
	 * 纯逻辑判定（时钟显式传入，供单元测试）：nowMs 时刻该 key 是否应放行告警。
	 */
	public static boolean allowAt(String key, long nowMs, long intervalMs) {
		Long prev = lastAllowedAt.get(key);
		if (prev != null && nowMs - prev < intervalMs) {
			return false;
		}
		lastAllowedAt.put(key, nowMs);
		return true;
	}

	/** 以墙钟为基准的便捷入口。 */
	public static boolean allow(String key, long intervalMs) {
		return allowAt(key, System.currentTimeMillis(), intervalMs);
	}

	/** 按前缀清除（各子系统会话结束时清掉自己的 key 空间）。 */
	public static void removePrefix(String prefix) {
		lastAllowedAt.keySet().removeIf(k -> k.startsWith(prefix));
	}

	/** P2-5：跨会话清空，避免旧玩家 key 永驻。 */
	public static void reset() {
		lastAllowedAt.clear();
	}
}
