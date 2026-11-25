package dev.evvie.waylandcraft.shared;

import java.awt.image.BufferedImage;
import java.io.ByteArrayInputStream;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.atomic.AtomicBoolean;

import javax.imageio.ImageIO;

import org.jetbrains.annotations.Nullable;
import org.lwjgl.system.MemoryUtil;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import com.mojang.blaze3d.platform.NativeImage;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientTickEvents;
import net.minecraft.client.Minecraft;
import net.minecraft.client.renderer.texture.DynamicTexture;
import net.minecraft.client.renderer.texture.TextureManager;
import net.minecraft.resources.Identifier;

/**
 * 远程窗口渲染器（⑦ playout 缓冲版）
 *
 * JPEG 路径架构：
 *   updateTexture() [任意线程] ──压缩帧环形队列──▶ 解码调度按 playout 时钟出队
 *     ──▶ 单线程解码 executor（工作线程：JPEG→ARGB）──▶ 每窗口解码结果队列（容量 3）
 *     ──▶ 客户端 tick（渲染线程）选择"已到播放时刻的最新帧"→ GL 上传
 *
 * playout 模型（预缓冲换流畅；D = sharePlayoutDelayMs，默认 4000，0 = 低延迟旧行为）：
 * - 每帧携带发送端 captureTs；接收端对 (本地到达 − captureTs) 做 min-tracking，
 *   得到传输下界估计 minTransit（含跨机时钟偏移常数项——偏移不破坏平滑性，
 *   播放节奏只取决于相邻帧 captureTs 的相对差）。
 * - 帧播放时刻 = captureTs + minTransit + D。解码只在播放时刻前 DECODE_LOOKAHEAD_MS
 *   窗口内进行，不为"看了就丢"的帧浪费解码。
 * - 网络抖动/中继批量到达由缓冲吸收：观众看到恒定 D 延迟，但画面时间抖动趋零。
 * - 欠载（帧未到期而队列空）时画面冻结在上一帧自然追平，不做撕裂式跳帧追赶。
 * - 压缩环形队列单窗口 48MB 封顶：超限丢最旧，宁可缓冲变浅也绝不 OOM。
 *
 * 关键约束：
 * 1. 解码线程绝不碰 GL：createTexture/destroyTexture/upload 只允许在客户端（渲染）线程执行
 * 2. updateTextureRGBA（Portal 本地捕获）仍为同步路径，调用方必须保证在渲染线程
 * 3. 每窗口至多一个解码任务在途（decodeInFlight），完成后续调度，多窗口公平轮转
 * 4. 流状态（环形队列/时钟估计）与纹理生命周期解耦：纹理因尺寸变化重建不动流；
 *    窗口注销由 removeWindow() 显式清理（handleWindowListUpdate / clear）
 */
public class RemoteWindowRenderer {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-remote-renderer");
	
	private final Map<Long, TextureEntry> textureCache = new ConcurrentHashMap<>();
	
	private static final int MAX_CACHED_TEXTURES = 50;
	private static final int CLEANUP_INTERVAL = 600;
	private int tickCounter = 0;
	
	// === ⑦ playout 缓冲状态 ===
	/** 每窗口一个压缩帧环形队列 + playout 时钟估计。锁：synchronized(stream) */
	private final Map<Long, BufferedStream> streamsByWindow = new ConcurrentHashMap<>();
	/** 每窗口解码结果队列（容量 DECODED_QUEUE_CAPACITY，满则丢最旧） */
	private final Map<Long, ArrayDeque<DecodedFrame>> decodedByWindow = new ConcurrentHashMap<>();
	private static final int DECODED_QUEUE_CAPACITY = 3;
	/** 压缩环形队列单窗口字节上限：超限丢最旧（缓冲深度降级，拒绝 OOM） */
	private static final int MAX_BUFFER_BYTES_PER_WINDOW = 48 * 1024 * 1024;
	/** 允许在播放时刻前多久预解码（ms）：更远的帧不占解码线程 */
	private static final long DECODE_LOOKAHEAD_MS = 150;
	/** minTransit 向上漂移速率（ms/帧）：容忍发送端变慢，但抗突发尖峰拉高估计 */
	private static final double MIN_TRANSIT_DRIFT_MS = 5.0;
	/** settings 不可用时的默认预缓冲深度：4s，用延迟换零卡顿 */
	private static final long DEFAULT_PLAYOUT_DELAY_MS = 4000;
	
	private final Object executorLock = new Object();
	private ExecutorService decodeExecutor;
	
	private static final AtomicBoolean TICK_HOOK_REGISTERED = new AtomicBoolean(false);
	
	public RemoteWindowRenderer() {
		LOGGER.info("RemoteWindowRenderer initialized (playout buffer)");
		// 客户端 tick 运行在渲染/GL 线程：在这里推进解码调度并上传
		if(TICK_HOOK_REGISTERED.compareAndSet(false, true)) {
			ClientTickEvents.END_CLIENT_TICK.register(mc -> tick());
		}
	}
	
	/** 当前预缓冲深度（ms）。0 = 低延迟模式（上传最新帧，行为与旧版一致）。 */
	public static long playoutDelayMs() {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc != null && wlc.settings != null) return wlc.settings.getSharePlayoutDelayMs();
		return DEFAULT_PLAYOUT_DELAY_MS;
	}
	
	private Identifier getTextureLocation(long windowHandle) {
		return Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "remote_" + windowHandle);
	}
	
	/**
	 * 更新远程窗口纹理（JPEG 路径：环形缓冲 + 按播放时刻调度解码）
	 * 
	 * 只把压缩帧 append 到该窗口的环形队列后立即返回（任意线程可调）。
	 * 解码在后台工作线程按 playout 时钟出队执行；渲染线程在 tick 上传"已到点
	 * 的最新帧"（见 flushPendingUploads）。
	 */
	public void updateTexture(long windowHandle, int x, int y, int width, int height, byte[] jpegData, long captureTimestampMs, int frameSeq) {
		if(jpegData == null || jpegData.length == 0) return;
		
		BufferedStream stream = streamsByWindow.computeIfAbsent(windowHandle, BufferedStream::new);
		long now = System.currentTimeMillis();
		boolean shouldSchedule = false;
		synchronized(stream) {
			stream.lastArrivalMs = now;
			long transit = now - captureTimestampMs;
			if(transit < 0) transit = 0; // 发送端时钟超前：按 0 处理，损失缓冲深度不破坏平滑
			stream.minTransitMs = stream.minTransitMs < 0
				? transit
				: (long)Math.min(transit, stream.minTransitMs + MIN_TRANSIT_DRIFT_MS);
			
			stream.queue.addLast(new CompressedFrame(captureTimestampMs, frameSeq, x, y, width, height, jpegData));
			stream.bufferedBytes += jpegData.length;
			// 低延迟模式（D=0）：等价旧"单槽覆盖"语义——只保留最新 2 帧
			// （1 帧在解码在途 + 1 帧待解），多余压缩帧直接丢弃，不浪费解码。
			if(playoutDelayMs() <= 0) {
				while(stream.queue.size() > 2) {
					CompressedFrame dropped = stream.queue.removeFirst();
					stream.bufferedBytes -= dropped.jpegData.length;
					stream.droppedByCap++;
				}
			}
			while(stream.bufferedBytes > MAX_BUFFER_BYTES_PER_WINDOW && stream.queue.size() > 1) {
				CompressedFrame dropped = stream.queue.removeFirst();
				stream.bufferedBytes -= dropped.jpegData.length;
				stream.droppedByCap++;
			}
			shouldSchedule = !stream.decodeInFlight;
		}
		// 到达即尝试推进调度（帧可能已到期；未到期的由 tick 泵兜底）
		if(shouldSchedule) pumpStreamDecode(stream, now);
	}
	
	/**
	 * 解码调度（调用方保证逻辑上单窗口串行；内部以 stream 锁 + decodeInFlight 防重入）：
	 * 队头帧到达"播放时刻 − lookahead"时出队并提交解码；每窗口至多一个在途任务。
	 */
	private void pumpStreamDecode(BufferedStream stream, long now) {
		CompressedFrame next = null;
		synchronized(stream) {
			if(stream.decodeInFlight || stream.queue.isEmpty()) return;
			CompressedFrame head = stream.queue.peekFirst();
			long dueAt = head.captureTimestampMs + stream.minTransitMs + playoutDelayMs() - DECODE_LOOKAHEAD_MS;
			if(now < dueAt) return;
			next = stream.queue.removeFirst();
			stream.bufferedBytes -= next.jpegData.length;
			stream.decodeInFlight = true;
		}
		final CompressedFrame job = next;
		try {
			ensureExecutor().submit(() -> decodeWindow(stream, job));
		} catch(RejectedExecutionException e) {
			try {
				ensureExecutor().submit(() -> decodeWindow(stream, job));
			} catch(RejectedExecutionException e2) {
				LOGGER.warn("Failed to submit JPEG decode for window 0x{}", Long.toHexString(stream.windowHandle));
				synchronized(stream) {
					stream.decodeInFlight = false;
				}
			}
		}
	}
	
	/**
	 * 工作线程：解码一帧 JPEG → ARGB，结果（带 captureTs）放入该窗口解码结果队列。
	 * 结束后续调度本流（若仍是当前流），让多个窗口在单线程 executor 上公平轮转。
	 * 全程不碰 GL。
	 */
	private void decodeWindow(BufferedStream stream, CompressedFrame job) {
		try {
			int[] argbPixels;
			int decodedW, decodedH;
			try {
				BufferedImage bufferedImage = ImageIO.read(new ByteArrayInputStream(job.jpegData));
				if(bufferedImage == null) {
					LOGGER.warn("ImageIO.read returned null ({} bytes)", job.jpegData.length);
					return;
				}
				decodedW = bufferedImage.getWidth();
				decodedH = bufferedImage.getHeight();
				// 批量获取所有像素（1次调用）
				argbPixels = bufferedImage.getRGB(0, 0, decodedW, decodedH, null, 0, decodedW);
			} catch(Exception e) {
				LOGGER.error("Failed to decode JPEG ({} bytes) on worker thread", job.jpegData.length, e);
				return;
			}
			
			offerDecoded(stream, new DecodedFrame(stream.windowHandle, job.x, job.y, decodedW, decodedH, argbPixels, job.captureTimestampMs, job.frameSeq));
		} finally {
			if(streamsByWindow.get(stream.windowHandle) == stream) {
				boolean hadWork;
				synchronized(stream) {
					stream.decodeInFlight = false;
					hadWork = !stream.queue.isEmpty();
				}
				if(hadWork) pumpStreamDecode(stream, System.currentTimeMillis());
			}
		}
	}
	
	/**
	 * 把解码结果放入该窗口的结果队列；窗口已注销/重建则丢弃。
	 * 队列满（容量 DECODED_QUEUE_CAPACITY）时丢最旧帧。
	 */
	private void offerDecoded(BufferedStream stream, DecodedFrame frame) {
		if(streamsByWindow.get(stream.windowHandle) != stream) return;
		
		ArrayDeque<DecodedFrame> queue = decodedByWindow.computeIfAbsent(stream.windowHandle, h -> new ArrayDeque<>());
		synchronized(queue) {
			if(queue.size() >= DECODED_QUEUE_CAPACITY) {
				queue.poll(); // 丢最旧
			}
			queue.offer(frame);
		}
	}
	
	/**
	 * 渲染线程：按 playout 时钟选择每窗口"已到播放时刻的最新帧"上传（必须在 GL/客户端线程调用）。
	 * D=0 时退化为旧行为（只传最新、丢弃中间）。D>0 时未到点不动纹理（冻结即平滑的一部分）。
	 */
	private void flushPendingUploads() {
		if(decodedByWindow.isEmpty()) return;
		long now = System.currentTimeMillis();
		long delay = playoutDelayMs();
		for(long windowHandle : new ArrayList<>(decodedByWindow.keySet())) {
			ArrayDeque<DecodedFrame> queue = decodedByWindow.get(windowHandle);
			if(queue == null) continue;
			BufferedStream stream = streamsByWindow.get(windowHandle);
			long offset = stream != null && stream.minTransitMs >= 0 ? stream.minTransitMs : 0;
			DecodedFrame chosen = null;
			synchronized(queue) {
				if(delay <= 0) {
					chosen = queue.pollLast();
					queue.clear();
				}
				else {
					// 队列按 captureTs 升序（单流顺序解码）。吃掉所有已到点帧，
					// 取最后一个到点帧上传（中间的到点过期帧丢弃——它们已过播放时刻）。
					while(true) {
						DecodedFrame head = queue.peekFirst();
						if(head == null || head.captureTimestampMs + offset + delay > now) break;
						queue.pollFirst();
						chosen = head;
					}
				}
			}
			if(chosen != null) {
				applyDecodedFrame(chosen);
			}
		}
	}
	
	/**
	 * 渲染线程：把解码好的 ARGB 帧写入纹理并上传（原地内存写入 + upload）。
	 * 纹理尺寸随帧变化时重建（不动流状态——尺寸变化是合法事件）。
	 */
	private void applyDecodedFrame(DecodedFrame frame) {
		long windowHandle = frame.windowHandle;
		int decodedW = frame.width;
		int decodedH = frame.height;
		
		TextureEntry entry = textureCache.get(windowHandle);
		
		// 尺寸变化或首次创建
		if(entry == null || entry.width != decodedW || entry.height != decodedH) {
			destroyTexture(windowHandle);
			entry = createTexture(windowHandle, decodedW, decodedH);
			if(entry == null) return;
		}
		
		// === 原地更新：直接写入 NativeImage 原生内存 ===
		try {
			NativeImage nativeImage = entry.texture.getPixels();
			if(nativeImage == null || nativeImage.isClosed()) {
				// NativeImage 无效，重建
				destroyTexture(windowHandle);
				entry = createTexture(windowHandle, decodedW, decodedH);
				if(entry == null) return;
				nativeImage = entry.texture.getPixels();
				if(nativeImage == null) return;
			}
			
			// 通过 getPointer() 直接访问原生内存
			long ptr = nativeImage.getPointer();
			if(ptr == 0L) {
				LOGGER.warn("NativeImage pointer is null for window 0x{}", Long.toHexString(windowHandle));
				return;
			}
			
			// NativeImage 内存布局：每像素 4 字节，通道字节序 = A,B,G,R（pixel offset 0 = A 字节）
			// 必须用 memPutByte 逐字节写入 — putInt 会按 JVM LE 字节序把 int 写为 [R,G,B,A],
			// 索引 0 是 A 而不是 R，等于把通道对调，渲染出来全花。逐字节写 A,B,G,R 才行。
			writeArgbPixelsToNative(ptr, frame.argbPixels);
			
			// 上传到 GPU（不重建纹理对象，不重新 register）
			entry.texture.upload();
			entry.lastUpdate = System.currentTimeMillis();
			
		} catch(Exception e) {
			LOGGER.error("Failed to update texture in-place for window 0x{}", Long.toHexString(windowHandle), e);
			// 回退：完全重建
			destroyTexture(windowHandle);
			entry = createTexture(windowHandle, decodedW, decodedH);
			if(entry != null) {
				try {
					writePixelsAndUpload(entry, frame.argbPixels, decodedW, decodedH);
				} catch(Exception e2) {
					LOGGER.error("Fallback also failed", e2);
				}
			}
		}
	}
	
	/**
	 * 直接上传 RGBA 像素（用于 Portal ScreenCast 原始帧，不经过 JPEG 解码）
	 * 字节顺序为 R,G,B,A（与 NativeImage Format.RGBA 内存布局一致，直接写入）
	 */
	public void updateTextureRGBA(long windowHandle, int width, int height, byte[] rgbaData) {
		if(width <= 0 || height <= 0 || rgbaData == null) return;
		if(rgbaData.length < width * height * 4) {
			LOGGER.warn("RGBA data too small: {} bytes for {}x{}", rgbaData.length, width, height);
			return;
		}
		
		TextureEntry entry = textureCache.get(windowHandle);
		if(entry == null || entry.width != width || entry.height != height) {
			destroyTexture(windowHandle);
			entry = createTexture(windowHandle, width, height);
			if(entry == null) return;
		}
		
		try {
			NativeImage nativeImage = entry.texture.getPixels();
			if(nativeImage == null || nativeImage.isClosed()) {
				destroyTexture(windowHandle);
				entry = createTexture(windowHandle, width, height);
				if(entry == null) return;
				nativeImage = entry.texture.getPixels();
				if(nativeImage == null) return;
			}
			
			long ptr = nativeImage.getPointer();
			if(ptr == 0L) {
				LOGGER.warn("NativeImage pointer is null for window 0x{}", Long.toHexString(windowHandle));
				return;
			}
			
			// NativeImage Format.RGBA 内存布局 = [R,G,B,A]（byte0 = R）
			// Portal 帧字节序已是 R,G,B,A → 直接 memPutInt（little-endian 先写 R）
			long offset = ptr;
			for(int i = 0; i < width * height; i++) {
				int base = i * 4;
				int r = rgbaData[base] & 0xFF;
				int g = rgbaData[base + 1] & 0xFF;
				int b = rgbaData[base + 2] & 0xFF;
				int a = rgbaData[base + 3] & 0xFF;
				MemoryUtil.memPutInt(offset, (a << 24) | (b << 16) | (g << 8) | r);
				offset += 4;
			}
			
			entry.texture.upload();
			entry.lastUpdate = System.currentTimeMillis();
		} catch(Exception e) {
			LOGGER.error("Failed to update RGBA texture for window 0x{}", Long.toHexString(windowHandle), e);
		}
	}
	
	/**
	 * 惰性获取解码线程池（clear() 关闭后重建，保证重连后仍可用）
	 */
	private ExecutorService ensureExecutor() {
		synchronized(executorLock) {
			if(decodeExecutor == null || decodeExecutor.isShutdown() || decodeExecutor.isTerminated()) {
				decodeExecutor = Executors.newSingleThreadExecutor(r -> {
					Thread t = new Thread(r, "waylandcraft-jpeg-decode");
					t.setDaemon(true); // 守护线程：Minecraft 退出时不阻塞 JVM
					return t;
				});
			}
			return decodeExecutor;
		}
	}
	
	private void shutdownExecutor() {
		synchronized(executorLock) {
			if(decodeExecutor != null) {
				decodeExecutor.shutdownNow();
				decodeExecutor = null;
			}
		}
	}
	
	/**
	 * 写入像素并上传（用于首次创建和回退）
	 */
	private void writePixelsAndUpload(TextureEntry entry, int[] argbPixels, int width, int height) {
		NativeImage nativeImage = entry.texture.getPixels();
		if(nativeImage == null) return;
		
		long ptr = nativeImage.getPointer();
		if(ptr == 0L) return;
		
		writeArgbPixelsToNative(ptr, argbPixels);
		entry.texture.upload();
	}
	
	/**
	 * 把 ARGB int 像素数组写入 NativeImage 原生内存（通过绝对 ptr）
	 * 
	 * MC 26.x NativeImage（new NativeImage(w,h,false) → Format.RGBA）的内存布局：
	 *   redOffset=0, greenOffset=8, blueOffset=16, alphaOffset=24
	 * 即 little-endian 内存 [R, G, B, A]（byte0 = R）。
	 * 
	 * 用 memPutInt 一次写 4 字节（JVM little-endian）：
	 *   int V = (a<<24)|(b<<16)|(g<<8)|r → 内存 [r, g, b, a] ✓
	 */
	private static void writeArgbPixelsToNative(long ptr, int[] argbPixels) {
		long offset = ptr;
		for(int i = 0; i < argbPixels.length; i++) {
			int argb = argbPixels[i];
			int a = (argb >>> 24) & 0xFF;
			int r = (argb >>> 16) & 0xFF;
			int g = (argb >>> 8) & 0xFF;
			int b = argb & 0xFF;
			MemoryUtil.memPutInt(offset, (a << 24) | (b << 16) | (g << 8) | r);
			offset += 4;
		}
	}
	
	@Nullable
	private TextureEntry createTexture(long windowHandle, int width, int height) {
		try {
			if(textureCache.size() >= MAX_CACHED_TEXTURES) {
				cleanupOldTextures();
			}
			
			NativeImage image = new NativeImage(width, height, false);
			DynamicTexture texture = new DynamicTexture(() -> "remote_window_" + Long.toHexString(windowHandle), image);
			
			Identifier location = getTextureLocation(windowHandle);
			TextureManager textureManager = Minecraft.getInstance().getTextureManager();
			textureManager.register(location, texture);
			
			TextureEntry entry = new TextureEntry(texture, location, width, height);
			textureCache.put(windowHandle, entry);
			
			LOGGER.debug("Created texture for window 0x{} ({}x{})", Long.toHexString(windowHandle), width, height);
			return entry;
		} catch(Exception e) {
			LOGGER.error("Failed to create texture for window 0x{}", Long.toHexString(windowHandle), e);
			return null;
		}
	}
	
	public void destroyTexture(long windowHandle) {
		// 只清纹理侧状态：playout 流与在途解码跨纹理重建保留（尺寸变化是合法事件，
		// 缓冲/时钟估计不应因此清零）。窗口真正注销走 removeWindow()。
		decodedByWindow.remove(windowHandle);
		
		TextureEntry entry = textureCache.remove(windowHandle);
		if(entry != null) {
			TextureManager textureManager = Minecraft.getInstance().getTextureManager();
			textureManager.release(entry.location);
			LOGGER.debug("Destroyed texture for window 0x{}", Long.toHexString(windowHandle));
		}
	}
	
	/**
	 * ⑦ 窗口注销（共享列表不再包含该窗口）：丢流、丢结果队列；在途任务经身份校验自行丢弃。
	 */
	public void removeWindow(long windowHandle) {
		BufferedStream stream = streamsByWindow.remove(windowHandle);
		if(stream != null) {
			synchronized(stream) {
				stream.queue.clear();
				stream.bufferedBytes = 0;
			}
		}
		decodedByWindow.remove(windowHandle);
	}
	
	@Nullable
	public Identifier getTextureLocation_obj(long windowHandle) {
		TextureEntry entry = textureCache.get(windowHandle);
		return entry != null ? entry.location : null;
	}
	
	public boolean hasTexture(long windowHandle) {
		return textureCache.containsKey(windowHandle);
	}
	
	public int[] getTextureDimensions(long windowHandle) {
		TextureEntry entry = textureCache.get(windowHandle);
		if(entry == null) return null;
		return new int[]{entry.width, entry.height};
	}
	
	/** ⑦ 诊断：该窗口当前缓冲的压缩字节数与帧数（buffering 状态展示用） */
	public long getBufferedBytes(long windowHandle) {
		BufferedStream stream = streamsByWindow.get(windowHandle);
		if(stream == null) return 0;
		synchronized(stream) {
			return stream.bufferedBytes;
		}
	}
	
	public int getBufferedFrames(long windowHandle) {
		BufferedStream stream = streamsByWindow.get(windowHandle);
		if(stream == null) return 0;
		synchronized(stream) {
			return stream.queue.size();
		}
	}
	
	private void cleanupOldTextures() {
		long now = System.currentTimeMillis();
		long threshold = 30000;
		
		// 同步清理无流量的陈旧流（发送端停发/注销未走 removeWindow 的兜底）
		long streamThreshold = 60000;
		for(Map.Entry<Long, BufferedStream> e : new ArrayList<>(streamsByWindow.entrySet())) {
			BufferedStream stream = e.getValue();
			long lastActivity;
			synchronized(stream) {
				lastActivity = stream.lastArrivalMs;
			}
			if(now - lastActivity > streamThreshold) {
				removeWindow(e.getKey());
			}
		}
		
		textureCache.entrySet().removeIf(entry -> {
			if(now - entry.getValue().lastUpdate > threshold) {
				TextureManager textureManager = Minecraft.getInstance().getTextureManager();
				textureManager.release(entry.getValue().location);
				LOGGER.debug("Cleaned up old texture for window 0x{}", Long.toHexString(entry.getKey()));
				return true;
			}
			return false;
		});
	}
	
	public void tick() {
		// 渲染线程：推进各流解码调度（帧到期不依赖新包到达），再上传到点帧
		long now = System.currentTimeMillis();
		for(BufferedStream stream : new ArrayList<>(streamsByWindow.values())) {
			if(stream.decodeInFlight) continue;
			pumpStreamDecode(stream, now);
		}
		flushPendingUploads();
		
		tickCounter++;
		if(tickCounter >= CLEANUP_INTERVAL) {
			tickCounter = 0;
			cleanupOldTextures();
		}
	}
	
	public void clear() {
		streamsByWindow.clear();
		decodedByWindow.clear();
		
		TextureManager textureManager = Minecraft.getInstance().getTextureManager();
		textureCache.values().forEach(entry -> textureManager.release(entry.location));
		textureCache.clear();
		
		shutdownExecutor();
		LOGGER.info("RemoteWindowRenderer cleared");
	}
	
	/**
	 * ⑦ 每窗口压缩帧环形队列 + playout 时钟估计。所有字段在 synchronized(this) 下访问
	 * （lastArrivalMs 除外：写线程在锁内更新，读方可容忍旧值）。
	 */
	private static class BufferedStream {
		final long windowHandle;
		final ArrayDeque<CompressedFrame> queue = new ArrayDeque<>();
		long bufferedBytes;
		boolean decodeInFlight;
		/** (本地到达 − captureTs) 的 min-tracking 估计（ms）；-1 = 尚无样本 */
		long minTransitMs = -1;
		long droppedByCap;
		long lastArrivalMs = System.currentTimeMillis();
		
		BufferedStream(long windowHandle) {
			this.windowHandle = windowHandle;
		}
	}
	
	/** 环形队列元素：压缩帧 + playout 时钟信息 */
	private static class CompressedFrame {
		final long captureTimestampMs;
		final int frameSeq;
		final int x, y, width, height;
		final byte[] jpegData;
		CompressedFrame(long captureTimestampMs, int frameSeq, int x, int y, int width, int height, byte[] jpegData) {
			this.captureTimestampMs = captureTimestampMs;
			this.frameSeq = frameSeq;
			this.x = x;
			this.y = y;
			this.width = width;
			this.height = height;
			this.jpegData = jpegData;
		}
	}
	
	/** 工作线程解码结果（ARGB + captureTs），交由渲染线程按 playout 时钟上传 */
	private static class DecodedFrame {
		final long windowHandle;
		final int x, y;
		final int width, height;
		final int[] argbPixels;
		final long captureTimestampMs;
		final int frameSeq;
		DecodedFrame(long windowHandle, int x, int y, int width, int height, int[] argbPixels, long captureTimestampMs, int frameSeq) {
			this.windowHandle = windowHandle;
			this.x = x;
			this.y = y;
			this.width = width;
			this.height = height;
			this.argbPixels = argbPixels;
			this.captureTimestampMs = captureTimestampMs;
			this.frameSeq = frameSeq;
		}
	}
	
	private static class TextureEntry {
		DynamicTexture texture;
		final Identifier location;
		final int width;
		final int height;
		long lastUpdate;
		
		TextureEntry(DynamicTexture texture, Identifier location, int width, int height) {
			this.texture = texture;
			this.location = location;
			this.width = width;
			this.height = height;
			this.lastUpdate = System.currentTimeMillis();
		}
	}
}
