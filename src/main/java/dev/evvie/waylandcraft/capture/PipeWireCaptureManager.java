package dev.evvie.waylandcraft.capture;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.render.SharedWindowDisplay;
import dev.evvie.waylandcraft.shared.RemoteWindowRenderer;
import net.minecraft.client.Camera;
import net.minecraft.client.Minecraft;

/**
 * 桌面窗口捕获管理器（XDG Desktop Portal ScreenCast + PipeWire）
 * 
 * 通过 native bridge 调用 portal_capture_start/frame/stop：
 * - start: 弹出系统选择对话框，返回 PipeWire 节点 ID
 * - frame: 返回 [width(4), height(4), rgba...] 或空数组
 * - stop:  停止捕获
 * 
 * 捕获到的帧通过 {@link RemoteWindowRenderer} 上传为纹理，
 * 并用 {@link SharedWindowDisplay} 渲染到游戏世界中。
 */
public class PipeWireCaptureManager {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-capture");
	
	/** 当前激活的捕获会话（同一时间只允许一个） */
	private CaptureSession activeSession = null;
	
	/**
	 * 启动一次 Portal ScreenCast 捕获（会弹出系统选择对话框）。
	 * 
	 * @return 捕获会话；失败或用户取消时返回 null
	 */
	public CaptureSession startCapture() {
		if(activeSession != null) {
			LOGGER.warn("Capture session already active, stopping it first");
			activeSession.stop();
			activeSession = null;
		}
		
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc == null || wlc.bridge == null) {
			LOGGER.error("Cannot start capture: WaylandCraft not initialized");
			return null;
		}
		
		byte[] result = wlc.bridge.portalCaptureStart();
		if(result == null || result.length == 0) {
			LOGGER.warn("portalCaptureStart returned empty result (canceled or timed out)");
			return null;
		}
		
		String text = new String(result, StandardCharsets.UTF_8);
		if(!text.startsWith("ok:")) {
			LOGGER.error("portalCaptureStart failed: {}", text);
			return null;
		}
		
		long nodeId;
		try {
			nodeId = Long.parseLong(text.substring(3).trim());
		} catch(NumberFormatException e) {
			LOGGER.error("Cannot parse node id from '{}'", text);
			return null;
		}
		
		LOGGER.info("Portal capture started, PipeWire node={}", nodeId);
		activeSession = new CaptureSession(nodeId);
		return activeSession;
	}
	
	/**
	 * 每帧调用：拉取当前捕获帧并更新纹理。
	 * 由 {@link WaylandCraft#update()} 驱动。
	 */
	public void tick() {
		if(activeSession != null) {
			activeSession.tick();
		}
	}
	
	/**
	 * 停止当前捕获会话（如有）。
	 */
	public void stopCapture() {
		if(activeSession != null) {
			activeSession.stop();
			activeSession = null;
		}
	}
	
	/**
	 * 当前激活的捕获会话（可能为 null）。
	 */
	public CaptureSession getActiveSession() {
		return activeSession;
	}
	/**
	 * 当前是否有激活的捕获会话
	 */
	public boolean isCapturing() {
		return activeSession != null && activeSession.isRunning();
	}
	
	/**
	 * 一次 Portal ScreenCast 捕获会话。
	 * 会话建立后注册一个虚拟显示，把捕获帧渲染到游戏世界。
	 */
	public static class CaptureSession {
		
		/** PipeWire 节点 ID（由 native 侧分配） */
		private final long nodeId;
		
		private volatile boolean running = true;
		
		/** 虚拟显示句柄（用 nodeId 作为唯一 handle） */
		private final long displayHandle;
		
		private SharedWindowDisplay display = null;
		private RemoteWindowRenderer renderer = null;
		
		CaptureSession(long nodeId) {
			this.nodeId = nodeId;
			// 用 nodeId 做句柄；为避免与真实窗口句柄冲突，取反一个固定偏移
			this.displayHandle = nodeId ^ 0x7000000000000000L;
		}
		
		/**
		 * 注册一个虚拟 toplevel，把捕获窗口显示到游戏世界。
		 * 
		 * @param title 窗口标题
		 */
		public void registerToplevel(String title) {
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc == null) return;
			
			renderer = wlc.remoteWindowRenderer;
			display = new SharedWindowDisplay(displayHandle, title, "Portal Capture", renderer);
			
			// 定位到玩家面前
			Camera camera = Minecraft.getInstance().gameRenderer.getMainCamera();
			if(camera != null) {
				display.anchorToCamera(camera);
			}
			
			wlc.sharedDisplays.add(display);
			LOGGER.info("Registered portal capture display: '{}'", title);
		}
		
		/**
		 * 拉取最新一帧并更新纹理（需在渲染线程调用）
		 */
		public void tick() {
			if(!running) return;
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc == null || wlc.bridge == null || renderer == null || display == null) return;
			
			byte[] frame = wlc.bridge.portalCaptureFrame();
			if(frame == null || frame.length < 8) return;
			
			ByteBuffer buf = ByteBuffer.wrap(frame).order(ByteOrder.LITTLE_ENDIAN);
			int width = buf.getInt();
			int height = buf.getInt();
			if(width <= 0 || height <= 0 || width > 16384 || height > 16384) return;
			
			byte[] rgba = new byte[frame.length - 8];
			System.arraycopy(frame, 8, rgba, 0, rgba.length);
			
			// 更新纹理并同步显示尺寸
			renderer.updateTextureRGBA(displayHandle, width, height, rgba);
			display.updateSize(width, height);
		}
		
		/**
		 * 停止捕获并清理显示
		 */
		public void stop() {
			if(!running) return;
			running = false;
			
			WaylandCraft wlc = WaylandCraft.instance;
			if(wlc != null) {
				if(wlc.bridge != null) {
					wlc.bridge.portalCaptureStop();
				}
				if(renderer != null) {
					renderer.destroyTexture(displayHandle);
				}
				if(display != null) {
					wlc.sharedDisplays.remove(display);
				}
			}
			LOGGER.info("Portal capture stopped");
		}
		
		public boolean isRunning() {
			return running;
		}
		
		public long getNodeId() {
			return nodeId;
		}
		
		public long getDisplayHandle() {
			return displayHandle;
		}
	}
}
