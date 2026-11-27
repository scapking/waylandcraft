package dev.evvie.waylandcraft.immerse;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.network.ImmerseStatePayload;
import dev.evvie.waylandcraft.network.ImmerseTogglePayload;
import dev.evvie.waylandcraft.utils.WcLogging;
import net.fabricmc.api.EnvType;
import net.fabricmc.api.Environment;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayConnectionEvents;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking;
import net.minecraft.client.Minecraft;

/**
 * ⑩ v1.5.0 沉浸模式客户端管理器（单例）。
 *
 * 职责：状态机托管（ImmerseState 纯类）、C2S 意图发送 / S2C 确认接收、
 * PENDING 超时降级（vanilla 服 local-only）、断线清理。
 * 渲染热路径（mixin/穹顶）只读 {@link #isVisualActive()}（volatile 布尔镜像，
 * 零开销护栏）。
 *
 * 服务端权威语义：视觉本地立即生效（PENDING），隐私效果（still 冻结 / ghost
 * 消失）以服务端确认（S2C serverConfirmed）为准；vanilla 服 3s 超时 →
 * local-only（纯特效，无隐私承诺，HUD/CLI 如实标注）。
 */
@Environment(EnvType.CLIENT)
public final class ImmerseManager {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-immerse");
	
	private static final ImmerseManager INSTANCE = new ImmerseManager();
	
	/** 渲染/mixin 热路径镜像（状态机每次迁移后同步写） */
	private volatile boolean visualActive = false;
	private final ImmerseState state = new ImmerseState();
	private boolean registered = false;
	private boolean firstEnableLogged = false;
	
	private ImmerseManager() {}
	
	public static ImmerseManager instance() {
		return INSTANCE;
	}
	
	/** 渲染/mixin 热路径：视觉是否生效（穹顶/跳世界短路读这里） */
	public static boolean isVisualActive() {
		return INSTANCE.visualActive;
	}
	
	/** 注册 S2C 接收器与断线清理（WaylandCraft client init 调用一次） */
	public void register() {
		if(registered) return;
		registered = true;
		
		ClientPlayNetworking.registerGlobalReceiver(ImmerseStatePayload.TYPE, (payload, ctx) -> {
			ctx.client().execute(() -> {
				boolean before = INSTANCE.state.isVisualActive();
				ImmerseState.Mode mode = parseMode(payload.mode());
				INSTANCE.state.onServerConfirm(payload.active(), mode, System.currentTimeMillis());
				syncMirror();
				boolean now = INSTANCE.state.isVisualActive();
				if(before && !now) {
					LOGGER.info("Immerse ended (server, mode={})", INSTANCE.state.modeName());
				} else if(!before && now) {
					LOGGER.info("Immerse active (server confirmed, mode={}, local-only={})",
						INSTANCE.state.modeName(), !payload.serverConfirmed());
				}
			});
		});
		
		ClientPlayConnectionEvents.DISCONNECT.register((handler, server) -> {
			// 断线：状态整体回落（服务端状态由服务端自己清理；本地视觉立即退出）
			if(!INSTANCE.state.isOff()) {
				INSTANCE.state.requestOff(System.currentTimeMillis());
				syncMirror();
				LOGGER.info("Immerse reset on disconnect");
			}
		});
	}
	
	/**
	 * CLI 入口：切换沉浸。返回给命令层的反馈文本（null=静默）。
	 */
	public String toggle(boolean enable, String modeRaw) {
		long now = System.currentTimeMillis();
		if(enable) {
			ImmerseState.Mode mode = parseMode(modeRaw);
			if(mode == null) {
				return "§c✘ immerse 档位只能是 still|ghost§r";
			}
			boolean wasActive = state.isVisualActive();
			state.requestOn(mode, now);
			syncMirror();
			sendToggle(true, state.modeName());
			if(!wasActive) {
				if(!firstEnableLogged) {
					firstEnableLogged = true;
					LOGGER.info("Immerse first enable (mode={}) — awaiting server confirm (3s timeout → local-only)",
						state.modeName());
				} else {
					LOGGER.info("Immerse enable (mode={})", state.modeName());
				}
			}
			return "§a✔ 沉浸模式开启§f（" + state.modeName() + "）§7— 等待服务端确认…3s 无回执将按本地特效运行§r";
		}
		else {
			if(state.isOff()) {
				return "§7沉浸模式未开启§r";
			}
			state.requestOff(now);
			syncMirror();
			sendToggle(false, state.modeName());
			LOGGER.info("Immerse disabled by user");
			return "§a✔ 沉浸模式已关闭§r";
		}
	}
	
	/** CLI status：单行状态快照 */
	public String statusLine() {
		String base = state.isOff()
			? "§7off§r"
			: "§a" + state.modeName() + "§r" + (state.serverConfirmed()
				? " §7(服务端已确认)§r"
				: " §e(等待服务端确认/local-only)§r");
		return base;
	}
	
	/** 客户端 tick 驱动：PENDING 超时降级（vanilla 服 → local-only，视觉保留） */
	public void clientTick() {
		if(state.isOff()) return;
		if(state.tickPendingTimeout(System.currentTimeMillis())) {
			syncMirror();
			LOGGER.info("Immerse server confirm timeout — running local-only (visual kept, no privacy promise)");
		}
	}
	
	private void sendToggle(boolean enable, String mode) {
		try {
			Minecraft mc = Minecraft.getInstance();
			if(mc != null && mc.getConnection() != null) {
				ClientPlayNetworking.send(new ImmerseTogglePayload(enable, mode));
			}
		} catch(Exception e) {
			if(WcLogging.verbose()) {
				LOGGER.debug("Immerse toggle send failed (single-player/local?): {}", e.toString());
			}
		}
	}
	
	private void syncMirror() {
		visualActive = state.isVisualActive();
	}
	
	/** null = 非法档位（调用方报错）；大小写不敏感 */
	private static ImmerseState.Mode parseMode(String raw) {
		if(raw == null || raw.isBlank() || "still".equalsIgnoreCase(raw)) return ImmerseState.Mode.STILL;
		if("ghost".equalsIgnoreCase(raw)) return ImmerseState.Mode.GHOST;
		return null;
	}
}
