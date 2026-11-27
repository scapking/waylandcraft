package dev.evvie.waylandcraft.network;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

/**
 * 每观众订阅上限（overload-guard：单个 viewer 同时观看窗口数上限）。
 *
 * 背景：owner 最多可共享 24 个窗口（MAX_WINDOWS_PER_PLAYER），观众端会为每个
 * 有 VIEW 权的窗口建显示对象 + 接收 20fps 帧流 + 音频流。观众过多 × 窗口过多时
 * 转发放大 = O(观众 × 窗口) 帧发送，是"观众过多打满端口/内存"的主放大器之一。
 *
 * 语义（最小侵入）：
 * - "VIEW 交付"以窗口列表广播（SharedWindowServerHandler.broadcastWindowListToAll）
 *   为权威：广播时按句柄升序取前 {@link #MAX_WINDOWS_PER_VIEWER} 个非本人窗口
 *   登记为交付集，超出部分不进列表（观众端不渲染）。
 * - 视频/音频转发前查登记表（isDelivered），未登记的窗口不转发——与列表保持一致。
 * - 授权本身不撤销（PermissionManager/grants 不动）：窗口数回落（关闭/注销其他
 *   窗口）后下一次广播自动恢复交付。
 * - fail-open：登记表中不存在的观众（首次进服/广播未跑过）放行——与 RelaySinks
 *   "未知即放行"哲学一致；广播每次权限变化后 1 tick 内必然重算，登记表不缺位。
 * - owner 自己的窗口不占观众额度（relay 本就不向发送者回发自己的帧）。
 *
 * 线程模型：refresh 只在服务端线程（列表广播）；isDelivered 在转发线程读，
 * ConcurrentHashMap + 不可变 Set 保证安全发布。retainOnly/clear 在 tick/会话清理。
 */
public final class ViewerSubscriptions {

	/** 单个观众同时接收转发的窗口数上限 */
	public static final int MAX_WINDOWS_PER_VIEWER = 16;

	/** viewer → 交付集（不可变 Set，refresh 时整体替换） */
	private static final Map<UUID, Set<Long>> delivered = new ConcurrentHashMap<>();

	/** viewer → 最近一次广播因上限被暂缓交付的窗口数（诊断用） */
	private static final Map<UUID, Integer> lastDenied = new ConcurrentHashMap<>();

	private ViewerSubscriptions() {}

	/**
	 * 纯函数（可单测）：候选句柄按升序取前 max 个为交付集。
	 * 升序截断 = 确定性（同一批候选在任何 tick 重算结果一致，交付集不抖动）。
	 * max ≤ 0 或空候选 → 空集。
	 */
	public static Set<Long> capDelivered(List<Long> candidates, int max) {
		if (max <= 0 || candidates.isEmpty()) return Set.of();
		List<Long> sorted = new ArrayList<>(candidates);
		Collections.sort(sorted);
		if (sorted.size() <= max) return Set.copyOf(sorted);
		return Set.copyOf(sorted.subList(0, max));
	}

	/**
	 * 服务端线程：列表广播时登记该观众本轮交付集与暂缓数（整体替换）。
	 */
	public static void refresh(UUID viewer, Set<Long> deliveredHandles, int deniedCount) {
		if (viewer == null) return;
		delivered.put(viewer, Set.copyOf(deliveredHandles));
		lastDenied.put(viewer, deniedCount);
	}

	/**
	 * 转发线程：该观众是否登记了此窗口的交付。无登记 = 放行（fail-open）。
	 */
	public static boolean isDelivered(UUID viewer, long windowHandle) {
		Set<Long> set = delivered.get(viewer);
		return set == null || set.contains(windowHandle);
	}

	/** 最近一次广播因上限暂缓的窗口数（诊断用；无记录 = 0） */
	public static int lastDeniedCount(UUID viewer) {
		Integer n = lastDenied.get(viewer);
		return n == null ? 0 : n;
	}

	/** 当前登记的观众数（诊断用） */
	public static int trackedViewers() {
		return delivered.size();
	}

	/** 全体观众最近一次暂缓数之和（诊断用） */
	public static int totalDenied() {
		int sum = 0;
		for (Integer n : lastDenied.values()) {
			if (n != null) sum += n;
		}
		return sum;
	}

	/** 下线观众清理（tick 调用，防 map 泄漏） */
	public static void retainOnly(Set<UUID> currentOnline) {
		delivered.keySet().retainAll(currentOnline);
		lastDenied.keySet().retainAll(currentOnline);
	}

	public static void clear() {
		delivered.clear();
		lastDenied.clear();
	}
}
