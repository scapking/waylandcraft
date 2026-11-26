package dev.evvie.waylandcraft.placement;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * 本地直显摆位的按服务器持久化（纯逻辑，无 MC/Gson 依赖，可单测）。
 *
 * 存储形态：settings.json 中三个纯字符串容器字段（Map<String,String> /
 * List<String>），本类负责全部编码/解码/键派生——与 WaylandCraftSettings
 * 既有的"非 transient 字段整体 Gson 序列化"模式一致，不需要新的存储文件。
 *
 * 键 = (serverKey, 窗口别名)。serverKey：多人服取服务器 IP（小写），单人存档
 * 取 "singleplayer:<世界名>"（单人同理）。窗口别名 = 窗口标题的 slug（与
 * /wl 命令别名同规则），同名窗口按出现顺序编号：首个不变，第二个起追加 #N，
 * 保存与恢复使用同一 enumeration 顺序，保证跨会话稳定。
 */
public final class WindowPlacementMemory {

	private WindowPlacementMemory() {}

	/** 一次摆位：窗口中心 + 朝向角（度）+ 锚定距离。 */
	public record Placement(double x, double y, double z, double yawDeg, double distance) {}

	public static final char KEY_SEP = '|';
	private static final int COORD_DECIMALS = 3;

	/** 应用别名 slug：小写、非字母数字→下划线、合并、去首尾（与命令面 slugify 同一实现）。 */
	public static String slugify(String s) {
		if(s == null) return "";
		return s.toLowerCase(Locale.ROOT)
			.replaceAll("[^a-z0-9]+", "_")
			.replaceAll("_+", "_")
			.replaceAll("^_+|_+$", "");
	}

	/** 服务器标识。ip 优先（多人）；无 ip 用世界名（单人/集成服）；都缺 → ""（不持久化）。 */
	public static String serverKey(String serverIp, String levelName) {
		if(serverIp != null && !serverIp.isBlank()) {
			return sanitize(serverIp.trim().toLowerCase(Locale.ROOT));
		}
		if(levelName != null && !levelName.isBlank()) {
			return sanitize("singleplayer:" + levelName.trim());
		}
		return "";
	}

	/** 组合存储键；'|' 是保留分隔符，各段内出现即替换为 '_'（IP/别名正常都不含 '|'）。 */
	public static String storageKey(String serverKey, String alias) {
		return sanitize(serverKey) + KEY_SEP + sanitize(alias);
	}

	private static String sanitize(String s) {
		return s.replace(KEY_SEP, '_').replace('\0', '_');
	}

	/** 编码：x,y,z,yaw,dist（Locale.ROOT 定点，跨语言环境稳定）。 */
	public static String encode(Placement p) {
		return String.format(Locale.ROOT, "%." + COORD_DECIMALS + "f,%." + COORD_DECIMALS + "f,%." + COORD_DECIMALS + "f,%." + COORD_DECIMALS + "f,%." + COORD_DECIMALS + "f",
				p.x(), p.y(), p.z(), p.yawDeg(), p.distance());
	}

	/** 解码；null/字段数不符/非数字/NaN/Inf 一律返回 null（脏数据不得污染摆位）。 */
	public static Placement decode(String s) {
		if(s == null || s.isBlank()) return null;
		String[] parts = s.split(",", -1);
		if(parts.length != 5) return null;
		try {
			double[] v = new double[5];
			for(int i = 0; i < 5; i++) {
				v[i] = Double.parseDouble(parts[i].trim());
				if(!Double.isFinite(v[i])) return null;
			}
			return new Placement(v[0], v[1], v[2], v[3], v[4]);
		} catch(NumberFormatException e) {
			return null;
		}
	}

	/** 查询记忆摆位；无记录或脏记录返回 null。 */
	public static Placement lookup(Map<String, String> store, String serverKey, String alias) {
		if(store == null || serverKey.isEmpty() || alias.isEmpty()) return null;
		return decode(store.get(storageKey(serverKey, alias)));
	}

	/** 写入记忆摆位；store 为 null 时静默（例如单人世界名未知时上层传了 null 容器）。 */
	public static void remember(Map<String, String> store, String serverKey, String alias, Placement p) {
		if(store == null || serverKey.isEmpty() || alias.isEmpty() || p == null) return;
		store.put(storageKey(serverKey, alias), encode(p));
	}

	/**
	 * 同名别名编号：输入枚举顺序的原始别名表，输出与之一一对应的存储别名。
	 * 首次出现不变，第二次起 "alias#N"（N=出现序号）。例：[qq, xterm, qq] → [qq, xterm, qq#2]。
	 */
	public static List<String> indexedAliases(List<String> aliases) {
		Map<String, Integer> seen = new HashMap<String, Integer>();
		List<String> out = new ArrayList<String>(aliases.size());
		for(String alias : aliases) {
			int n = seen.merge(alias, 1, Integer::sum);
			out.add(n == 1 ? alias : alias + "#" + n);
		}
		return out;
	}

	/**
	 * 一次性计算 handle → 存储别名 映射（保存与恢复共用同一规则，顺序一致才匹配）。
	 * unknown 标题统一 slug 后同样编号（多个 Unknown 窗口互不覆盖）。
	 */
	public static Map<Long, String> memoryAliasesByHandle(List<Long> handles, List<String> rawTitles) {
		if(handles.size() != rawTitles.size()) {
			throw new IllegalArgumentException("handles/rawTitles size mismatch: " + handles.size() + " vs " + rawTitles.size());
		}
		List<String> slugs = new ArrayList<String>(rawTitles.size());
		for(int i = 0; i < rawTitles.size(); i++) {
			String slug = slugify(rawTitles.get(i));
			slugs.add(slug.isEmpty() ? "window" : slug);
		}
		List<String> indexed = indexedAliases(slugs);
		Map<Long, String> out = new LinkedHashMap<Long, String>();
		for(int i = 0; i < handles.size(); i++) {
			out.put(handles.get(i), indexed.get(i));
		}
		return out;
	}

	/** 按服解析 localItems 生效档：服专用记录优先，否则全局档；一切值经 normalize。 */
	public static String resolveLocalItems(String serverKey, Map<String, String> perServer, String globalDefault) {
		if(serverKey != null && !serverKey.isEmpty() && perServer != null) {
			String v = perServer.get(serverKey);
			if(v != null && !v.isBlank()) {
				return dev.evvie.waylandcraft.localmode.LocalModePolicy.normalizeLocalItems(v);
			}
		}
		return dev.evvie.waylandcraft.localmode.LocalModePolicy.normalizeLocalItems(globalDefault);
	}

	/** "已提示过"标记集合（本地模式教学每服一次）：无副作用成员判定 + 幂等添加。 */
	public static boolean isMarked(List<String> marks, String key) {
		return marks != null && !key.isEmpty() && marks.contains(key);
	}

	public static void mark(List<String> marks, String key) {
		if(marks == null || key.isEmpty() || marks.contains(key)) return;
		marks.add(key);
	}
}
