package dev.evvie.waylandcraft.shared;

public enum WindowPermission {
	NONE(0),
	VIEW(1),
	INTERACT(2),
	CONTROL(3);

	private final int level;

	WindowPermission(int level) {
		this.level = level;
	}

	public int getLevel() {
		return level;
	}

	public boolean hasPermission(WindowPermission required) {
		return this.level >= required.level;
	}

	/**
	 * P0-1：把客户端传来的权限等级钳位到合法 ordinal 范围 [0, values().length-1]。
	 * 网络字节可为负数或 >3，直接 values()[level] 会 ArrayIndexOutOfBounds 崩服。
	 */
	public static WindowPermission fromLevelClamped(int level) {
		WindowPermission[] all = values();
		if (level <= 0) return NONE;
		if (level >= all.length) return all[all.length - 1];
		return all[level];
	}

	/**
	 * P1-6：入站解码专用——查表 + 非法回退 NONE（拒绝，绝不钳位提升）。
	 * 与 fromLevelClamped 的分工：fromLevelClamped 越界钳到 CONTROL，适合服务端处理
	 * owner 自己的授权命令；本方法用于不可信对端（服务端→客户端的列表/权限包），
	 * 越界或负数必须降级为 NONE——否则恶意字节能把 VIEW 提权成 CONTROL，
	 * 且 values()[varInt] 直接下标会 ArrayIndexOutOfBounds 崩掉解码线程。
	 */
	public static WindowPermission fromLevel(int level) {
		WindowPermission[] all = values();
		if (level < 0 || level >= all.length) return NONE;
		return all[level];
	}
}
