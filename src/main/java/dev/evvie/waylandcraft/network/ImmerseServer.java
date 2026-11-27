package dev.evvie.waylandcraft.network;

import java.util.ArrayList;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

import org.jetbrains.annotations.Nullable;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.shared.ShareAclAudit;
import dev.evvie.waylandcraft.utils.ServerWarnThrottle;
import net.fabricmc.fabric.api.entity.event.v1.ServerLivingEntityEvents;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerLifecycleEvents;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerTickEvents;
import net.fabricmc.fabric.api.event.player.AttackBlockCallback;
import net.fabricmc.fabric.api.event.player.AttackEntityCallback;
import net.fabricmc.fabric.api.event.player.UseBlockCallback;
import net.fabricmc.fabric.api.event.player.UseEntityCallback;
import net.fabricmc.fabric.api.event.player.UseItemCallback;
import net.fabricmc.fabric.api.networking.v1.PayloadTypeRegistry;
import net.fabricmc.fabric.api.networking.v1.ServerPlayConnectionEvents;
import net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.util.Mth;
import net.minecraft.world.InteractionResult;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.player.Player;
import net.minecraft.world.phys.Vec3;

/**
 * v1.5.0 沉浸模式服务端权威层 + 反作弊。
 *
 * <p>需求：进入沉浸空间后"没有人可以看到我们（ghost），或只看到一个在原地不动的我们（still）"；
 * 兼容本地（集成服务端）与独立服务端；带反作弊。
 *
 * <p>职责与实现路径（勘查结论见各方法注释，均经 javap 字节码验证，26.1.2）：
 * <ul>
 * <li><b>状态机</b>：per-player {@link ImmersePlayerState}，纯函数决策 {@link #decideToggle}
 *     保证幂等（重复 enable 同 mode = no-op；mode 变更 = 先清旧状态再入新）。</li>
 * <li><b>still 冻结</b>：记录进入时位置/姿态，{@code ServerTickEvents.END_SERVER_TICK}
 *     检测漂移后 {@code setYRot/setXRot + ServerPlayer.teleportTo(x,y,z)} 强制回弹
 *     （teleportTo 字节码：PositionMoveRotation(pos, Vec3.ZERO, yRot, xRot) +
 *     Relative.DELTA∪ROTATION → connection.teleport → 客户端强制回弹 + delta 清零）。</li>
 * <li><b>ghost 隐身</b>：不触碰 ChunkMap 内部——ServerPlayerMixin 注入
 *     {@code ServerPlayer.broadcastToPlayer(observer)} 返回 false 即可：
 *     vanilla {@code ChunkMap$TrackedEntity.updatePlayer}（每 tick 由 ChunkMap.tick 驱动，
 *     另有 move/addEntity 驱动点）在 broadcastToPlayer=false 时走 removePlayer 分支
 *     （seenBy 清理 + 下发 ClientboundRemoveEntitiesPacket），退出后恢复 true 时同一路径
 *     自动重配对。observer 迭代、包下发、恢复全部交给 vanilla 自身生命周期。</li>
 * <li><b>反作弊</b>：受伤强制退出（取消伤害 + 清状态 + 回执 + 审计）；沉浸玩家禁攻击/
 *     放置/交互/物品使用（still 冻结与 ghost 隐身都不应能改动世界——ghost 隐身玩家
 *     若能攻击就是隐身施暴者，故两种模式一并禁止）。</li>
 * <li><b>超时保护</b>：最长 {@link #MAX_DURATION_MS}（30 分钟），到期前 60s 日志提示；
 *     判定为纯函数（时钟由调用方注入，可测）。</li>
 * <li><b>观测</b>：进入/退出/强制退出各 INFO 一行（IMMERSE|op=... 单行竖线分字段，
 *     与 ShareAclAudit 日志规范一致）；每 tick 回弹不落日志。</li>
 * </ul>
 *
 * <p>线程模型：states 为 ConcurrentHashMap，但所有状态写入只发生在服务端线程
 * （payload receiver 经 ctx.server().execute 调度，tick/事件均为服务端线程）；
 * shouldSuppressBroadcast 被 vanilla 每 tick 高频调用，读路径走 isEmpty+get 快速通道。
 */
public final class ImmerseServer {

	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-immerse");

	/** 超时保护：单次沉浸最长 30 分钟 */
	public static final long MAX_DURATION_MS = 30L * 60L * 1000L;

	/** 超时保护：到期前 60s 提示一次 */
	public static final long TIMEOUT_WARN_BEFORE_MS = 60L * 1000L;

	/** still 冻结位置回弹容差（平方距离，约 1mm；客户端浮点抖动不触发发包） */
	static final double FREEZE_EPS_SQ = 1.0e-6;

	/** still 冻结姿态回弹容差（度） */
	static final float FREEZE_ROT_EPS_DEG = 0.05f;

	/** 沉浸模式。STILL 为默认（冻结在原地可见）；GHOST 隐身可自由移动。 */
	public enum ImmerseMode {
		STILL(ImmerseTogglePayload.MODE_STILL),
		GHOST(ImmerseTogglePayload.MODE_GHOST);

		/** wire 值（payload 契约） */
		public final String wire;

		ImmerseMode(String wire) {
			this.wire = wire;
		}
	}

	/** mode 解析结果：rejected=true 表示 wire 值非法（已按 STILL 兜底） */
	public record ModeResolution(ImmerseMode mode, boolean rejected) {}

	/** 状态机动作 */
	public enum ToggleAction {
		/** 新进入沉浸 */
		ENTER,
		/** mode 变更：先清旧状态再入新 */
		SWITCH,
		/** 主动退出 */
		EXIT,
		/** 幂等 no-op（状态不动，仅回执） */
		NOOP
	}

	/**
	 * 状态机决策（纯函数输出）：mode 为本次会话实际生效/回执的 mode，
	 * activeAfter 为决策执行后的激活状态（= 回执 active）。
	 */
	public record ToggleDecision(ToggleAction action, ImmerseMode mode, boolean activeAfter) {}

	/**
	 * per-player 沉浸状态。纯 Java（无 MC 类型），ImmerseServerLogicTest 可直接构造。
	 * 字段在进入时确定后不再变（timeoutWarned 除外，仅服务端线程写）。
	 */
	public static final class ImmersePlayerState {

		public final ImmerseMode mode;
		public final long enterMs;
		/** still 模式冻结锚点（ghost 模式不冻结，锚点仅记录进入位置） */
		public final double frozenX;
		public final double frozenY;
		public final double frozenZ;
		public final float frozenYaw;
		public final float frozenPitch;
		/** 到期前 60s 提示只发一次的标记 */
		public boolean timeoutWarned;

		public ImmersePlayerState(ImmerseMode mode, long enterMs,
				double frozenX, double frozenY, double frozenZ,
				float frozenYaw, float frozenPitch) {
			this.mode = mode;
			this.enterMs = enterMs;
			this.frozenX = frozenX;
			this.frozenY = frozenY;
			this.frozenZ = frozenZ;
			this.frozenYaw = frozenYaw;
			this.frozenPitch = frozenPitch;
			this.timeoutWarned = false;
		}
	}

	/** UUID → 状态。写仅发生在服务端线程；ConcurrentHashMap 支撑 mixin 读路径无锁。 */
	private static final Map<UUID, ImmersePlayerState> states = new ConcurrentHashMap<>();

	/** 总开关（默认开；CLI 由命令侧调用 setEnabled） */
	private static volatile boolean enabled = true;

	/** 会话服务器引用（SERVER_STARTING 捕获，setEnabled(false) 批量强退用） */
	@Nullable
	private static volatile MinecraftServer sessionServer = null;

	private static boolean registered = false;

	private ImmerseServer() {}

	// ==================== 纯函数（ImmerseServerLogicTest 覆盖，无 MC 依赖） ====================

	/**
	 * wire mode 解析：仅精确接受 "still"/"ghost"；
	 * null/空/其他值 → 拒绝标记 + 按 STILL 兜底（契约：拒绝并按 still 处理+回执）。
	 */
	public static ModeResolution resolveMode(@Nullable String raw) {
		if (ImmerseTogglePayload.MODE_STILL.equals(raw)) {
			return new ModeResolution(ImmerseMode.STILL, false);
		}
		if (ImmerseTogglePayload.MODE_GHOST.equals(raw)) {
			return new ModeResolution(ImmerseMode.GHOST, false);
		}
		return new ModeResolution(ImmerseMode.STILL, true);
	}

	/** 超时保护判定（时钟注入，纯函数）：held ≥ 30min 即到期。 */
	public static boolean shouldAutoExit(long enterMs, long nowMs) {
		return nowMs - enterMs >= MAX_DURATION_MS;
	}

	/** 到期前 60s 提示窗口（含 29:40 起点行，不含到期行；到期走 shouldAutoExit）。 */
	public static boolean shouldWarnTimeout(long enterMs, long nowMs) {
		long held = nowMs - enterMs;
		return held >= MAX_DURATION_MS - TIMEOUT_WARN_BEFORE_MS && held < MAX_DURATION_MS;
	}

	/**
	 * 切换状态机（幂等核心）：
	 * <ul>
	 * <li>enable=true：无状态 → ENTER；同 mode → NOOP（重复 enable 幂等）；异 mode → SWITCH。</li>
	 * <li>enable=false：有状态 → EXIT（mode 回执为实际生效过的旧 mode）；无状态 → NOOP（回执 false）。</li>
	 * </ul>
	 */
	public static ToggleDecision decideToggle(@Nullable ImmersePlayerState existing, boolean enable, ImmerseMode requested) {
		if (enable) {
			if (existing == null) {
				return new ToggleDecision(ToggleAction.ENTER, requested, true);
			}
			if (existing.mode == requested) {
				return new ToggleDecision(ToggleAction.NOOP, requested, true);
			}
			return new ToggleDecision(ToggleAction.SWITCH, requested, true);
		}
		if (existing != null) {
			return new ToggleDecision(ToggleAction.EXIT, existing.mode, false);
		}
		return new ToggleDecision(ToggleAction.NOOP, requested, false);
	}

	// ==================== 注册与总开关 ====================

	/**
	 * 注册 payload、C2S receiver 与全部服务端事件。由 WaylandCraftCommon.onInitialize 调用；
	 * 幂等（重复调用只生效一次）。客户端对 S2C 回执的接收处理由客户端渲染侧负责。
	 */
	public static synchronized void register() {
		if (registered) {
			return;
		}
		registered = true;

		PayloadTypeRegistry.serverboundPlay().register(ImmerseTogglePayload.TYPE, ImmerseTogglePayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(ImmerseStatePayload.TYPE, ImmerseStatePayload.CODEC);

		// C2S：netty 线程只做转发，逻辑全部在服务端线程（house runSafely 同款外层兜底：
		// 单个恶意/异常包的任务异常只记 WARN，不炸穿 tick）
		ServerPlayNetworking.registerGlobalReceiver(ImmerseTogglePayload.TYPE, (payload, ctx) ->
			ctx.server().execute(() -> {
				try {
					handleToggle(ctx.player(), payload);
				} catch (Throwable t) {
					LOGGER.warn("[IMMERSE] toggle handler failed for player {} (kept server alive)",
						ctx.player().getUUID(), t);
				}
			}));

		// still 冻结回弹 + 超时保护（house 同款 END_SERVER_TICK：WaylandCraftCommon.java:51 / SharedWindowFrameRelay.java:154）
		ServerTickEvents.END_SERVER_TICK.register(ImmerseServer::onServerTick);

		// 受伤强制退出（反作弊核心）：取消本次伤害本身，立即退出并回执
		ServerLivingEntityEvents.ALLOW_DAMAGE.register((entity, source, amount) -> {
			if (entity instanceof ServerPlayer player && isImmersed(player.getUUID())) {
				forceExit(player, "damage");
				return false;
			}
			return true;
		});

		// 沉浸玩家禁攻击/放置/交互/物品使用（两种模式一并禁止：ghost 隐身者能攻击=隐身施暴）
		AttackBlockCallback.EVENT.register((player, world, hand, pos, direction) ->
			isInteractionBlocked(player) ? InteractionResult.FAIL : InteractionResult.PASS);
		AttackEntityCallback.EVENT.register((player, world, hand, entity, hitResult) ->
			isInteractionBlocked(player) ? InteractionResult.FAIL : InteractionResult.PASS);
		UseBlockCallback.EVENT.register((player, world, hand, hitResult) ->
			isInteractionBlocked(player) ? InteractionResult.FAIL : InteractionResult.PASS);
		UseEntityCallback.EVENT.register((player, world, hand, entity, hitResult) ->
			isInteractionBlocked(player) ? InteractionResult.FAIL : InteractionResult.PASS);
		UseItemCallback.EVENT.register((player, world, hand) ->
			isInteractionBlocked(player) ? InteractionResult.FAIL : InteractionResult.PASS);

		// 断线清理：ghost 状态随实体移除自然消失，只需清 per-player 表
		ServerPlayConnectionEvents.DISCONNECT.register((handler, srv) -> {
			ImmersePlayerState st = states.remove(handler.getPlayer().getUUID());
			if (st != null) {
				LOGGER.info("IMMERSE|op=DISCONNECT_CLEAN player={} mode={}",
					handler.getPlayer().getUUID(), st.mode.wire);
			}
		});

		ServerLifecycleEvents.SERVER_STARTING.register(srv -> sessionServer = srv);
		// 会话结束清 per-player 表（ShareAclAudit.immerse* 计数由
		// WaylandCraftNetworking.clearSessionState 挂 SERVER_STOPPED 统一清零，不在此重复）
		ServerLifecycleEvents.SERVER_STOPPED.register(srv -> {
			sessionServer = null;
			states.clear();
		});

		LOGGER.info("ImmerseServer registered (toggle payload / tick freeze / damage / interactions / disconnect), enabled={}", enabled);
	}

	/**
	 * 总开关（默认 true）。关闭时立刻强制退出全部在沉浸中的玩家（服务器权威；
	 * 客户端会收到各自的 (false, mode, true) 回执）。由命令侧（CLI）调用。
	 */
	public static void setEnabled(boolean value) {
		enabled = value;
		if (!value) {
			MinecraftServer srv = sessionServer;
			if (srv != null) {
				for (UUID id : new ArrayList<>(states.keySet())) {
					ServerPlayer player = srv.getPlayerList().getPlayer(id);
					if (player != null) {
						forceExit(player, "disabled");
					} else {
						states.remove(id);
					}
				}
			}
			LOGGER.info("IMMERSE|op=DISABLED (all immersed players force-exited)");
		} else {
			LOGGER.info("IMMERSE|op=ENABLED");
		}
	}

	public static boolean isEnabled() {
		return enabled;
	}

	/** 是否处于沉浸中（供命令侧/观测用） */
	public static boolean isImmersed(UUID id) {
		return states.containsKey(id);
	}

	/** 当前沉浸 mode 的 wire 值；未沉浸返回 null（供命令侧/观测用） */
	@Nullable
	public static String getActiveMode(UUID id) {
		ImmersePlayerState st = states.get(id);
		return st == null ? null : st.mode.wire;
	}

	/** 当前沉浸玩家快照（供命令侧/观测用） */
	public static Set<UUID> immersedPlayers() {
		return Set.copyOf(states.keySet());
	}

	// ==================== ghost 追踪抑制（ServerPlayerMixin 消费，读路径） ====================

	/**
	 * ghost 模式判定：tracked 玩家沉浸中且为 GHOST 时，对 observer 不可见。
	 *
	 * <p>vanilla 语义（26.1.2 字节码验证）：ChunkMap$TrackedEntity.updatePlayer 中
	 * Entity.broadcastToPlayer(observer)==false 时直接走 removePlayer(observer) 分支
	 * （seenBy 移除 + ServerEntity.removePairing → 下发 ClientboundRemoveEntitiesPacket）；
	 * updatePlayer 由 ChunkMap.tick 每 tick 驱动（另在 move/addEntity 处驱动），
	 * 因此进入后 1 tick 内消失、退出后 1 tick 内自动重配对恢复——observer 迭代、
	 * 包下发、恢复全部由 vanilla 生命周期完成，本层只提供判定。
	 */
	public static boolean shouldSuppressBroadcast(Entity tracked, ServerPlayer observer) {
		if (!enabled || states.isEmpty()) {
			return false;
		}
		if (!(tracked instanceof ServerPlayer player) || observer == player) {
			return false;
		}
		ImmersePlayerState st = states.get(player.getUUID());
		return st != null && st.mode == ImmerseMode.GHOST;
	}

	// ==================== 内部处理（全部服务端线程） ====================

	/** 沉浸玩家交互阻断判定（五个 Fabric 事件共用；快速通道在前） */
	private static boolean isInteractionBlocked(Player player) {
		return enabled && player instanceof ServerPlayer sp && isImmersed(sp.getUUID());
	}

	private static void handleToggle(ServerPlayer player, ImmerseTogglePayload payload) {
		UUID id = player.getUUID();
		ModeResolution res = resolveMode(payload.mode());
		if (res.rejected()) {
			// 契约：非法 mode 拒绝并按 still 处理+回执（warn 限频，防刷屏）
			if (ServerWarnThrottle.allow("immerse-mode-reject:" + id, 5000)) {
				LOGGER.warn("IMMERSE|op=MODE_REJECT player={} raw={}", id, payload.mode());
			}
		}
		if (!enabled) {
			// 总开关关闭：一律回执未激活（状态不可能存在——setEnabled(false) 已批量强退）
			sendState(player, new ImmerseStatePayload(false, res.mode().wire, true));
			return;
		}
		ImmersePlayerState existing = states.get(id);
		ToggleDecision decision = decideToggle(existing, payload.enable(), res.mode());
		switch (decision.action()) {
			case ENTER -> enter(player, decision.mode());
			case SWITCH -> {
				cleanupOldState(existing, player);
				enter(player, decision.mode());
			}
			case EXIT -> exit(player, existing, "user");
			case NOOP -> {
				// 幂等：状态不动，仅回执
			}
		}
		sendState(player, new ImmerseStatePayload(decision.activeAfter(), decision.mode().wire, true));
	}

	/** 进入沉浸：记录锚点 + 审计 + INFO 一行（ghost 无需手动迭代 observer，见 shouldSuppressBroadcast） */
	private static void enter(ServerPlayer player, ImmerseMode mode) {
		ImmersePlayerState st = new ImmersePlayerState(mode, System.currentTimeMillis(),
			player.getX(), player.getY(), player.getZ(), player.getYRot(), player.getXRot());
		states.put(player.getUUID(), st);
		ShareAclAudit.immerseEnter.incrementAndGet();
		LOGGER.info("IMMERSE|op=ENTER player={} mode={} pos=({},{},{})",
			player.getUUID(), mode.wire,
			Math.round(st.frozenX), Math.round(st.frozenY), Math.round(st.frozenZ));
		if (mode == ImmerseMode.STILL) {
			// 进入即停残运动量，避免首 tick 回弹前被物理推移
			player.setDeltaMovement(Vec3.ZERO);
		}
	}

	/** 主动退出：清状态 + INFO 一行（ghost 恢复由 vanilla updatePlayer 自动完成） */
	private static void exit(ServerPlayer player, ImmersePlayerState existing, String reason) {
		states.remove(player.getUUID(), existing);
		LOGGER.info("IMMERSE|op=EXIT player={} mode={} reason={}", player.getUUID(), existing.mode.wire, reason);
	}

	/** mode 变更前半段：清旧状态（旧 ghost 的可见性恢复由 vanilla 自动完成） */
	private static void cleanupOldState(@Nullable ImmersePlayerState existing, ServerPlayer player) {
		if (existing == null) {
			return;
		}
		states.remove(player.getUUID(), existing);
		LOGGER.info("IMMERSE|op=SWITCH_OUT player={} mode={}", player.getUUID(), existing.mode.wire);
	}

	/**
	 * 强制退出（伤害/超时/总开关关闭）：清状态 + 审计 + INFO 一行 + 回执。
	 * 幂等：无状态时直接返回。
	 */
	static void forceExit(ServerPlayer player, String reason) {
		ImmersePlayerState existing = states.remove(player.getUUID());
		if (existing == null) {
			return;
		}
		ShareAclAudit.immerseForceExit.incrementAndGet();
		LOGGER.info("IMMERSE|op=FORCE_EXIT player={} mode={} reason={}",
			player.getUUID(), existing.mode.wire, reason);
		sendState(player, new ImmerseStatePayload(false, existing.mode.wire, true));
	}

	/** 每 tick：超时保护 + still 冻结回弹（ghost 无 per-tick 需求）。空表快速返回。 */
	private static void onServerTick(MinecraftServer srv) {
		if (states.isEmpty()) {
			return;
		}
		long now = System.currentTimeMillis();
		for (Map.Entry<UUID, ImmersePlayerState> entry : states.entrySet()) {
			ServerPlayer player = srv.getPlayerList().getPlayer(entry.getKey());
			if (player == null) {
				// 兜底：断线钩子已清，此处理论上不可达；防陈旧条目泄漏
				states.remove(entry.getKey());
				continue;
			}
			ImmersePlayerState st = entry.getValue();
			if (shouldAutoExit(st.enterMs, now)) {
				forceExit(player, "timeout");
				continue;
			}
			if (!st.timeoutWarned && shouldWarnTimeout(st.enterMs, now)) {
				st.timeoutWarned = true;
				LOGGER.info("IMMERSE|op=TIMEOUT_WARN player={} mode={} remaining<=60s",
					entry.getKey(), st.mode.wire);
			}
			if (st.mode == ImmerseMode.STILL) {
				snapFrozen(player, st);
			}
		}
	}

	/**
	 * still 冻结回弹：漂移超容差才发包（站立不动零流量），每 tick 停服务端运动量
	 * （重力/击退不再累积；回弹包本身带 Vec3.ZERO delta）。
	 */
	private static void snapFrozen(ServerPlayer player, ImmersePlayerState st) {
		double dx = player.getX() - st.frozenX;
		double dy = player.getY() - st.frozenY;
		double dz = player.getZ() - st.frozenZ;
		boolean drifted = dx * dx + dy * dy + dz * dz > FREEZE_EPS_SQ
			|| Math.abs(Mth.wrapDegrees(player.getYRot() - st.frozenYaw)) > FREEZE_ROT_EPS_DEG
			|| Math.abs(Mth.wrapDegrees(player.getXRot() - st.frozenPitch)) > FREEZE_ROT_EPS_DEG;
		player.setDeltaMovement(Vec3.ZERO);
		if (!drifted) {
			return;
		}
		// 先设姿态再 teleportTo：teleportTo 以当前 yRot/xRot 构建 PositionMoveRotation
		player.setYRot(st.frozenYaw);
		player.setXRot(st.frozenPitch);
		player.teleportTo(st.frozenX, st.frozenY, st.frozenZ);
	}

	/** 回执下发：canSend 过滤（未装 mod 的客户端静默跳过，house 同款） */
	private static void sendState(ServerPlayer player, ImmerseStatePayload payload) {
		if (!ServerPlayNetworking.canSend(player, ImmerseStatePayload.TYPE)) {
			return;
		}
		ServerPlayNetworking.send(player, payload);
	}

}
