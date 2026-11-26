package dev.evvie.waylandcraft.localmode;

/**
 * 服务端能力 × 显示模式 的纯判定（无 MC 依赖，可单测）。
 *
 * 背景（⑩）：窗口物品的"存在性"锚定在服务端；vanilla 服务端对未知 payload 静默
 * 丢弃、对未知物品 ID 直接拒绝，客户端收不到任何错误。客户端据此有两种世界内
 * 显示形态：
 *  - ITEM：物品驱动显示（标准模式；物品由**服务端**写入背包，客户端零背包写入）。
 *  - LOCAL_DIRECT：本地直显，无物品参与，纯客户端建显示。
 *
 * localItems 设置档（用户显式选择，绝不自动开启）：
 *  - auto（默认）：有 mod=ITEM，无 mod=LOCAL_DIRECT。
 *  - items：无 mod 服也走物品档——由**客户端本地**创建窗口物品（单人/无反作弊服
 *    手感）。反作弊服开启此项可能被判刷物品，风险由用户自担；因此它是
 *    clientItemWriteAllowed 的唯一放行条件，且必须用户显式选择（全局或按服）。
 *  - display：强制 LOCAL_DIRECT（即使服务端有 mod）。
 *
 * 反作弊红线（审计锁死，见 LocalModePolicyTest）：
 *  - 物品类 payload（ServerboundGiveItemsPayload/ServerboundAliveWindowsPayload）
 *    仅在 serverHasMod==true 时发送——严格，不看任何档位。
 *  - 客户端写背包仅在 (!serverHasMod && localItems=="items") 时发生；
 *    serverHasMod==true 时客户端永远零背包写入（物品由服务端权威发放）。
 *  - auto/display 档下任何路径都不触碰背包。
 */
public final class LocalModePolicy {

	public enum WindowDisplayMode { ITEM, LOCAL_DIRECT }

	public static final String LOCAL_ITEMS_AUTO = "auto";
	public static final String LOCAL_ITEMS_ITEMS = "items";
	public static final String LOCAL_ITEMS_DISPLAY = "display";

	private LocalModePolicy() {}

	/** 任意输入归一化为合法档名；未知/空值按默认 auto（宁可保守，不默认放开客户端物品）。 */
	public static String normalizeLocalItems(String value) {
		if(value == null) return LOCAL_ITEMS_AUTO;
		String v = value.trim().toLowerCase(java.util.Locale.ROOT);
		if(v.equals(LOCAL_ITEMS_ITEMS) || v.equals(LOCAL_ITEMS_DISPLAY)) return v;
		return LOCAL_ITEMS_AUTO;
	}

	/**
	 * 显示模式判定。
	 * @param serverHasMod 能力探测结果（canSend 单一信号）
	 * @param localItems   生效档（按服解析后的值）
	 * @param localOverride 运行期强制直显（give 静默失败自愈等；仅影响当前连接，重连重探）
	 */
	public static WindowDisplayMode resolveDisplayMode(boolean serverHasMod, String localItems, boolean localOverride) {
		if(localOverride) return WindowDisplayMode.LOCAL_DIRECT;
		String v = normalizeLocalItems(localItems);
		if(v.equals(LOCAL_ITEMS_DISPLAY)) return WindowDisplayMode.LOCAL_DIRECT;
		if(v.equals(LOCAL_ITEMS_ITEMS)) return WindowDisplayMode.ITEM;
		return serverHasMod ? WindowDisplayMode.ITEM : WindowDisplayMode.LOCAL_DIRECT;
	}

	/** 物品/存活窗口 payload 发送门：严格跟随服务端 mod 存在性，任何档位不得绕过。 */
	public static boolean itemPayloadSendAllowed(boolean serverHasMod) {
		return serverHasMod;
	}

	/**
	 * 客户端本地写背包（放/收窗口物品）门：仅当服务端无 mod **且**用户显式选了
	 * items 档。有 mod 时物品由服务端发放，客户端写入只会制造不同步。
	 */
	public static boolean clientItemWriteAllowed(boolean serverHasMod, String localItems) {
		return !serverHasMod && normalizeLocalItems(localItems).equals(LOCAL_ITEMS_ITEMS);
	}

	/** 依赖服务端 mod 的功能（多人共享、物品持久化命令等）放行门。 */
	public static boolean serverModFeatureAllowed(boolean serverHasMod) {
		return serverHasMod;
	}
}
