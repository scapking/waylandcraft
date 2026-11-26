package dev.evvie.waylandcraft.network;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.render.SharedWindowDisplay;
import dev.evvie.waylandcraft.shared.RemoteWindowRenderer;
import dev.evvie.waylandcraft.shared.WindowPermission;
import dev.evvie.waylandcraft.shared.WindowShareManager;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayConnectionEvents;
import dev.evvie.waylandcraft.network.PermissionResponsePayload;
import net.minecraft.client.Minecraft;
import net.minecraft.world.phys.Vec3;

/**
 * 客户端窗口接收处理器
 * 处理从服务器接收的共享窗口数据
 */
public class SharedWindowClientHandler {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-client-handler");
	
	// 远程窗口缓存: windowHandle -> WindowInfo
	private static final Map<Long, WindowInfo> remoteWindows = new ConcurrentHashMap<>();
	
	// ⑦ 窗口列表到达前暂存的音频包（列表广播与音频包到达顺序不保证）：
	// TTL 15s、每窗口 100 包封顶，超时/超量丢最旧。
	private record HeldAudio(SharedWindowAudioPayload payload, long arrivalMs) {}
	private static final Map<Long, java.util.ArrayDeque<HeldAudio>> pendingAudio = new ConcurrentHashMap<>();
	private static final long PENDING_AUDIO_TTL_MS = 15_000;
	private static final int PENDING_AUDIO_MAX_PACKETS = 100;
	private static volatile long lastPendingAudioLogMs = 0;
	
	/** ⑥ /wl status：viewer 侧已知共享窗口句柄集合（只读视图） */
	public static java.util.Collection<Long> getKnownWindowHandles() {
		return remoteWindows.keySet();
	}
	
		/** ⑦ 音频范围衰减用：窗口在世界中的 pivot（未知返回 null，播放侧保持上次增益） */
	public static Vec3 getWindowPivot(long windowHandle) {
		WindowInfo info = remoteWindows.get(windowHandle);
		return info != null ? info.pivot() : null;
	}
	
	private static void holdAudio(SharedWindowAudioPayload payload) {
		java.util.ArrayDeque<HeldAudio> q =
			pendingAudio.computeIfAbsent(payload.windowHandle(), h -> new java.util.ArrayDeque<>());
		int size;
		long now = System.currentTimeMillis();
		synchronized(q) {
			q.addLast(new HeldAudio(payload, now));
			while(q.size() > PENDING_AUDIO_MAX_PACKETS) q.removeFirst();
			size = q.size();
		}
		// 首次暂存提示一次（10s 节流），便于定位"无声"是否为时序问题
		if(now - lastPendingAudioLogMs > 10_000) {
			lastPendingAudioLogMs = now;
			LOGGER.info("[audio] holding packets for not-yet-listed window 0x{} ({} pending) — list update not arrived yet",
				Long.toHexString(payload.windowHandle()), size);
		}
	}
	
	/** 列表更新后补放暂存音频；过期整批丢弃 */
	private static void flushPendingAudio() {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.audioPlaybackManager == null) return;
		long now = System.currentTimeMillis();
		for(Long handle : new ArrayList<>(pendingAudio.keySet())) {
			if(!remoteWindows.containsKey(handle)) {
				java.util.ArrayDeque<HeldAudio> q = pendingAudio.get(handle);
				if(q != null) {
					synchronized(q) {
						while(!q.isEmpty() && now - q.peekFirst().arrivalMs() > PENDING_AUDIO_TTL_MS) q.removeFirst();
						if(q.isEmpty()) pendingAudio.remove(handle);
					}
				}
				continue;
			}
			java.util.ArrayDeque<HeldAudio> q = pendingAudio.remove(handle);
			if(q == null) continue;
			int flushed = 0;
			synchronized(q) {
				HeldAudio held;
				while((held = q.pollFirst()) != null) {
					if(now - held.arrivalMs() > PENDING_AUDIO_TTL_MS) continue;
					SharedWindowAudioPayload p = held.payload();
					wlc.audioPlaybackManager.enqueue(p.windowHandle(), p.sampleRate(), p.channels(), p.pcmData(), p.captureTimestampMs());
					flushed++;
				}
			}
			LOGGER.info("[audio] flushed {} held packets for window 0x{}", flushed, Long.toHexString(handle));
		}
	}
	
	/**
	 * 注册客户端网络处理器
	 */
	public static void register() {
		// 处理窗口列表更新
		ClientPlayNetworking.registerGlobalReceiver(SharedWindowListPayload.TYPE, (payload, ctx) -> {
			ctx.client().execute(() -> {
				handleWindowListUpdate(payload);
			});
		});
		
		// 处理窗口状态更新
		ClientPlayNetworking.registerGlobalReceiver(SharedWindowUpdatePayload.TYPE, (payload, ctx) -> {
			ctx.client().execute(() -> {
				handleWindowStateUpdate(payload);
			});
		});
		
		// 处理窗口图像更新
		ClientPlayNetworking.registerGlobalReceiver(SharedWindowImagePayload.TYPE, (payload, ctx) -> {
			ctx.client().execute(() -> {
				handleWindowImageUpdate(payload);
			});
		});
		
		// 处理共享窗口音频（OpenAL 流式播放；netty 线程收到 → enqueue 内部转主线程）
		ClientPlayNetworking.registerGlobalReceiver(SharedWindowAudioPayload.TYPE, (payload, ctx) -> {
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc != null && wlc.audioPlaybackManager != null) {
				// ⑦ 此前"窗口列表未到就丢音频包"是多人收不到声音的根因之一
				// （音频包往往先于列表广播到达）。现在暂存，列表更新时补放。
				if(remoteWindows.containsKey(payload.windowHandle())) {
					wlc.audioPlaybackManager.enqueue(
						payload.windowHandle(), payload.sampleRate(), payload.channels(),
						payload.pcmData(), payload.captureTimestampMs());
				}
				else {
					holdAudio(payload);
				}
			}
		});
		
		// 处理权限更新
		ClientPlayNetworking.registerGlobalReceiver(SharedWindowPermissionPayload.TYPE, (payload, ctx) -> {
			ctx.client().execute(() -> {
				handlePermissionUpdate(payload);
			});
		});
		
		// 处理权限命令响应
		ClientPlayNetworking.registerGlobalReceiver(PermissionResponsePayload.TYPE, (payload, ctx) -> {
			ctx.client().execute(() -> {
				var mc = net.minecraft.client.Minecraft.getInstance();
				if (mc.player != null) {
					mc.player.sendSystemMessage(net.minecraft.network.chat.Component.literal(payload.message()));
				}
			});
		});

		// 处理交互事件（窗口所有者侧）：服务器把玩家对共享窗口的操作转发给 owner，
		// owner 把事件注入到真实窗口。X11 窗口走 XTest 注入（wayland 注入后续接入）。
		ClientPlayNetworking.registerGlobalReceiver(SharedWindowInteractionPayload.TYPE, (payload, ctx) -> {
			ctx.client().execute(() -> {
				handleInteraction(payload);
			});
		});
		
		// 处理客户端断开：清空渲染器的待解码队列、解码线程池与纹理（避免资源泄漏）
		ClientPlayConnectionEvents.DISCONNECT.register((handler, client) -> {
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc != null) {
				if(wlc.remoteWindowRenderer != null) {
					wlc.remoteWindowRenderer.clear();
				}
				pendingAudio.clear(); // ⑦ 断线清空暂存音频
				if(wlc.audioPlaybackManager != null) {
					// OpenAL 必须在主线程释放
					client.execute(() -> wlc.audioPlaybackManager.closeAll());
				}
			}
		});
		
		LOGGER.info("Shared window client handlers registered");
	}
	
	/**
	 * 处理窗口列表更新
	 */
	private static void handleWindowListUpdate(SharedWindowListPayload payload) {
		List<SharedWindowListPayload.WindowInfo> windowList = payload.windows();
		
		LOGGER.info("Received window list update: {} windows", windowList.size());
		
		// 清理已注销/消失窗口的纹理与解码队列（避免残留纹理与队列泄漏）
		WaylandCraft instance = WaylandCraft.instance;
		if(instance != null && instance.remoteWindowRenderer != null) {
			for(Long handle : remoteWindows.keySet()) {
				boolean stillListed = false;
				for(SharedWindowListPayload.WindowInfo w : windowList) {
					if(w.windowHandle() == handle) {
						stillListed = true;
						break;
					}
				}
				if(!stillListed) {
					instance.remoteWindowRenderer.destroyTexture(handle);
					instance.remoteWindowRenderer.removeWindow(handle); // ⑦ 丢 playout 流与缓冲
					pendingAudio.remove(handle);
					if(instance.audioPlaybackManager != null) {
						instance.audioPlaybackManager.close(handle);
					}
				}
			}
		}
		
		// 更新远程窗口缓存
		remoteWindows.clear();
		for(SharedWindowListPayload.WindowInfo info : windowList) {
			remoteWindows.put(info.windowHandle(), new WindowInfo(
				info.windowHandle(),
				info.ownerUUID(),
				info.windowTitle(),
				info.permission()
			));
		}
		
		// 更新WaylandCraft的共享窗口显示
		updateSharedDisplays();
		
		// ⑦ 列表就绪后补放此前因"窗口未知"被暂存的音频包
		flushPendingAudio();
	}
	
	/**
	 * 处理窗口状态更新
	 */
	private static void handleWindowStateUpdate(SharedWindowUpdatePayload payload) {
		WindowInfo info = remoteWindows.get(payload.windowHandle());
		if(info == null) {
			LOGGER.warn("Received state update for unknown window 0x{}", 
				Long.toHexString(payload.windowHandle()));
			return;
		}
		
		// 更新窗口状态
		info.updateState(payload.x(), payload.y(), payload.width(), payload.height(), payload.visible());
		
		// 更新对应的SharedWindowDisplay
		updateSharedDisplay(info);
	}
	
	/**
	 * 处理窗口图像更新
	 */
	private static void handleWindowImageUpdate(SharedWindowImagePayload payload) {
		WindowInfo info = remoteWindows.get(payload.windowHandle());
		if(info == null) {
			LOGGER.warn("[CLIENT] received image for unknown window 0x{} (remoteWindows has {} entries)",
				Long.toHexString(payload.windowHandle()), remoteWindows.size());
			return;
		}
		
		// 节流：仅在低延迟模式（D=0）保留 50ms 入队节流匹配 20fps。
		// ⑦ 预缓冲模式下绝不丢包——每一帧都是缓冲深度的来源，由 playout 时钟
		// 决定何时解码/显示；updateTexture 只 append 环形队列，主线程成本 O(1)。
		if(RemoteWindowRenderer.playoutDelayMs() <= 0) {
			long now = System.currentTimeMillis();
			if(now - info.lastImageProcessTime < 50) {
				return;
			}
			info.lastImageProcessTime = now;
		}
		
		// 更新图像数据
		info.updateImage(payload.imageData(), payload.width(), payload.height());
		
		// 同步更新窗口尺寸（payload.width/height是原始framebuffer尺寸，用于世界空间渲染）
		info.updateState(info.x(), info.y(), payload.width(), payload.height(), info.visible());
		
		// 记录 framebuffer 内容偏移（xoff/yoff），用于接收端 bufOffset 对齐
		info.setBufferOffset(payload.x(), payload.y());
		
		// 更新窗口变换（pivot/normal/down）
		info.updateTransform(
			payload.pivotX(), payload.pivotY(), payload.pivotZ(),
			payload.normalX(), payload.normalY(), payload.normalZ(),
			payload.downX(), payload.downY(), payload.downZ()
		);
		
		// 同步视觉缩放与 geometry 尺寸（与本地 WindowDisplay 渲染一致）
		info.updateViewScale(payload.viewScale());
		info.updateGeometrySize(payload.geometryWidth(), payload.geometryHeight());
		
		// 同步发送端 pixelsPerBlock（接收端用它渲染，保证世界尺寸与发送端一致）
		info.setSenderPixelsPerBlock(payload.senderPixelsPerBlock());
		
		// 更新RemoteWindowRenderer
		WaylandCraft instance = WaylandCraft.instance;
		if(instance != null && instance.remoteWindowRenderer != null) {
			instance.remoteWindowRenderer.updateTexture(
				payload.windowHandle(),
				payload.x(), payload.y(),
				payload.width(), payload.height(),
				payload.imageData(),
				payload.captureTimestampMs(), payload.frameSeq()
			);
		}
		
		// 更新SharedWindowDisplay位置
		updateSharedDisplay(info);
	}
	
	/**
	 * 处理权限更新
	 */
	private static void handlePermissionUpdate(SharedWindowPermissionPayload payload) {
		WindowInfo info = remoteWindows.get(payload.windowHandle());
		if(info == null) {
			return;
		}
		
		// 更新权限
		info.setPermission(payload.permission());
		
		LOGGER.info("Permission updated for window 0x{}: {}",
			Long.toHexString(payload.windowHandle()), payload.permission());
	}
	
	/**
	 * 更新共享窗口显示
	 */
	private static void updateSharedDisplays() {
		WaylandCraft instance = WaylandCraft.instance;
		if(instance == null) return;
		
		// 清理旧的显示
		instance.sharedDisplays.clear();
		
		// 创建新的显示
		for(WindowInfo info : remoteWindows.values()) {
			if(info.permission().hasPermission(WindowPermission.VIEW)) {
				SharedWindowDisplay display = new SharedWindowDisplay(
					info.windowHandle(),
					info.title(),
					info.ownerName(),
					instance.remoteWindowRenderer
				);
				display.setPermission(info.permission());
				instance.sharedDisplays.add(display);
			}
		}
		
		LOGGER.info("Updated shared displays: {} windows", instance.sharedDisplays.size());
	}
	
	/**
	 * 更新单个共享窗口显示
	 */
	private static void updateSharedDisplay(WindowInfo info) {
		WaylandCraft instance = WaylandCraft.instance;
		if(instance == null) return;
		
		// 查找对应的显示
		for(SharedWindowDisplay display : instance.sharedDisplays) {
			if(display.getWindowHandle() == info.windowHandle()) {
				display.updatePosition(info.x(), info.y());
				display.updateSize(info.width(), info.height());
				display.setBufferOffset(info.bufferXOff(), info.bufferYOff());
				display.setVisible(info.visible());
				// 传递窗口变换
				display.setTransform(info.pivot(), info.normal(), info.down());
				// 传递视觉缩放与 geometry 尺寸（与本地 WindowDisplay 一致）
				display.setViewScale(info.viewScale());
				display.setGeometrySize(info.geometryWidth(), info.geometryHeight());
				// 传递发送端 PPB（保证世界尺寸一致）
				display.setSenderPixelsPerBlock(info.senderPixelsPerBlock());
				break;
			}
		}
	}
	
	/**
	 * 请求注册共享窗口
	 * @param targetPlayer 定向共享目标玩家名（null/空 = 公开共享给所有人）
	 */
	public static void requestWindowRegister(long windowHandle, String windowTitle, String targetPlayer) {
		SharedWindowRegisterPayload payload = new SharedWindowRegisterPayload(windowHandle, windowTitle, targetPlayer == null ? "" : targetPlayer);
		ClientPlayNetworking.send(payload);
		
		LOGGER.info("Requested window registration: 0x{} - {} (targeted={})",
			Long.toHexString(windowHandle), windowTitle, targetPlayer != null && !targetPlayer.isBlank());
	}

	/**
	 * 请求注册共享窗口（公开共享给所有人）
	 */
	public static void requestWindowRegister(long windowHandle, String windowTitle) {
		requestWindowRegister(windowHandle, windowTitle, null);
	}
	
	/**
	 * 请求注销共享窗口
	 */
	public static void requestWindowUnregister(long windowHandle) {
		SharedWindowUnregisterPayload payload = new SharedWindowUnregisterPayload(windowHandle);
		ClientPlayNetworking.send(payload);
		LOGGER.info("Requested window unregistration: 0x{}", Long.toHexString(windowHandle));
	}
	
	/**
	 * 发送窗口所有者授权管理命令（grant / revoke / list）
	 */
	public static void sendWindowPermCommand(long windowHandle, byte action, String targetName) {
		SharedWindowPermCommandPayload payload = new SharedWindowPermCommandPayload(windowHandle, action, targetName);
		ClientPlayNetworking.send(payload);
		LOGGER.info("Sent window perm command: action={} target={} window=0x{}",
			action, targetName, Long.toHexString(windowHandle));
	}

	/**
	 * 发送交互事件。
	 * P1-2：必须在 play 连接存在时发送——断线清理/重连间隙调用时
	 * ClientPlayNetworking.send 会抛 IllegalStateException（未注册通道的包在
	 * Fabric 0.11x 后对 null connection 直接抛错），这里以 no-op 方式安全丢弃。
	 */
	public static void sendInteraction(long windowHandle, SharedWindowInteractionPayload.InteractionType type,
			double x, double y, int button, int key) {
		if(Minecraft.getInstance().getConnection() == null) return;
		UUID senderUUID = Minecraft.getInstance().player != null ? Minecraft.getInstance().player.getUUID() : null;
		SharedWindowInteractionPayload payload = new SharedWindowInteractionPayload(
			windowHandle, type, x, y, button, key, senderUUID
		);
		ClientPlayNetworking.send(payload);
	}

	/**
	 * P1-8 纯函数（可单测）：把不可信的窗口内坐标钳进 [0, max]。
	 * NaN/±Infinity → 0；max ≤ 0（尺寸未知的竞态）→ 0，绝不放行窗口外坐标。
	 */
	static double clampToWindow(double v, int max) {
		if(Double.isNaN(v) || Double.isInfinite(v)) return 0;
		if(max <= 0) return 0;
		return Math.min(Math.max(v, 0), max);
	}

	/**
	 * 窗口所有者侧：把远程玩家对共享窗口的操作注入到真实窗口。
	 * 当前只支持 X11 窗口（XTest 注入）；wayland 窗口注入后续接入。
	 *
	 * 坐标约定：payload.x/y 是窗口内像素坐标（0..geometryW/H，原始尺寸），
	 * 注入时换算到屏幕根坐标（窗口根坐标 + 偏移）。
	 */
	private static void handleInteraction(SharedWindowInteractionPayload payload) {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.windowShareManager == null) {
			return;
		}

		WindowShareManager.ShareState state = wlc.windowShareManager.getShareState(payload.windowHandle());
		if(state == null) {
			return; // 不是正在共享的窗口（可能已停止）
		}

		// 目前只注入 X11 窗口；wayland 窗口的 wl 指针注入待后续接入
		if(state.source != WindowShareManager.ShareState.Source.X11) {
			return;
		}

		String display = state.x11Display;
		long xid = state.x11Xid;
		// P1-8（坐标钳位）：payload.x/y 是"窗口内像素坐标"，但来源是远端玩家，不可信。
		// 越界/NaN/负值经 x11RootX+x 直接变成屏幕根坐标——恶意 payload 可以把全局 X
		// 指针拽到窗口外（其他应用、终端、桌面）再点击。钳进共享窗口矩形内。
		int winW = state.x11Width;
		int winH = state.x11Height;
		double cx = clampToWindow(payload.x(), winW);
		double cy = clampToWindow(payload.y(), winH);
		int rootX = state.x11RootX + (int) Math.round(cx);
		int rootY = state.x11RootY + (int) Math.round(cy);

		switch(payload.interactionType()) {
			case MOUSE_MOVE -> {
				dev.evvie.waylandcraft.utils.X11Interaction.injectPointerMotion(display, rootX, rootY);
			}
			case MOUSE_CLICK -> {
				dev.evvie.waylandcraft.utils.X11Interaction.injectPointerMotion(display, rootX, rootY);
				dev.evvie.waylandcraft.utils.X11Interaction.injectButton(display, payload.button(), true);
			}
			case MOUSE_RELEASE -> {
				dev.evvie.waylandcraft.utils.X11Interaction.injectButton(display, payload.button(), false);
			}
			case KEY_PRESS -> {
				// P1-8（按键越权）：XTest 键事件落在"当前输入焦点窗口"。注入前必须把
				// 焦点移到被共享窗口（raise + XSetInputFocus + 回读验证），否则 INTERACT
				// 等于 owner 桌面级键盘注入（焦点在终端时按键直进终端）。验证失败拒绝注入。
				if(dev.evvie.waylandcraft.utils.X11Interaction.raiseAndFocusWindow(display, xid)) {
					dev.evvie.waylandcraft.utils.X11Interaction.injectKey(display, payload.key(), true);
				}
			}
			case KEY_RELEASE -> {
				if(dev.evvie.waylandcraft.utils.X11Interaction.raiseAndFocusWindow(display, xid)) {
					dev.evvie.waylandcraft.utils.X11Interaction.injectKey(display, payload.key(), false);
				}
			}
			case SCROLL -> {
				// 编码：低16位 = scrollX*100（符号位保留），高16位 = scrollY*100
				int data = payload.button();
				double scrollX = (short) (data & 0xFFFF) / 100.0;
				double scrollY = (short) ((data >> 16) & 0xFFFF) / 100.0;
				dev.evvie.waylandcraft.utils.X11Interaction.injectPointerMotion(display, rootX, rootY);
				dev.evvie.waylandcraft.utils.X11Interaction.injectScroll(display, scrollX, scrollY);
			}
			default -> {}
		}
	}
	
	/**
	 * 获取远程窗口信息
	 */
	public static WindowInfo getRemoteWindow(long windowHandle) {
		return remoteWindows.get(windowHandle);
	}
	
	/**
	 * 获取所有远程窗口
	 */
	public static List<WindowInfo> getAllRemoteWindows() {
		return new ArrayList<>(remoteWindows.values());
	}
	
	/**
	 * 窗口信息内部类
	 */
	public static class WindowInfo {
		private final long windowHandle;
		private final UUID ownerUUID;
		private final String title;
		private WindowPermission permission;
		
		private int x, y;
		private int width, height;
		private boolean visible = true;
		
		// framebuffer 内容偏移（图像 payload 的 x/y 承载），用于接收端 bufOffset 对齐
		private int bufferXOff;
		private int bufferYOff;
		
		// 发送端 WindowDisplay 的视觉缩放倍数与 geometry 尺寸（本地渲染一致）
		private double viewScale = 1.0;
		private int geometryWidth;
		private int geometryHeight;
		
		// 发送端自己的 pixelsPerBlock（0=未收到）
		private int senderPixelsPerBlock = 0;
		
		private byte[] imageData;
		private int imageWidth, imageHeight;
		
		private long lastImageProcessTime = 0;   // 图像处理节流时间戳
		
		private Vec3 pivot = new Vec3(0, 0, 0);
		private Vec3 normal = new Vec3(0, 0, 1);
		private Vec3 down = new Vec3(0, -1, 0);
		
		public WindowInfo(long windowHandle, UUID ownerUUID, String title, WindowPermission permission) {
			this.windowHandle = windowHandle;
			this.ownerUUID = ownerUUID;
			this.title = title;
			this.permission = permission;
		}
		
		public long windowHandle() { return windowHandle; }
		public UUID ownerUUID() { return ownerUUID; }
		public String title() { return title; }
		public String ownerName() {
			var player = net.minecraft.client.Minecraft.getInstance().level;
			if(player != null) {
				var p = player.getPlayerByUUID(ownerUUID);
				if(p != null) return p.getName().getString();
			}
			return ownerUUID.toString().substring(0, 8);
		}
		public WindowPermission permission() { return permission; }
		public int x() { return x; }
		public int y() { return y; }
		public int width() { return width; }
		public int height() { return height; }
		public boolean visible() { return visible; }
		public byte[] imageData() { return imageData; }
		public int imageWidth() { return imageWidth; }
		public int imageHeight() { return imageHeight; }
		
		public Vec3 pivot() { return pivot; }
		public Vec3 normal() { return normal; }
		public Vec3 down() { return down; }
		public int bufferXOff() { return bufferXOff; }
		public int bufferYOff() { return bufferYOff; }
		public double viewScale() { return viewScale; }
		public int geometryWidth() { return geometryWidth; }
		public int geometryHeight() { return geometryHeight; }
		public int senderPixelsPerBlock() { return senderPixelsPerBlock; }
		
		public void setPermission(WindowPermission permission) {
			this.permission = permission;
		}
		
		public void setBufferOffset(int xoff, int yoff) {
			this.bufferXOff = xoff;
			this.bufferYOff = yoff;
		}
		
		public void updateViewScale(double viewScale) {
			this.viewScale = viewScale;
		}
		
		public void setSenderPixelsPerBlock(int ppb) {
			this.senderPixelsPerBlock = ppb;
		}
		
		public void updateGeometrySize(int width, int height) {
			this.geometryWidth = width;
			this.geometryHeight = height;
		}
		
		public void updateState(int x, int y, int width, int height, boolean visible) {
			this.x = x;
			this.y = y;
			this.width = width;
			this.height = height;
			this.visible = visible;
		}
		
		public void updateImage(byte[] imageData, int width, int height) {
			this.imageData = imageData;
			this.imageWidth = width;
			this.imageHeight = height;
		}
		
		public void updateTransform(double pivotX, double pivotY, double pivotZ,
				double normalX, double normalY, double normalZ,
				double downX, double downY, double downZ) {
			this.pivot = new Vec3(pivotX, pivotY, pivotZ);
			this.normal = new Vec3(normalX, normalY, normalZ);
			this.down = new Vec3(downX, downY, downZ);
		}
	}
}
