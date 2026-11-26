package dev.evvie.waylandcraft.utils;

import java.util.Set;

public interface IMyServerPlayer {

	void setItemGiveCooldown(int cooldown);
	int getItemGiveCooldown();

	/**
	 * P1-5：客户端上报的存活窗口句柄集合。HashSet（O(1) contains，且
	 * 去重语义天然）；所有读写都发生在服务端线程（receiver 已移入
	 * ctx.server().execute，物品 tick 本就在服务端线程）。
	 */
	Set<Long> getAliveWindows();

}
