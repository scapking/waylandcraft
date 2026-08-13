package dev.evvie.waylandcraft.shared;

import java.awt.image.BufferedImage;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.IntBuffer;
import java.util.zip.Deflater;

import javax.imageio.IIOImage;
import javax.imageio.ImageIO;
import javax.imageio.ImageWriteParam;
import javax.imageio.ImageWriter;
import javax.imageio.stream.ImageOutputStream;
import javax.imageio.stream.MemoryCacheImageOutputStream;

import org.jetbrains.annotations.Nullable;
import org.lwjgl.opengl.GL11;
import org.lwjgl.opengl.GL15;
import org.lwjgl.opengl.GL21;
import org.lwjgl.opengl.GL30;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import com.mojang.blaze3d.buffers.GpuBuffer;
import com.mojang.blaze3d.systems.RenderSystem;
import com.mojang.blaze3d.textures.GpuTexture;

import dev.evvie.waylandcraft.render.WindowFramebuffer;

/**
 * 图像捕获和压缩模块（优化版）
 * 
 * 优化内容：
 * 1. PBO双缓冲异步回读（消除GPU→CPU同步阻塞）
 * 2. GPU侧缩放（glBlitFramebuffer，跳过CPU scaleImage）
 * 3. 直接RGBA→JPEG编码（跳过BufferedImage中间层）
 * 4. 像素差异检测（跳过无变化帧）
 */
public class ImageCapture {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-image-capture");
	
	// 默认JPEG质量
	private static final float DEFAULT_JPEG_QUALITY = 0.7f;
	
	// 默认缩放比例
	private static final float DEFAULT_SCALE = 0.5f;
	
	// 最大图像尺寸
	private static final int MAX_WIDTH = 1920;
	private static final int MAX_HEIGHT = 1080;
	
	// PBO双缓冲（按窗口句柄隔离 — 多窗口交替捕获时，若共享同一套 PBO，
	// 异步阶段 map 到的会是另一个窗口写入的数据，导致画面串扰：
	// 表现为"共享多个窗口时只显示最新的"。每窗口独立 PBO/seed 状态后互不影响。）
	private static final java.util.Map<Long, PboState> pboStates = new java.util.concurrent.ConcurrentHashMap<>();
	
	// PBO 连续失败计数（按窗口）：Mesa/EGL（xwayland-satellite + wayland）下
	// glGenBuffers 可能持续返回 0。连续失败超过阈值后永久降级 sync read，
	// 避免每帧都重复 glGenBuffers + 刷屏告警（v0.2.30）。
	private static final int PBO_MAX_CONSECUTIVE_FAILURES = 3;
	private static final java.util.Map<Long, Integer> pboFailures = new java.util.concurrent.ConcurrentHashMap<>();
	// GPU缩放用临时FBO+纹理（按窗口句柄隔离 — 避免多窗口尺寸不同时每帧重建 FBO 卡顿）
	private static final java.util.Map<Long, ScaleState> scaleStates = new java.util.concurrent.ConcurrentHashMap<>();

	/**
	 * 每窗口独立的 PBO 双缓冲状态
	 */
	private static class PboState {
		int[] ids = null;
		int index = 0;
		int width = 0;
		int height = 0;
		// 0=PBO刚分配未seed, 1=pbo[0]已seed, 2=pbo[1]已seed两步都已就绪可读
		int seedStage = 0;

		void cleanup() {
			if(ids != null) {
				IntBuffer buf = IntBuffer.wrap(ids);
				int[] alive = new int[ids.length];
				int n = 0;
				while(buf.hasRemaining()) {
					int id = buf.get();
					if(id != 0 && GL15.glIsBuffer(id)) {
						alive[n++] = id;
					}
				}
				if(n > 0) {
					IntBuffer aliveBuf = IntBuffer.wrap(alive, 0, n);
					GL15.glDeleteBuffers(aliveBuf);
				}
				ids = null;
			}
			width = 0;
			height = 0;
			seedStage = 0;
			index = 0;
		}
	}

	/**
	 * 每窗口独立的 GPU 缩放 FBO 状态
	 */
	private static class ScaleState {
		int fbo = 0;
		int tex = 0;
		int width = 0;
		int height = 0;

		void cleanup() {
			if(fbo != 0) {
				GL30.glDeleteFramebuffers(fbo);
				fbo = 0;
			}
			if(tex != 0) {
				GL11.glDeleteTextures(tex);
				tex = 0;
			}
			width = 0;
			height = 0;
		}
	}

	// 可复用的ByteBuffer（非PBO回退路径；同步使用，串行执行下无串扰，保持全局）
	private static ByteBuffer reusableBuffer = null;
	private static int lastBufferWidth = 0;
	private static int lastBufferHeight = 0;
	
	// 像素差异检测（per-window：windowHandle -> lastRawFrame）
	// 之前是 static 单缓存，多个窗口共享时会互相覆盖基准帧，导致 diff 误判
	private static final java.util.Map<Long, byte[]> lastRawFrames = new java.util.concurrent.ConcurrentHashMap<>();
	
	/**
	 * 从WindowFramebuffer捕获图像（主入口，带全部优化）
	 */
	@Nullable
	public static byte[] captureFromFramebuffer(WindowFramebuffer framebuffer) {
		return captureFromFramebuffer(0L, framebuffer, DEFAULT_SCALE, DEFAULT_JPEG_QUALITY);
	}
	
	/**
	 * 从WindowFramebuffer捕获图像（优化版，兼容旧签名 — 无名窗口用 handle=0）
	 */
	@Nullable
	public static byte[] captureFromFramebuffer(WindowFramebuffer framebuffer, float scale, float quality) {
		return captureFromFramebuffer(0L, framebuffer, scale, quality);
	}
	
	/**
	 * 从WindowFramebuffer捕获图像（优化版）
	 * 
	 * 流程：
	 * 1. GPU侧缩放（glBlitFramebuffer）
	 * 2. PBO异步回读
	 * 3. 直接RGBA→JPEG编码
	 * 
	 * @param windowHandle 窗口句柄（PBO/缩放FBO 按窗口隔离，多窗口互不串扰）
	 */
	@Nullable
	public static byte[] captureFromFramebuffer(long windowHandle, WindowFramebuffer framebuffer, float scale, float quality) {
		return captureFromFramebufferInternal(windowHandle, framebuffer, scale, quality, false);
	}
	
	/**
	 * 强制 JPEG 捕获（v0.2.32）：与 captureFromFramebuffer 相同，但透明像素
	 * 混合到黑色背景后一律走 JPEG 编码，绝不走 PNG。
	 * 
	 * 用途：JPEG 大小保护降级。原实现里窗口含透明像素时走 PNG（无损，
	 * quality 参数无效），降级重编码前后字节数完全相同 → 降到阶梯底仍超限
	 * → 所有帧被 DROP（用户实测 1139174 -> 1139174 恒定）。强制 JPEG 后
	 * quality 才能真正生效，降级才有效。
	 */
	@Nullable
	public static byte[] captureFromFramebufferJpeg(long windowHandle, WindowFramebuffer framebuffer, float scale, float quality) {
		return captureFromFramebufferInternal(windowHandle, framebuffer, scale, quality, true);
	}
	
	@Nullable
	private static byte[] captureFromFramebufferInternal(long windowHandle, WindowFramebuffer framebuffer, float scale, float quality, boolean forceJpeg) {
		if(!framebuffer.isValid()) {
			return null;
		}
		
		var target = framebuffer.getRenderTarget();
		if(target == null) {
			return null;
		}
		
		int srcW = framebuffer.getWidth();
		int srcH = framebuffer.getHeight();
		
		if(srcW <= 0 || srcH <= 0) {
			return null;
		}
		
		var colorTex = target.getColorTexture();
		if(colorTex == null || colorTex.isClosed()) {
			return null;
		}
		
		int readFbo = 0;
		try {
			int glTexId = ((com.mojang.blaze3d.opengl.GlTexture) colorTex).glId();
			
			// 计算缩放后尺寸
			int dstW = Math.max(1, (int)(srcW * scale));
			int dstH = Math.max(1, (int)(srcH * scale));
			dstW = Math.min(dstW, MAX_WIDTH);
			dstH = Math.min(dstH, MAX_HEIGHT);
			
			// Step 1: GPU侧缩放 — glBlitFramebuffer
			readFbo = GL30.glGenFramebuffers();
			GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, readFbo);
			GL30.glFramebufferTexture2D(GL30.GL_READ_FRAMEBUFFER, GL30.GL_COLOR_ATTACHMENT0, GL11.GL_TEXTURE_2D, glTexId, 0);
			
			int readStatus = GL30.glCheckFramebufferStatus(GL30.GL_READ_FRAMEBUFFER);
			if(readStatus != GL30.GL_FRAMEBUFFER_COMPLETE) {
				LOGGER.error("Source FBO incomplete: 0x{}", Integer.toHexString(readStatus));
				return null;
			}
			
			// 确保缩放FBO存在且尺寸正确（按窗口隔离）
			ensureScaleFbo(windowHandle, dstW, dstH);
			
			// 关键：ensureScaleFbo 重建时会 glBindFramebuffer(GL_FRAMEBUFFER, 0)，
			// 把 GL_READ_FRAMEBUFFER 也解绑成 0（默认FBO=屏幕）。
			// 必须在 blit 前重新绑定源 FBO，否则 blit 会从屏幕读取导致画面错误。
			GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, readFbo);
			
			// Blit: 源FBO → 缩放FBO（GPU做缩放，零CPU开销）
			ScaleState scaleState = scaleStates.get(windowHandle);
			int scaleFboId = (scaleState != null) ? scaleState.fbo : 0;
			GL30.glBindFramebuffer(GL30.GL_DRAW_FRAMEBUFFER, scaleFboId);
			GL30.glBlitFramebuffer(
				0, 0, srcW, srcH,        // 源矩形
				0, 0, dstW, dstH,        // 目标矩形
				GL11.GL_COLOR_BUFFER_BIT,
				GL30.GL_LINEAR            // 双线性插值
			);
			
			// 检查blit错误
			int blitError = GL11.glGetError();
			if(blitError != GL11.GL_NO_ERROR) {
				LOGGER.error("glBlitFramebuffer error: 0x{}", Integer.toHexString(blitError));
				return null;
			}
			
			// Step 2: PBO异步回读（按窗口隔离）
			ByteBuffer pixelData = readPixelsViaPbo(windowHandle, dstW, dstH);
			
			if(pixelData == null) {
				return null;
			}
			
			// Step 3: 直接RGBA→JPEG编码（跳过BufferedImage中间层）
			return compressToJpegDirect(pixelData, dstW, dstH, quality, true, forceJpeg);
			
		} catch(Exception e) {
			LOGGER.error("Failed to capture from framebuffer", e);
			return null;
		} finally {
			// 任何路径都要清干净：解绑 + 删临时 FBO
			GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, 0);
			GL30.glBindFramebuffer(GL30.GL_DRAW_FRAMEBUFFER, 0);
			GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, 0);
			if(readFbo != 0) {
				GL30.glDeleteFramebuffers(readFbo);
			}
		}
	}
	
	/**
	 * 从WindowFramebuffer捕获原始RGBA数据（用于diff检测）
	 * 
	 * @param windowHandle 窗口句柄（缩放FBO按窗口隔离，多窗口互不重建/串扰）
	 */
	@Nullable
	public static byte[] captureFromFramebufferRaw(long windowHandle, WindowFramebuffer framebuffer, float scale) {
		if(!framebuffer.isValid()) return null;
		
		var target = framebuffer.getRenderTarget();
		if(target == null) return null;
		
		int srcW = framebuffer.getWidth();
		int srcH = framebuffer.getHeight();
		if(srcW <= 0 || srcH <= 0) return null;
		
		var colorTex = target.getColorTexture();
		if(colorTex == null || colorTex.isClosed()) return null;
		
		int readFbo = 0;
		try {
			int glTexId = ((com.mojang.blaze3d.opengl.GlTexture) colorTex).glId();
			
			int dstW = Math.max(1, (int)(srcW * scale));
			int dstH = Math.max(1, (int)(srcH * scale));
			dstW = Math.min(dstW, MAX_WIDTH);
			dstH = Math.min(dstH, MAX_HEIGHT);
			
			// 源FBO
			readFbo = GL30.glGenFramebuffers();
			GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, readFbo);
			GL30.glFramebufferTexture2D(GL30.GL_READ_FRAMEBUFFER, GL30.GL_COLOR_ATTACHMENT0, GL11.GL_TEXTURE_2D, glTexId, 0);
			
			// 检查源FBO完整性：若附加纹理失败（非GL_TEXTURE_2D等），blit 会静默失败，
			// 导致 scaleFbo 残留旧内容 → diff 永远判定"无变化"→ 永不发帧。
			int readStatus = GL30.glCheckFramebufferStatus(GL30.GL_READ_FRAMEBUFFER);
			if(readStatus != GL30.GL_FRAMEBUFFER_COMPLETE) {
				LOGGER.error("Raw capture: source FBO incomplete: 0x{} (falling back to no-diff)", Integer.toHexString(readStatus));
				return null;
			}
			
			// GPU缩放（按窗口隔离）
			ensureScaleFbo(windowHandle, dstW, dstH);
			// ensureScaleFbo 重建后会把 READ 解绑成 0，必须重新绑定
			GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, readFbo);
			ScaleState scaleState = scaleStates.get(windowHandle);
			int scaleFboId = (scaleState != null) ? scaleState.fbo : 0;
			GL30.glBindFramebuffer(GL30.GL_DRAW_FRAMEBUFFER, scaleFboId);
			GL30.glBlitFramebuffer(0, 0, srcW, srcH, 0, 0, dstW, dstH, GL11.GL_COLOR_BUFFER_BIT, GL30.GL_LINEAR);
			
			// 同步读取（raw模式不用PBO，避免延迟）
			int needed = dstW * dstH * 4;
			if(reusableBuffer == null || lastBufferWidth != dstW || lastBufferHeight != dstH) {
				reusableBuffer = ByteBuffer.allocateDirect(needed);
				lastBufferWidth = dstW;
				lastBufferHeight = dstH;
			} else {
				reusableBuffer.clear();
			}
			
			// 从缩放FBO读取
			// 关键：MC 26.x 渲染器在 pass 之间会把 GL_READ_BUFFER 设成 GL_NONE，
			// 必须显式指向 COLOR_ATTACHMENT0，否则 glReadPixels 报 GL_INVALID_OPERATION(0x502)。
			// 同时解绑 PIXEL_PACK_BUFFER，避免残留 PBO 导致客户端指针被当成 offset。
			GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, scaleFboId);
			GL30.glReadBuffer(GL30.GL_COLOR_ATTACHMENT0);
			GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, 0);
			GL11.glReadPixels(0, 0, dstW, dstH, GL11.GL_RGBA, GL11.GL_UNSIGNED_BYTE, reusableBuffer);
			
			int readErr = GL11.glGetError();
			if(readErr != GL11.GL_NO_ERROR) {
				LOGGER.error("Raw capture glReadPixels error: 0x{}", Integer.toHexString(readErr));
				return null;
			}
			
			// 转为byte[]
			byte[] result = new byte[needed];
			reusableBuffer.rewind();
			reusableBuffer.get(result);
			return result;
			
		} catch(Exception e) {
			LOGGER.error("Failed to capture raw frame", e);
			return null;
		} finally {
			GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, 0);
			GL30.glBindFramebuffer(GL30.GL_DRAW_FRAMEBUFFER, 0);
			GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, 0);
			if(readFbo != 0) {
				GL30.glDeleteFramebuffers(readFbo);
			}
		}
	}
	
	/**
	 * 检测像素帧是否有显著变化
	 * 采样1/16像素，计算变化百分比
	 * 
	 * @param windowHandle 窗口句柄（diff 基准帧按窗口隔离）
	 * @param currentFrame 当前帧RGBA数据
	 * @param threshold 变化阈值（0.0-1.0），默认0.02（2%）
	 * @return true if significant change detected
	 */
	public static boolean hasSignificantChange(long windowHandle, byte[] currentFrame, float threshold) {
		byte[] last = lastRawFrames.get(windowHandle);
		if(last == null || last.length != currentFrame.length) {
			lastRawFrames.put(windowHandle, currentFrame.clone());
			return true;
		}
		
		int totalPixels = currentFrame.length / 4;
		int sampleStep = 4; // 采样间隔，1/16像素
		int sampled = 0;
		int changed = 0;
		
		for(int i = 0; i < currentFrame.length; i += 4 * sampleStep) {
			sampled++;
			// 比较RGB（跳过Alpha）
			if(currentFrame[i] != last[i] ||
			   currentFrame[i+1] != last[i+1] ||
			   currentFrame[i+2] != last[i+2]) {
				changed++;
			}
		}
		
		float changeRatio = sampled > 0 ? (float)changed / sampled : 1.0f;
		
		// 更新缓存
		lastRawFrames.put(windowHandle, currentFrame.clone());
		
		return changeRatio > threshold;
	}
	
	/**
	 * 清除指定窗口的 diff 基准帧（stopSharing 时调用）
	 */
	public static void clearDiffCache(long windowHandle) {
		lastRawFrames.remove(windowHandle);
	}
	
	/**
	 * 清除所有 diff 基准帧（断线时调用）
	 */
	public static void clearAllDiffCaches() {
		lastRawFrames.clear();
	}
	
	/**
	 * 通过PBO异步读取像素数据
	 * 双缓冲：帧N写入PBO[A]，同时读取PBO[B]的上一帧数据
	 * 
	 * 首帧特殊处理：刚分配的两个 PBO 都未写有效数据，直接走 sync 路径
	 * 并把结果同步写到一个 PBO 里，作为后续异步读的种子数据，
	 * 避免把"未初始化显存"当成有效帧返回导致花屏/黑屏。
	 */
	@Nullable
	private static ByteBuffer readPixelsViaPbo(long windowHandle, int width, int height) {
		int dataSize = width * height * 4;
		
		// 永久降级检查：该窗口已连续失败过阈值次数，直接走 sync，不再尝试 PBO
		Integer failCount = pboFailures.get(windowHandle);
		if(failCount != null && failCount >= PBO_MAX_CONSECUTIVE_FAILURES) {
			return readPixelsSync(windowHandle, width, height);
		}
		
		PboState state = pboStates.computeIfAbsent(windowHandle, k -> new PboState());
		
		// 初始化PBO（首次或尺寸变化时）
		if(state.ids == null || state.width != width || state.height != height) {
			state.cleanup();
			state.ids = new int[2];
			IntBuffer pboBuf = IntBuffer.wrap(state.ids);
			GL15.glGenBuffers(pboBuf);
			
			// Mesa/EGL 下 glGenBuffers 可能失败返回 0 —— 检测到无效 ID 直接降级 sync；
			// 连续失败 PBO_MAX_CONSECUTIVE_FAILURES 次后永久降级，不再每帧重试。
			if(state.ids[0] == 0 || state.ids[1] == 0) {
				int failures = pboFailures.merge(windowHandle, 1, Integer::sum);
				if(failures >= PBO_MAX_CONSECUTIVE_FAILURES) {
					LOGGER.warn("PBO disabled for window 0x{} after {} consecutive failures, using sync read permanently",
						Long.toHexString(windowHandle), failures);
				} else {
					LOGGER.warn("glGenBuffers returned invalid PBO ids ({}), falling back to sync read (failure {}/{})",
						state.ids[0] + "," + state.ids[1], failures, PBO_MAX_CONSECUTIVE_FAILURES);
				}
				state.ids = null;
				return readPixelsSync(windowHandle, width, height);
			}
			
			boolean pboAllocOk = true;
			for(int i = 0; i < 2; i++) {
				GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, state.ids[i]);
				GL15.glBufferData(GL21.GL_PIXEL_PACK_BUFFER, dataSize, GL15.GL_STREAM_READ);
				int err = GL11.glGetError();
				if(err != GL11.GL_NO_ERROR) {
					LOGGER.warn("PBO alloc error: 0x{} (falling back to sync read)", Integer.toHexString(err));
					pboAllocOk = false;
					break;
				}
			}
			GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, 0);
			if(!pboAllocOk) {
				int failures = pboFailures.merge(windowHandle, 1, Integer::sum);
				if(failures >= PBO_MAX_CONSECUTIVE_FAILURES) {
					LOGGER.warn("PBO disabled for window 0x{} after {} consecutive failures, using sync read permanently",
						Long.toHexString(windowHandle), failures);
				}
				state.cleanup();
				return readPixelsSync(windowHandle, width, height);
			}
			
			// PBO 初始化成功：清除失败计数（环境可能已恢复）
			pboFailures.remove(windowHandle);
			state.width = width;
			state.height = height;
			// index = 1 让首次 mapIndex=0 必然走 sync（map 未初始化 PBO 拿到全 0/乱码）
			state.index = 1;
			state.seedStage = 0;
			LOGGER.debug("Initialized PBOs for window 0x{}: {}x{} ({} bytes each)", Long.toHexString(windowHandle), width, height, dataSize);
		}
		
		// 首两次访问（seedStage 0→1→2）一定走 sync——sync 路径已读当前帧，
		// 顺势把内容 DMA 到本帧的 PBO，让下一帧 map 那个 PBO 时拿到刚写的有效数据。
		if(state.seedStage < 2) {
			int seedPbo = 1 - state.index;          // 本帧要写入的 PBO（DMA 当前帧进去）
			ByteBuffer current = readPixelsSync(windowHandle, width, height);
			if(current == null) {
				GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, 0);
				GL30.glBindFramebuffer(GL30.GL_DRAW_FRAMEBUFFER, 0);
				return null;
			}
			// 把当前帧 DMA 到 seedPbo（驱动驱动填满，后续 map 同一个不会失败）
			ScaleState scale = scaleStates.get(windowHandle);
			int scaleFboId = (scale != null) ? scale.fbo : 0;
			GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, state.ids[seedPbo]);
			GL15.glBufferData(GL21.GL_PIXEL_PACK_BUFFER, dataSize, GL15.GL_STREAM_READ);
			GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, scaleFboId);
			GL30.glReadBuffer(GL30.GL_COLOR_ATTACHMENT0);
			GL11.glReadPixels(0, 0, width, height, GL11.GL_RGBA, GL11.GL_UNSIGNED_BYTE, 0L);
			GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, 0);
			GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, 0);
			
			// 移动索引到下一帧（让 mapIndex = 上一帧，刚好是我们刚 seed 的那个）
			state.index = seedPbo;
			state.seedStage++;
			return current;
		}
		
		int readIndex = state.index;        // 当前帧写入这个PBO
		int mapIndex = 1 - state.index;     // 读取上一帧的PBO
		state.index = 1 - state.index;         // 交替
		
		// 将当前帧异步DMA到PBO[readIndex]
		GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, state.ids[readIndex]);
		// 重新分配buffer确保尺寸匹配
		GL15.glBufferData(GL21.GL_PIXEL_PACK_BUFFER, dataSize, GL15.GL_STREAM_READ);
		// 从缩放FBO读取到PBO（本窗口的 FBO）
		ScaleState scale = scaleStates.get(windowHandle);
		int scaleFboId = (scale != null) ? scale.fbo : 0;
		GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, scaleFboId);
		GL30.glReadBuffer(GL30.GL_COLOR_ATTACHMENT0);
		GL11.glReadPixels(0, 0, width, height, GL11.GL_RGBA, GL11.GL_UNSIGNED_BYTE, 0L);
		GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, 0);
		GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, 0);
		
		// 映射PBO[mapIndex]（本窗口上一帧的数据）
		GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, state.ids[mapIndex]);
		ByteBuffer mapped = GL15.glMapBuffer(GL21.GL_PIXEL_PACK_BUFFER, GL15.GL_READ_ONLY);
		
		if(mapped == null) {
			// 首帧或映射失败 — 回退到同步读取
			GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, 0);
			LOGGER.debug("PBO map returned null (first frame?), falling back to sync read");
			return readPixelsSync(windowHandle, width, height);
		}
		
		// 复制数据出来
		ByteBuffer result = ByteBuffer.allocateDirect(dataSize);
		result.put(mapped);
		result.rewind();
		
		GL15.glUnmapBuffer(GL21.GL_PIXEL_PACK_BUFFER);
		GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, 0);
		
		return result;
	}
	
	/**
	 * 同步读取像素（PBO回退路径）
	 * 
	 * 关键修复（0x502 = GL_INVALID_OPERATION 根因）：
	 * 1. MC 26.x 渲染器在 pass 之间会把 GL_READ_BUFFER 设成 GL_NONE，
	 *    必须显式 glReadBuffer(GL_COLOR_ATTACHMENT0) 再读。
	 * 2. 若 PIXEL_PACK_BUFFER 残留绑定，客户端指针会被当成 PBO offset → 0x502。
	 *    读取前必须解绑。
	 * 3. 读取后把 GL_READ_BUFFER 恢复为 GL_NONE（MC 的常用状态），避免破坏 MC 渲染。
	 */
	private static ByteBuffer readPixelsSync(long windowHandle, int width, int height) {
		int needed = width * height * 4;
		if(reusableBuffer == null || lastBufferWidth != width || lastBufferHeight != height) {
			reusableBuffer = ByteBuffer.allocateDirect(needed);
			lastBufferWidth = width;
			lastBufferHeight = height;
		} else {
			reusableBuffer.clear();
		}
		
		ScaleState scale = scaleStates.get(windowHandle);
		int scaleFboId = (scale != null) ? scale.fbo : 0;
		
		// 防止残留 PBO 绑定把客户端指针解释成 offset
		GL15.glBindBuffer(GL21.GL_PIXEL_PACK_BUFFER, 0);
		GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, scaleFboId);
		GL30.glReadBuffer(GL30.GL_COLOR_ATTACHMENT0);
		GL11.glReadPixels(0, 0, width, height, GL11.GL_RGBA, GL11.GL_UNSIGNED_BYTE, reusableBuffer);
		
		int glError = GL11.glGetError();
		// 恢复 MC 常用的 read buffer 状态（GL_NONE / 默认值），避免污染后续渲染
		GL30.glReadBuffer(GL11.GL_NONE);
		GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, 0);
		
		if(glError != GL11.GL_NO_ERROR) {
			LOGGER.error("glReadPixels error: 0x{}", Integer.toHexString(glError));
			return null;
		}
		
		reusableBuffer.rewind();
		return reusableBuffer;
	}
	
	/**
	 * 确保GPU缩放FBO存在且尺寸正确（按窗口句柄隔离）
	 */
	private static void ensureScaleFbo(long windowHandle, int width, int height) {
		ScaleState state = scaleStates.computeIfAbsent(windowHandle, k -> new ScaleState());
		if(state.fbo != 0 && state.width == width && state.height == height) {
			return; // 已存在且尺寸匹配
		}
		
		// 清理旧资源
		state.cleanup();
		
		// 创建缩放纹理
		state.tex = GL11.glGenTextures();
		GL11.glBindTexture(GL11.GL_TEXTURE_2D, state.tex);
		GL11.glTexImage2D(GL11.GL_TEXTURE_2D, 0, GL11.GL_RGBA8, width, height, 0, GL11.GL_RGBA, GL11.GL_UNSIGNED_BYTE, (ByteBuffer) null);
		GL11.glTexParameteri(GL11.GL_TEXTURE_2D, GL11.GL_TEXTURE_MIN_FILTER, GL11.GL_LINEAR);
		GL11.glTexParameteri(GL11.GL_TEXTURE_2D, GL11.GL_TEXTURE_MAG_FILTER, GL11.GL_LINEAR);
		GL11.glBindTexture(GL11.GL_TEXTURE_2D, 0);
		
		// 创建FBO并附加纹理
		state.fbo = GL30.glGenFramebuffers();
		GL30.glBindFramebuffer(GL30.GL_FRAMEBUFFER, state.fbo);
		GL30.glFramebufferTexture2D(GL30.GL_FRAMEBUFFER, GL30.GL_COLOR_ATTACHMENT0, GL11.GL_TEXTURE_2D, state.tex, 0);
		
		int status = GL30.glCheckFramebufferStatus(GL30.GL_FRAMEBUFFER);
		if(status != GL30.GL_FRAMEBUFFER_COMPLETE) {
			LOGGER.error("Scale FBO incomplete: 0x{}", Integer.toHexString(status));
		}
		
		GL30.glBindFramebuffer(GL30.GL_FRAMEBUFFER, 0);
		state.width = width;
		state.height = height;
		
		LOGGER.debug("Created scale FBO for window 0x{}: {}x{}", Long.toHexString(windowHandle), width, height);
	}
	
	/**
	 * 清理单个窗口的 PBO 资源（该窗口停止共享时调用）
	 * 加 glIsBuffer 校验：无效 ID 直传 glDeleteBuffers 在某些驱动（mesa gallium / nvidia legacy）会 SIGSEGV
	 */
	public static void cleanupPbo(long windowHandle) {
		PboState state = pboStates.remove(windowHandle);
		if(state != null) {
			state.cleanup();
		}
	}
	
	/**
	 * 清理单个窗口的缩放 FBO 资源（该窗口停止共享时调用）
	 */
	public static void cleanupScaleFbo(long windowHandle) {
		ScaleState state = scaleStates.remove(windowHandle);
		if(state != null) {
			state.cleanup();
		}
	}
	
	/**
	 * 清理所有窗口的 GPU 资源（mod 卸载/断开时调用）
	 */
	public static void cleanupAllWindowResources() {
		for(PboState state : pboStates.values()) {
			state.cleanup();
		}
		pboStates.clear();
		for(ScaleState state : scaleStates.values()) {
			state.cleanup();
		}
		scaleStates.clear();
	}
	
	/**
	 * 直接从RGBA ByteBuffer编码图像（跳过BufferedImage中间层）
	 * 
	 * 自动选择编码格式：
	 * - 全部像素 alpha==255（不透明）→ JPEG（体积小、速度快）
	 * - 存在透明/半透明像素 → PNG（保留 alpha，避免窗口圆角/阴影变黑边）
	 *   窗口 framebuffer 含 alpha（圆角、阴影等），JPEG 不支持 alpha 会把
	 *   透明区域编码成黑色 → 接收端出现"老式黑边框"。
	 * 
	 * @param rgbaBuffer RGBA像素数据（bottom-to-top if from PBO）
	 * @param width 图像宽度
	 * @param height 图像高度
	 * @param quality JPEG质量 (0.0-1.0)，PNG时忽略
	 * @param flipY 是否翻转Y轴（PBO数据是bottom-to-top）
	 * @return 压缩后的图像数据
	 */
	@Nullable
	public static byte[] compressToJpegDirect(ByteBuffer rgbaBuffer, int width, int height, float quality, boolean flipY) {
		return compressToJpegDirect(rgbaBuffer, width, height, quality, flipY, false);
	}
	
	/**
	 * forceJpeg=true 时（v0.2.32）：透明像素混合到黑色背景后一律 JPEG，
	 * 绝不走 PNG —— 供大小保护降级使用，保证 quality 参数真正生效。
	 */
	@Nullable
	public static byte[] compressToJpegDirect(ByteBuffer rgbaBuffer, int width, int height, float quality, boolean flipY, boolean forceJpeg) {
		try {
			// 先检查是否有透明像素（决定用 PNG 还是 JPEG；forceJpeg 时跳过）
			boolean hasAlpha = false;
			if(!forceJpeg) {
				// 采样检查：隔行扫描，减少开销；但为准确起见扫全部 alpha 通道
				for(int i = 3; i < width * height * 4; i += 4) {
					if((rgbaBuffer.get(i) & 0xFF) != 0xFF) {
						hasAlpha = true;
						break;
					}
				}
			}
			// 回到缓冲区开头（上面的扫描用了绝对 get，不影响 position；保险起见 rewind）
			rgbaBuffer.rewind();
			
			if(hasAlpha) {
				// PNG：保留 alpha
				BufferedImage argbImage = new BufferedImage(width, height, BufferedImage.TYPE_INT_ARGB);
				int[] argbPixels = new int[width * height];
				for(int y = 0; y < height; y++) {
					int srcY = flipY ? (height - 1 - y) : y;
					int rowOffset = srcY * width * 4;
					for(int x = 0; x < width; x++) {
						int pixelOffset = rowOffset + x * 4;
						int r = rgbaBuffer.get(pixelOffset) & 0xFF;
						int g = rgbaBuffer.get(pixelOffset + 1) & 0xFF;
						int b = rgbaBuffer.get(pixelOffset + 2) & 0xFF;
						int a = rgbaBuffer.get(pixelOffset + 3) & 0xFF;
						argbPixels[y * width + x] = (a << 24) | (r << 16) | (g << 8) | b;
					}
				}
				argbImage.setRGB(0, 0, width, height, argbPixels, 0, width);
				rgbaBuffer.rewind();
				
				ByteArrayOutputStream outputStream = new ByteArrayOutputStream();
				ImageIO.write(argbImage, "png", outputStream);
				return outputStream.toByteArray();
			}
			
			// JPEG：无透明，直接 RGB；forceJpeg 时透明像素混合到黑色背景
			BufferedImage rgbImage = new BufferedImage(width, height, BufferedImage.TYPE_INT_RGB);
			int[] rgbPixels = new int[width * height];
			
			for(int y = 0; y < height; y++) {
				// PBO数据是bottom-to-top，需要翻转
				int srcY = flipY ? (height - 1 - y) : y;
				int rowOffset = srcY * width * 4;
				
				for(int x = 0; x < width; x++) {
					int pixelOffset = rowOffset + x * 4;
					int r = rgbaBuffer.get(pixelOffset) & 0xFF;
					int g = rgbaBuffer.get(pixelOffset + 1) & 0xFF;
					int b = rgbaBuffer.get(pixelOffset + 2) & 0xFF;
					if(forceJpeg) {
						// 透明像素按 alpha 混合到黑色背景（alpha<255 时压暗；全透明=纯黑）
						int a = rgbaBuffer.get(pixelOffset + 3) & 0xFF;
						if(a < 255) {
							r = r * a / 255;
							g = g * a / 255;
							b = b * a / 255;
						}
					}
					// 跳过alpha（JPEG不支持）
					rgbPixels[y * width + x] = (r << 16) | (g << 8) | b;
				}
			}
			
			rgbImage.setRGB(0, 0, width, height, rgbPixels, 0, width);
			
			// JPEG压缩
			ByteArrayOutputStream outputStream = new ByteArrayOutputStream();
			ImageWriter writer = ImageIO.getImageWritersByFormatName("jpeg").next();
			ImageWriteParam param = writer.getDefaultWriteParam();
			param.setCompressionMode(ImageWriteParam.MODE_EXPLICIT);
			param.setCompressionQuality(quality);
			
			ImageOutputStream imageOutputStream = new MemoryCacheImageOutputStream(outputStream);
			writer.setOutput(imageOutputStream);
			writer.write(null, new IIOImage(rgbImage, null, null), param);
			writer.dispose();
			imageOutputStream.close();
			
			return outputStream.toByteArray();
			
		} catch(IOException e) {
			LOGGER.error("Failed to compress image to JPEG (direct)", e);
			return null;
		}
	}
	
	// ===== 保留旧方法以兼容其他调用点 =====
	
	@Nullable
	public static byte[] captureFramebuffer(int x, int y, int width, int height) {
		return captureFramebuffer(x, y, width, height, DEFAULT_SCALE, DEFAULT_JPEG_QUALITY);
	}
	
	@Nullable
	public static byte[] captureFramebuffer(int x, int y, int width, int height, float scale, float quality) {
		width = Math.min(width, MAX_WIDTH);
		height = Math.min(height, MAX_HEIGHT);
		
		int scaledWidth = (int)(width * scale);
		int scaledHeight = (int)(height * scale);
		
		if(scaledWidth <= 0 || scaledHeight <= 0) {
			LOGGER.warn("Invalid capture dimensions: {}x{}", scaledWidth, scaledHeight);
			return null;
		}
		
		try {
			ByteBuffer buffer = ByteBuffer.allocateDirect(width * height * 4);
			GL11.glReadPixels(x, y, width, height, GL11.GL_RGBA, GL11.GL_UNSIGNED_BYTE, buffer);
			
			// 直接编码（旧路径不用PBO，但用直接编码）
			if(scale != 1.0f) {
				// 需要缩放 — 用旧的CPU路径
				BufferedImage image = pixelBufferToImage(buffer, width, height);
				image = scaleImage(image, scaledWidth, scaledHeight);
				return compressToJpeg(image, quality);
			} else {
				return compressToJpegDirect(buffer, width, height, quality, true);
			}
		} catch(Exception e) {
			LOGGER.error("Failed to capture framebuffer", e);
			return null;
		}
	}
	
	// ===== 旧的辅助方法（保留兼容） =====
	
	private static BufferedImage pixelBufferToImage(ByteBuffer buffer, int width, int height) {
		BufferedImage image = new BufferedImage(width, height, BufferedImage.TYPE_INT_ARGB);
		for(int y = 0; y < height; y++) {
			for(int x = 0; x < width; x++) {
				int index = ((height - y - 1) * width + x) * 4;
				int r = buffer.get(index) & 0xFF;
				int g = buffer.get(index + 1) & 0xFF;
				int b = buffer.get(index + 2) & 0xFF;
				int a = buffer.get(index + 3) & 0xFF;
				int argb = (a << 24) | (r << 16) | (g << 8) | b;
				image.setRGB(x, y, argb);
			}
		}
		return image;
	}
	
	private static BufferedImage scaleImage(BufferedImage source, int targetWidth, int targetHeight) {
		BufferedImage scaled = new BufferedImage(targetWidth, targetHeight, BufferedImage.TYPE_INT_ARGB);
		java.awt.Graphics2D g2d = scaled.createGraphics();
		g2d.setRenderingHint(java.awt.RenderingHints.KEY_INTERPOLATION, 
			java.awt.RenderingHints.VALUE_INTERPOLATION_BILINEAR);
		g2d.setRenderingHint(java.awt.RenderingHints.KEY_RENDERING, 
			java.awt.RenderingHints.VALUE_RENDER_QUALITY);
		g2d.drawImage(source, 0, 0, targetWidth, targetHeight, null);
		g2d.dispose();
		return scaled;
	}
	
	@Nullable
	public static byte[] compressToJpeg(BufferedImage image, float quality) {
		try {
			BufferedImage rgbImage = new BufferedImage(image.getWidth(), image.getHeight(), BufferedImage.TYPE_INT_RGB);
			java.awt.Graphics2D g2d = rgbImage.createGraphics();
			g2d.drawImage(image, 0, 0, null);
			g2d.dispose();
			
			ByteArrayOutputStream outputStream = new ByteArrayOutputStream();
			ImageWriter writer = ImageIO.getImageWritersByFormatName("jpeg").next();
			ImageWriteParam param = writer.getDefaultWriteParam();
			param.setCompressionMode(ImageWriteParam.MODE_EXPLICIT);
			param.setCompressionQuality(quality);
			
			ImageOutputStream imageOutputStream = new MemoryCacheImageOutputStream(outputStream);
			writer.setOutput(imageOutputStream);
			writer.write(null, new IIOImage(rgbImage, null, null), param);
			writer.dispose();
			imageOutputStream.close();
			
			return outputStream.toByteArray();
		} catch(IOException e) {
			LOGGER.error("Failed to compress image to JPEG", e);
			return null;
		}
	}
	
	@Nullable
	public static byte[] compressWithDeflater(byte[] data) {
		Deflater deflater = new Deflater(Deflater.BEST_SPEED);
		deflater.setInput(data);
		deflater.finish();
		
		ByteArrayOutputStream outputStream = new ByteArrayOutputStream(data.length);
		byte[] buffer = new byte[1024];
		
		while(!deflater.finished()) {
			int count = deflater.deflate(buffer);
			outputStream.write(buffer, 0, count);
		}
		
		deflater.end();
		return outputStream.toByteArray();
	}
	
	@Nullable
	public static byte[] calculateDiff(byte[] previousFrame, byte[] currentFrame) {
		if(previousFrame == null || currentFrame == null) {
			return currentFrame;
		}
		if(previousFrame.length != currentFrame.length) {
			return currentFrame;
		}
		
		ByteArrayOutputStream diffStream = new ByteArrayOutputStream();
		int changedPixels = 0;
		
		for(int i = 0; i < currentFrame.length; i++) {
			if(previousFrame[i] != currentFrame[i]) {
				diffStream.write(i & 0xFF);
				diffStream.write((i >> 8) & 0xFF);
				diffStream.write(currentFrame[i]);
				changedPixels++;
			}
		}
		
		if(changedPixels > currentFrame.length / 2) {
			return currentFrame;
		}
		
		return diffStream.toByteArray();
	}
	
	public static CaptureConfig getRecommendedConfig(int width, int height) {
		// scale 一律 1.0：远程跟本地渲染一致是底线，绝不缩放。
		// 差异化只体现在 fps（大窗口低帧率、小窗口高帧率）与 quality。
		if(width * height > 1920 * 1080) {
			return new CaptureConfig(1.0f, 0.5f, 15);
		} else if(width * height > 1280 * 720) {
			return new CaptureConfig(1.0f, 0.6f, 24);
		} else {
			return new CaptureConfig(1.0f, 0.7f, 30);
		}
	}
	
	/**
	 * 清理所有GPU资源（mod卸载时调用）
	 */
	public static void cleanup() {
		cleanupAllWindowResources();
		lastRawFrames.clear();
		reusableBuffer = null;
	}
	
	/**
	 * 捕获配置
	 */
	public static class CaptureConfig {
		public float scale;
		public float quality;
		public int maxFps;
		public boolean diffUpdate;
		public int maxBitrate;
		public int frameBuffer;
		public int latencyComp;
		public boolean prediction;
		public String compression;
		public float diffThreshold;  // 新增：像素变化阈值
		
		// === JPEG 大小保护（发送端自动降级） ===
		// 默认参数：上限 600KB（v0.2.30 收紧自 1.8MB —— 弱服务器上 450KB+ 大帧的
		// netty/GC 压力会拖垮服务端 tick；600KB 对 1080p 文本窗口在 quality=0.85
		// 下通常 300KB 左右，正常内容不会触发降级，只有超大/超复杂窗口才降），
		// 最多降 2 轮：先降 quality（1.0 → 0.85 → 0.7），仍超限再降 scale（1.0 → 0.75 → 0.5）。
		// 注意：WindowShareManager 的"降级只降 quality 不降 scale"是 UI 大小底线，
		// 与这里的 scale 阶梯并不冲突 —— captureFrameWithSizeProtection 传参时
		// scale 被冻结为 effectiveScale，实际运行时不会走 scale 阶梯。
		// v0.2.32：maxJpegBytes 600KB → 1.8MB（对齐服务端 SharedWindowFrameRelay 1.9MB
		// 协议上限留余量）。原 600KB 对高分屏窗口（1080p+ 满屏 UI 可达 1.1MB+）过严，
		// 且含透明像素时走 PNG 无损路径 quality 无效 → 降级无效 → 全部帧被 DROP → 查看端卡死。
		// 少部分人使用、带宽不敏感，优先保证画面可达；极端超大帧由强制 JPEG 降级兜底。
		public static final long DEFAULT_MAX_JPEG_BYTES = 1_800_000L;
		public static final int DEFAULT_MAX_DEGRADE_ROUNDS = 3;
		// 阶梯加低档：1.0 → 0.85 → 0.7 → 0.55 → 0.4，极端情况仍有降级空间
		public static final float[] DEFAULT_JPEG_QUALITY_LADDER = {1.0f, 0.85f, 0.7f, 0.55f, 0.4f};
		public static final float[] DEFAULT_JPEG_SCALE_LADDER = {1.0f, 0.75f, 0.5f};
		
		public long maxJpegBytes;         // 单帧 JPEG 大小上限（超限自动降级重编码；0 表示不限制）
		public int maxDegradeRounds;      // 最多降级重编码轮数（0 = 不降级，超限直接丢弃）
		public float[] jpegQualityLadder; // quality 降级阶梯（取严格小于当前 quality 的下一档）
		public float[] jpegScaleLadder;   // scale 降级阶梯（取严格小于当前 scale 的下一档）
		
		public CaptureConfig(float scale, float quality, int maxFps) {
			this(scale, quality, maxFps, true, 0, 3, 0, false, "jpeg", 0.02f);
		}
		
		public CaptureConfig(float scale, float quality, int maxFps,
				boolean diffUpdate, int maxBitrate, int frameBuffer,
				int latencyComp, boolean prediction, String compression) {
			this(scale, quality, maxFps, diffUpdate, maxBitrate, frameBuffer, latencyComp, prediction, compression, 0.02f);
		}
		
		public CaptureConfig(float scale, float quality, int maxFps,
				boolean diffUpdate, int maxBitrate, int frameBuffer,
				int latencyComp, boolean prediction, String compression, float diffThreshold) {
			this.scale = Math.max(0.1f, Math.min(1.0f, scale));
			this.quality = Math.max(0.1f, Math.min(1.0f, quality));
			// maxFps=0 表示无限制（跟随渲染帧率/编码能力）；>0 为硬上限（最高 240）
			this.maxFps = Math.max(0, Math.min(240, maxFps));
			this.diffUpdate = diffUpdate;
			this.maxBitrate = Math.max(0, maxBitrate);
			this.frameBuffer = Math.max(1, Math.min(10, frameBuffer));
			this.latencyComp = Math.max(0, Math.min(500, latencyComp));
			this.prediction = prediction;
			this.compression = compression;
			this.diffThreshold = Math.max(0.001f, Math.min(1.0f, diffThreshold));
			// JPEG 大小保护默认值（所有既有构造路径自动获得，不影响兼容性）
			this.maxJpegBytes = DEFAULT_MAX_JPEG_BYTES;
			this.maxDegradeRounds = DEFAULT_MAX_DEGRADE_ROUNDS;
			this.jpegQualityLadder = DEFAULT_JPEG_QUALITY_LADDER.clone();
			this.jpegScaleLadder = DEFAULT_JPEG_SCALE_LADDER.clone();
		}
		
		/**
		 * 完整配置（含 JPEG 大小保护参数）。
		 * 传入 null 或空数组的阶梯时保持默认阶梯；maxJpegBytes/maxDegradeRounds 传入负值取 0。
		 */
		public CaptureConfig(float scale, float quality, int maxFps,
				boolean diffUpdate, int maxBitrate, int frameBuffer,
				int latencyComp, boolean prediction, String compression, float diffThreshold,
				long maxJpegBytes, int maxDegradeRounds, float[] jpegQualityLadder, float[] jpegScaleLadder) {
			this(scale, quality, maxFps, diffUpdate, maxBitrate, frameBuffer, latencyComp, prediction, compression, diffThreshold);
			this.maxJpegBytes = Math.max(0L, maxJpegBytes);
			this.maxDegradeRounds = Math.max(0, maxDegradeRounds);
			if(jpegQualityLadder != null && jpegQualityLadder.length > 0) {
				this.jpegQualityLadder = jpegQualityLadder.clone();
			}
			if(jpegScaleLadder != null && jpegScaleLadder.length > 0) {
				this.jpegScaleLadder = jpegScaleLadder.clone();
			}
		}
		
		public static CaptureConfig highPerformance() {
			// scale=1.0：性能靠高帧率 + 低画质体现，不靠缩放（底线）
			return new CaptureConfig(1.0f, 0.5f, 60, true, 1000, 2, 50, true, "jpeg", 0.03f);
		}
		
		public static CaptureConfig highQuality() {
			return new CaptureConfig(1.0f, 1.0f, 24, true, 0, 5, 0, false, "jpeg", 0.01f);
		}
		
		public static CaptureConfig balanced() {
			return new CaptureConfig(1.0f, 0.85f, 24, true, 2000, 3, 20, false, "jpeg", 0.02f);
		}
		
		public static CaptureConfig lowLatency() {
			return new CaptureConfig(1.0f, 0.6f, 60, true, 1500, 1, 0, true, "jpeg", 0.05f);
		}
		
		public String getSummary() {
			return String.format(
				"scale=%.2f quality=%.2f fps=%d diff=%s bitrate=%dkbps buffer=%d latency=%dms pred=%s comp=%s diffThreshold=%.3f jpegLimit=%d jpegRounds=%d",
				scale, quality, maxFps, diffUpdate, maxBitrate, frameBuffer, latencyComp, prediction, compression, diffThreshold, maxJpegBytes, maxDegradeRounds
			);
		}
	}
}
