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
}
