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
				// 只播放已知窗口的声音（窗口列表里有才播放，避免脏包）
				if(remoteWindows.containsKey(payload.windowHandle())) {
					wlc.audioPlaybackManager.enqueue(
						payload.windowHandle(), payload.sampleRate(), payload.channels(), payload.pcmData());
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
		
		// 处理客户端断开：清空渲染器的待解码队列、解码线程池与纹理（避免资源泄漏）
		ClientPlayConnectionEvents.DISCONNECT.register((handler, client) -> {
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc != null) {
				if(wlc.remoteWindowRenderer != null) {
					wlc.remoteWindowRenderer.clear();
				}
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
		
		// 节流：JPEG 解码已移到后台工作线程（updateTexture 只入队），主线程不再被解码卡住。
		// 保留 50ms 节流匹配 20fps，防止入队/状态更新过频造成无意义堆积。
		long now = System.currentTimeMillis();
		if(now - info.lastImageProcessTime < 50) {
			return;
		}
		info.lastImageProcessTime = now;
		
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
				payload.imageData()
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
	 */
	public static void requestWindowRegister(long windowHandle, String windowTitle) {
		SharedWindowRegisterPayload payload = new SharedWindowRegisterPayload(windowHandle, windowTitle);
		ClientPlayNetworking.send(payload);
		
		LOGGER.info("Requested window registration: 0x{} - {}",
			Long.toHexString(windowHandle), windowTitle);
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
	 * 发送交互事件
	 */
	public static void sendInteraction(long windowHandle, SharedWindowInteractionPayload.InteractionType type,
			double x, double y, int button, int key) {
		UUID senderUUID = Minecraft.getInstance().player != null ? Minecraft.getInstance().player.getUUID() : null;
		SharedWindowInteractionPayload payload = new SharedWindowInteractionPayload(
			windowHandle, type, x, y, button, key, senderUUID
		);
		ClientPlayNetworking.send(payload);
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
