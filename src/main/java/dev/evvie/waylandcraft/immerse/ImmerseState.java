package dev.evvie.waylandcraft.immerse;

/**
 * ⑩ v1.5.0 沉浸模式状态机（纯逻辑，时钟注入可单测；MC 零依赖）。
 *
 * 状态流（用户需求：进入特效空间专注用窗口；服务端权威反作弊，本地降级纯视觉）：
 *   OFF --toggle(on)--> PENDING --S2C confirmed--> ACTIVE(serverConfirmed=true)
 *                        PENDING --超时 LOCAL_TIMEOUT_MS--> ACTIVE(serverConfirmed=false)
 *                          （vanilla 服无回执：视觉保留但标注 local-only，无隐私承诺）
 *   ACTIVE --toggle(off)--> OFF
 *   ACTIVE --服务端强制退出(S2C active=false)--> OFF
 *
 * 视觉生效规则：PENDING 与 ACTIVE 都算"视觉生效"（本地立即有反馈，不必等服务端）。
 * mode 在 PENDING 期即生效（still|ghost）。
 */
public final class ImmerseState {
	
	/** 沉浸档位：still=他人看到原地不动的我们（服务端冻结）；ghost=他人完全看不到（服务端 vanish） */
	public enum Mode { STILL, GHOST }
	
	enum Phase { OFF, PENDING, ACTIVE }
	
	private Phase phase = Phase.OFF;
	private Mode mode = Mode.STILL;
	private boolean serverConfirmed = false;
	private long phaseEnteredMs = 0;
	
	/** vanilla 服（无服务端 mod）S2C 超时：超过即标记 local-only，视觉保留 */
	public static final long LOCAL_TIMEOUT_MS = 3_000;
	
	// ===== 查询（渲染/mixin 热路径） =====
	
	/** 视觉是否生效（PENDING/ACTIVE 都是——本地立即有反馈）。mixin 热路径读 */
	public boolean isVisualActive() {
		return phase != Phase.OFF;
	}
	
	public boolean isOff() {
		return phase == Phase.OFF;
	}
	
	public Mode mode() {
		return mode;
	}
	
	public String modeName() {
		return mode == Mode.GHOST ? "ghost" : "still";
	}
	
	public boolean serverConfirmed() {
		return serverConfirmed;
	}
	
	// ===== 迁移（时钟注入，可单测） =====
	
	/** 请求开启：OFF/PENDING → PENDING（重复 on 幂等）；ACTIVE 同 mode 幂等，异 mode 换档回 PENDING */
	public void requestOn(Mode requested, long nowMs) {
		if(requested != mode || phase == Phase.OFF) {
			phase = Phase.PENDING;
			mode = requested;
			serverConfirmed = false;
			phaseEnteredMs = nowMs;
		}
	}
	
	/** 请求关闭：任何态 → OFF（幂等） */
	public void requestOff(long nowMs) {
		phase = Phase.OFF;
		serverConfirmed = false;
		phaseEnteredMs = nowMs;
	}
	
	/** 服务端确认（S2C）：PENDING → ACTIVE（active=true）；ACTIVE 期回执刷新 confirmed */
	public void onServerConfirm(boolean active, Mode confirmedMode, long nowMs) {
		if(!active) {
			// 服务端强制退出（受伤/超时/服务端关闭）：无条件落 OFF
			phase = Phase.OFF;
			serverConfirmed = false;
			phaseEnteredMs = nowMs;
			return;
		}
		mode = confirmedMode;
		serverConfirmed = true;
		if(phase == Phase.PENDING) {
			phase = Phase.ACTIVE;
			phaseEnteredMs = nowMs;
		}
	}
	
	/**
	 * PENDING 超时检测（客户端 tick 驱动）：超过 LOCAL_TIMEOUT_MS 未见回执 →
	 * 视觉保留但标记 serverConfirmed=false（vanilla 服 local-only 降级）。
	 * @return true 当本次调用刚发生超时迁移（调用方打一条日志）
	 */
	public boolean tickPendingTimeout(long nowMs) {
		if(phase == Phase.PENDING && nowMs - phaseEnteredMs >= LOCAL_TIMEOUT_MS) {
			phase = Phase.ACTIVE; // 视觉保留，只是没服务端背书
			return true;
		}
		return false;
	}
	
	/** 测试/诊断快照 */
	public String snapshot() {
		return phase + "/" + modeName() + "/confirmed=" + serverConfirmed;
	}
}
