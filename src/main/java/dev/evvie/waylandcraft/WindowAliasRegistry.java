package dev.evvie.waylandcraft;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Random;
import java.util.Set;

/**
 * 窗口实例别名注册表。
 * 
 * 为每个 toplevel 分配一个会话内唯一的随机别名（4 位字母数字，如 k7xq），
 * 通过 /wl list windows 直接可见、可直接用于所有 <handle> 参数。
 * 
 * 字符集剔除易混字符 0/o、1/l/i，避免玩家手动输入时敲错；
 * 31^4 ≈ 92 万组合，多人共享场景下冲突概率极低，冲突时自动重试。
 * 
 * 别名在会话（游戏进程）内保持稳定；重启后 handle 变化，别名也会重新分配
 * —— 与临时模板的语义一致。
 */
public class WindowAliasRegistry {

	private static final char[] ALIAS_CHARS = "23456789abcdefghjkmnpqrstuvwxyz".toCharArray();
	private static final int ALIAS_LENGTH = 4;

	private final Map<Long, String> aliasByHandle = new HashMap<>();
	private final Map<String, Long> handleByAlias = new HashMap<>();
	private final Random random = new Random();

	/** 获取窗口别名，不存在则分配一个新的（4 位随机，如 k7xq） */
	public String getOrCreate(long handle) {
		String alias = aliasByHandle.get(handle);
		if(alias != null) return alias;

		alias = nextAlias();
		aliasByHandle.put(handle, alias);
		handleByAlias.put(alias, handle);
		return alias;
	}

	/** 生成一个当前未被占用的随机别名 */
	private String nextAlias() {
		while(true) {
			StringBuilder sb = new StringBuilder(ALIAS_LENGTH);
			for(int i = 0; i < ALIAS_LENGTH; i++) {
				sb.append(ALIAS_CHARS[random.nextInt(ALIAS_CHARS.length)]);
			}
			String alias = sb.toString();
			if(!handleByAlias.containsKey(alias)) return alias;
		}
	}

	/** 获取已有别名，没有则返回 null */
	public String get(long handle) {
		return aliasByHandle.get(handle);
	}

	/** 别名 -> handle，未注册返回 null */
	public Long resolve(String alias) {
		return handleByAlias.get(alias);
	}

	/** 清理已消失窗口的别名映射（编号不回收，保持会话内唯一稳定） */
	public void cleanup(Set<Long> aliveHandles) {
		List<String> dead = new ArrayList<>();
		for(Map.Entry<String, Long> e : handleByAlias.entrySet()) {
			if(!aliveHandles.contains(e.getValue())) dead.add(e.getKey());
		}
		for(String alias : dead) {
			Long handle = handleByAlias.remove(alias);
			if(handle != null) aliasByHandle.remove(handle);
		}
	}

}
