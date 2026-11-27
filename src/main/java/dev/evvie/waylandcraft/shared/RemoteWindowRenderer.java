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
 * 远程窗口渲染器（⑦ playout 缓冲版 → ⑧ v1.4.8 共享时钟 + 自适应深度版）
 *
 * JPEG 路径架构：
 *   updateTexture() [任意线程] ──压缩帧环形队列──▶ 解码调度按 playout 时钟出队
 *     ──▶ 单线程解码 executor（工作线程：JPEG→ARGB）──▶ 每窗口解码结果队列（容量 3）
 *     ──▶ 客户端 tick（渲染线程）按像素预算轮转选择"已到播放时刻的最新帧"→ GL 上传
 *
 * playout 模型（预缓冲换流畅；深度 D 与传输下界 X 全部由 SharePlayoutController
 * 统一管理——视频与音频两轨共享同一个 X（旧逐轨估计差 = 音画恒定错拍的根因）；
 * sharePlayoutDelayMs：-1=auto（按负载 14–32s 自适应，新默认）、0=低延迟、>0=手动）：
 * - 每帧携带发送端 captureTs（⑧ 发送端语义改为内容时刻）；transit 样本
 *   （本地到达 − captureTs）喂每窗口共享 min-tracking 估计。
 * - 帧播放时刻 = captureTs + X(窗口) + D。解码只在播放时刻前 DECODE_LOOKAHEAD_MS
 *   窗口内进行，不为"看了就丢"的帧浪费解码。
 * - 网络抖动/中继批量到达由缓冲吸收；D 由拥塞证据（容量截断/解码卡死/
 *   音频迟到/占用逼近）自动加深，30s 干净才缓降——延迟换流畅，尺寸绝不动。
 * - 欠载（帧未到期而队列空）时画面冻结在上一帧自然追平，不做撕裂式跳帧追赶。
 * - 压缩缓冲上限 = 目标深度 × 实测码率 EMA × 2.5（钳 [48,192]MB/窗）+
 *   全局总额 384MB 封顶：超限丢帧，绝不 OOM。
 *
 * 关键约束：
 * 1. 解码线程绝不碰 GL：createTexture/destroyTexture/upload 只允许在客户端（渲染）线程执行
 * 2. updateTextureRGBA（Portal 本地捕获）仍为同步路径，调用方必须保证在渲染线程
 * 3. 每窗口至多一个解码任务在途（decodeInFlight），完成后续调度，多窗口公平轮转
 * 4. 流状态（环形队列）与纹理生命周期解耦：纹理因尺寸变化重建不动流；时钟估计
 *    归 SharePlayoutController（窗口注销 removeWindow / 断线 clear 时同步失效）；
 *    窗口注销由 removeWindow() 显式清理（handleWindowListUpdate / clear）
 * 5. ⑧ 上传像素预算 UPLOAD_PIXEL_BUDGET_PER_TICK + 轮转：N 窗同 tick 到点不再
 *    钉死渲染线程；像素写入从逐像素 memPutInt 改为 scratch+memCopy（bulk）
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
	// ⑧ v1.4.8：单窗口缓冲上限不再是固定 48MB——那会把自适应深度（14–32s）
	// 在高码率下钳回浅缓冲（超限丢最旧=缓冲变浅=更卡，自伤）。现在由
	// SharePlayoutController.videoBufferCapBytes(目标深度, 实测码率EMA) 动态计算，
	// 另设全局总额上限（VIDEO_TOTAL_CAP_BYTES）拒绝 OOM。
	/**
	 * P1-7：单边像素上限（16384 = GL_MAX_TEXTURE_SIZE 的常见天花板）与总像素上限
	 * （16384²）。JPEG 头可以声称任意尺寸：1KB 恶意文件解出 40000×40000，
	 * getRGB/NativeImage 分配即 OOM。解码出尺寸后、分配像素数组前先校验。
	 */
	static final int MAX_DECODED_DIMENSION = 16384;
	static final long MAX_DECODED_PIXELS = 16384L * 16384L;
	/**
	 * P1-7：活跃 playout 流的全局上限（= 每 viewer 接受的"不同 windowHandle"上限）。
	 * 恶意/异常服务端可用海量 handle 撑爆 streamsByWindow（每条流含环形队列，
	 * 单窗口压缩缓冲最高 48MB），超限的新 handle 直接丢帧，不影响已建立的流。
	 */
	static final int MAX_ACTIVE_STREAMS = 64;
	/** stream 上限告警节流（10s 一条，避免丢帧风暴刷屏） */
	private volatile long lastStreamCapWarnMs = 0;
	/** ⑧ 全局缓冲总额封顶告警节流 */
	private volatile long lastGlobalCapWarnMs = 0;
	/** 允许在播放时刻前多久预解码（ms）：更远的帧不占解码线程 */
	private static final long DECODE_LOOKAHEAD_MS = 150;
	/**
	 * ⑧ v1.4.8：每渲染 tick 上传像素预算（≈4 路 1080p）。start all 后 N 条流
	 * 同 tick 全部到点时，逐窗 2M 像素循环 + GL 上传会把渲染线程钉死——
	 * 预算内的窗上传，预算外的留到下一 tick（decoded 队列丢最旧兜底），
	 * 表现从"整帧冻结一下"变成"个别窗低一两帧"，公平轮转不饿死。
	 */
	private static final long UPLOAD_PIXEL_BUDGET_PER_TICK = 8_000_000L;
	/** ⑧ 上传轮转起点（防饿死游标） */
	private int uploadRotation = 0;
	/** ⑧ 自适应采样驱动（1Hz）与上一样本周计数器快照 */
	private long lastPlayoutSampleMs = 0;
	private long lastSampleCapDrops = 0;
	private long lastSampleDecodeStalls = 0;
	private long lastSampleAudioStale = 0;
	/** 全局压缩缓冲总额（所有流之和；SharePlayoutController.VIDEO_TOTAL_CAP_BYTES 封顶） */
	private final java.util.concurrent.atomic.AtomicLong totalBufferedBytes = new java.util.concurrent.atomic.AtomicLong();
	
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
	
	/**
	 * 当前预缓冲深度（ms）。⑧ 语义：settings -1 = AUTO（按负载 14–32s 自适应）、
	 * 0 = 低延迟模式（上传最新帧，行为与旧版一致）、>0 = 手动档。
	 * 两轨（视频/音频）都经此单一入口 → 深度切换天然同刻生效。
	 */
	public static long playoutDelayMs() {
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc != null && wlc.settings != null) return SharePlayoutController.resolveDelayMs(wlc.settings.getSharePlayoutDelayMs());
		return SharePlayoutController.resolveDelayMs(SharePlayoutController.AUTO);
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

		// P1-7：handle 数不设限 → 活跃流全局上限。超限的新窗口句柄不建流、直接丢帧
		//（已建立的流不受影响；陈旧流由 cleanupOldTextures 的 60s 无流量兜底回收）。
		BufferedStream stream = streamsByWindow.get(windowHandle);
		if(stream == null) {
			if(streamsByWindow.size() >= MAX_ACTIVE_STREAMS) {
				long now2 = System.currentTimeMillis();
				if(now2 - lastStreamCapWarnMs > 10_000) {
					lastStreamCapWarnMs = now2;
					LOGGER.warn("Drop frame for unknown window 0x{}: active stream cap {} reached",
						Long.toHexString(windowHandle), MAX_ACTIVE_STREAMS);
				}
				return;
			}
			stream = streamsByWindow.computeIfAbsent(windowHandle, BufferedStream::new);
		}
		long now = System.currentTimeMillis();
		boolean shouldSchedule = false;
		boolean droppedIncoming = false;
		long delay = playoutDelayMs();
		synchronized(stream) {
			long prevArrival = stream.lastArrivalMs;
			stream.lastArrivalMs = now;
			long transit = now - captureTimestampMs;
			if(transit < 0) transit = 0; // 发送端时钟超前：按 0 处理，损失缓冲深度不破坏平滑
			// ⑧ 统一 playout 时钟：transit 样本喂给每窗口共享估计（音频轨喂同一条），
			// 两轨 dueAt = captureTs + 同一个 X + D → 音视频同刻播（旧逐轨估计差=恒定错拍）。
			SharePlayoutController.observeTransit(windowHandle, transit, now);
			// 实测码率 EMA（1/16）：动态容量公式的输入；异常间隔（心跳/长期静止）不采样
			long gap = now - prevArrival;
			if(gap > 0 && gap < 5000) {
				long inst = jpegData.length * 1000L / gap;
				stream.rateEmaBytesPerSec = stream.rateEmaBytesPerSec <= 0
					? inst
					: (15 * stream.rateEmaBytesPerSec + inst) / 16;
			}
			
			// 全局总额封顶：超限时新帧整体丢弃（保各流已有深度不被互抢清空）
			if(totalBufferedBytes.get() + jpegData.length > SharePlayoutController.VIDEO_TOTAL_CAP_BYTES) {
				stream.droppedByCap++;
				droppedIncoming = true;
			}
			else {
				stream.queue.addLast(new CompressedFrame(captureTimestampMs, frameSeq, x, y, width, height, jpegData));
				stream.bufferedBytes += jpegData.length;
				totalBufferedBytes.addAndGet(jpegData.length);
				// 低延迟模式（D=0）：等价旧"单槽覆盖"语义——只保留最新 2 帧
				// （1 帧在解码在途 + 1 帧待解），多余压缩帧直接丢弃，不浪费解码。
				if(delay <= 0) {
					while(stream.queue.size() > 2) {
						CompressedFrame dropped = stream.queue.removeFirst();
						stream.bufferedBytes -= dropped.jpegData.length;
						totalBufferedBytes.addAndGet(-dropped.jpegData.length);
						stream.droppedByCap++;
					}
				}
				// ⑧ 动态单窗上限：目标深度 × 实测码率（自适应缓冲的内存配套）
				long cap = SharePlayoutController.videoBufferCapBytes(delay, stream.rateEmaBytesPerSec);
				while(stream.bufferedBytes > cap && stream.queue.size() > 1) {
					CompressedFrame dropped = stream.queue.removeFirst();
					stream.bufferedBytes -= dropped.jpegData.length;
					totalBufferedBytes.addAndGet(-dropped.jpegData.length);
					stream.droppedByCap++;
				}
			}
			shouldSchedule = !stream.decodeInFlight;
		}
		if(droppedIncoming) {
			long warnAt = lastGlobalCapWarnMs;
			if(now - warnAt > 10_000) {
				lastGlobalCapWarnMs = now;
				LOGGER.warn("Drop incoming frame for window 0x{}: global buffer cap {} MB reached",
					Long.toHexString(windowHandle), SharePlayoutController.VIDEO_TOTAL_CAP_BYTES / (1024 * 1024));
			}
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
			// ⑧ 共享 playout 时钟（音频轨的样本也参与收敛——首包尖峰后两轨一起收敛）
			long offset = SharePlayoutController.transitMs(stream.windowHandle);
			if(offset < 0) offset = 0;
			long dueAt = head.captureTimestampMs + offset + playoutDelayMs() - DECODE_LOOKAHEAD_MS;
			if(now < dueAt) return;
			next = stream.queue.removeFirst();
			stream.bufferedBytes -= next.jpegData.length;
			totalBufferedBytes.addAndGet(-next.jpegData.length);
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
		long decodeStartMs = System.currentTimeMillis();
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
				// P1-7：恶意 JPEG 头可声称 40000×40000（1KB 文件 → 解出 16 亿像素）。
				// 在分配 int[]/纹理前拒绝超限尺寸（getRGB 与 createTexture 都不做尺寸校验）。
				if(!decodedDimensionsAllowed(decodedW, decodedH)) {
					LOGGER.warn("Reject decoded frame {}x{} (over pixel cap) from window stream", decodedW, decodedH);
					return;
				}
				// 批量获取所有像素（1次调用）
				argbPixels = bufferedImage.getRGB(0, 0, decodedW, decodedH, null, 0, decodedW);
			} catch(Exception e) {
				LOGGER.error("Failed to decode JPEG ({} bytes) on worker thread", job.jpegData.length, e);
				return;
			}
			
			offerDecoded(stream, new DecodedFrame(stream.windowHandle, job.x, job.y, decodedW, decodedH, argbPixels, job.captureTimestampMs, job.frameSeq));
		} finally {
			// ⑧ 解码卡死观测（>1.5s 一帧）：viewer 机器解码跟不上 → 自适应加深信号
			long decodeMs = System.currentTimeMillis() - decodeStartMs;
			if(streamsByWindow.get(stream.windowHandle) == stream) {
				boolean hadWork;
				synchronized(stream) {
					if(decodeMs > 1500) stream.decodeStalls++;
					stream.decodeInFlight = false;
					hadWork = !stream.queue.isEmpty();
				}
				if(hadWork) pumpStreamDecode(stream, System.currentTimeMillis());
			}
		}
	}
	
	/**
	 * P1-7 纯函数（可单测）：解码结果尺寸是否允许进入像素分配。
	 * 拒绝条件：非正尺寸、单边超过 GL 纹理上限、总像素超过 MAX_DECODED_PIXELS。
	 */
	static boolean decodedDimensionsAllowed(int width, int height) {
		if(width <= 0 || height <= 0) return false;
		if(width > MAX_DECODED_DIMENSION || height > MAX_DECODED_DIMENSION) return false;
		return (long) width * (long) height <= MAX_DECODED_PIXELS;
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
	 * ⑧ 每 tick 像素预算 + 轮转起点（start all 多流同刻到点时渲染线程不再被钉死）。
	 */
	private void flushPendingUploads() {
		if(decodedByWindow.isEmpty()) return;
		long now = System.currentTimeMillis();
		long delay = playoutDelayMs();
		java.util.List<UploadCandidate> candidates = new ArrayList<>();
		for(long windowHandle : new ArrayList<>(decodedByWindow.keySet())) {
			ArrayDeque<DecodedFrame> queue = decodedByWindow.get(windowHandle);
			if(queue == null) continue;
			// ⑧ 共享 playout 时钟（与解码调度、音频轨同一个估计 → 音画同刻）
			long offset = SharePlayoutController.transitMs(windowHandle);
			if(offset < 0) offset = 0;
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
				candidates.add(new UploadCandidate(windowHandle, (long)chosen.width * chosen.height, chosen));
			}
		}
		if(candidates.isEmpty()) return;
		int start = uploadRotation % candidates.size();
		uploadRotation = (uploadRotation + 1) % Math.max(1, candidates.size());
		for(DecodedFrame frame : pickUploads(candidates, start, UPLOAD_PIXEL_BUDGET_PER_TICK)) {
			applyDecodedFrame(frame);
		}
	}
	
	/** ⑧ 上传候选（窗口、像素量、待传帧） */
	record UploadCandidate(long windowHandle, long pixels, DecodedFrame frame) {}
	
	/**
	 * ⑧ 纯函数（可单测）：轮转起点 + 像素预算内选取本轮上传帧。
	 * 从 rotateStart 起环形扫描；预算不足则跳过该窗（留到下一 tick）；
	 * 但至少保证一个（单窗像素 > 预算时也要有画面，防整屏冻结）。
	 * 返回顺序即上传顺序。
	 */
	static java.util.List<DecodedFrame> pickUploads(java.util.List<UploadCandidate> candidates, int rotateStart, long pixelBudget) {
		java.util.List<DecodedFrame> selected = new ArrayList<>();
		int n = candidates.size();
		if(n == 0) return selected;
		long used = 0;
		for(int i = 0; i < n; i++) {
			UploadCandidate c = candidates.get(Math.floorMod(rotateStart + i, n));
			if(!selected.isEmpty() && used + c.pixels() > pixelBudget) continue;
			selected.add(c.frame());
			used += c.pixels();
		}
		return selected;
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
	 * 即 little-endian 内存 [R, G, B, A]（byte0 = R），
	 * 目标 int 值 = (a<<24)|(b<<16)|(g<<8)|r。
	 *
	 * ⑧ v1.4.8 重写：旧实现逐像素 memPutInt（1080p 每帧 ≈2M 次调用），
	 * start all 后 N 窗同 tick 上传直接钉死渲染线程。现在单次变换进共享
	 * scratch + 一次 memCopy——数组变换是普通 int 位运算（JIT 友好），
	 * 拷贝走原生 memcpy。scratch 只在渲染线程触碰（applyDecodedFrame 唯一
	 * 调用方，flushPendingUploads 由 tick 串行驱动），无需同步。
	 * OOM（窗口极大时 scratch 分配失败）自动回退逐像素旧路径。
	 */
	private static int[] bulkConvScratch;
	private static java.nio.ByteBuffer bulkConvBuf;
	private static java.nio.IntBuffer bulkConvView;
	
	/** ⑧ 纯函数（可单测）：ARGB → NativeImage int 布局（字节 [R,G,B,A]） */
	static int argbToNativeInt(int argb) {
		return (argb & 0xFF00FF00) | ((argb & 0xFF) << 16) | ((argb >>> 16) & 0xFF);
	}
	
	private static void writeArgbPixelsToNative(long ptr, int[] argbPixels) {
		int n = argbPixels.length;
		try {
			if(bulkConvScratch == null || bulkConvScratch.length < n) {
				bulkConvScratch = new int[n];
			}
			int[] conv = bulkConvScratch;
			for(int i = 0; i < n; i++) {
				conv[i] = argbToNativeInt(argbPixels[i]);
			}
			if(bulkConvBuf == null || bulkConvBuf.capacity() < n * 4) {
				if(bulkConvBuf != null) MemoryUtil.memFree(bulkConvBuf);
				bulkConvBuf = MemoryUtil.memAlloc(n * 4 + 4096);
				bulkConvView = bulkConvBuf.asIntBuffer();
			}
			bulkConvView.put(0, conv, 0, n);
			MemoryUtil.memCopy(MemoryUtil.memAddress(bulkConvBuf), ptr, (long)n * 4);
		} catch(OutOfMemoryError e) {
			// 极端大帧：释放 scratch 回退逐像素（慢但可用的旧行为），下帧再试 bulk
			if(bulkConvBuf != null) MemoryUtil.memFree(bulkConvBuf);
			bulkConvBuf = null;
			bulkConvView = null;
			bulkConvScratch = null;
			writeArgbPixelsToNativeScalar(ptr, argbPixels);
		}
	}
	
	/** 旧逐像素路径：仅作 OOM 回退保留（语义与 v1.4.7 完全一致） */
	private static void writeArgbPixelsToNativeScalar(long ptr, int[] argbPixels) {
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
				totalBufferedBytes.addAndGet(-stream.bufferedBytes);
				stream.queue.clear();
				stream.bufferedBytes = 0;
			}
		}
		decodedByWindow.remove(windowHandle);
		// ⑧ 共享 playout 时钟随流注销（音频侧经此句柄的观测一并失效，重建流时重新收敛）
		SharePlayoutController.forgetWindow(windowHandle);
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
		
		// ⑧ 1Hz 自适应采样（目标深度 14–32s 由负载证据伸缩；含过期共享时钟清扫）
		if(now - lastPlayoutSampleMs >= 1000) {
			lastPlayoutSampleMs = now;
			samplePlayoutMetrics(now);
		}
		
		tickCounter++;
		if(tickCounter >= CLEANUP_INTERVAL) {
			tickCounter = 0;
			cleanupOldTextures();
		}
	}
	
	/**
	 * ⑧ 汇总各流拥塞证据（周期增量）喂自适应控制器。信号选择刻意保守：
	 * 只有"缓冲容量截断丢帧/解码卡死/音频迟到丢弃/占用逼近上限"这类
	 * **卡顿的直接证据**才计拥塞；静止画面无帧到达（diff+心跳语义）不算。
	 */
	private void samplePlayoutMetrics(long now) {
		long capDrops = 0;
		long stalls = 0;
		double maxFill = 0;
		long delay = playoutDelayMs();
		for(BufferedStream stream : streamsByWindow.values()) {
			synchronized(stream) {
				capDrops += stream.droppedByCap;
				stalls += stream.decodeStalls;
				long cap = SharePlayoutController.videoBufferCapBytes(delay, stream.rateEmaBytesPerSec);
				if(cap > 0) maxFill = Math.max(maxFill, (double)stream.bufferedBytes / cap);
			}
		}
		long audioStale = 0;
		WaylandCraft wlc = WaylandCraft.instance;
		if(wlc != null && wlc.audioPlaybackManager != null) {
			audioStale = wlc.audioPlaybackManager.lateDroppedPackets();
		}
		SharePlayoutController.sample(now, new SharePlayoutController.PlayoutMetrics(
			capDrops - lastSampleCapDrops,
			stalls - lastSampleDecodeStalls,
			audioStale - lastSampleAudioStale,
			maxFill));
		lastSampleCapDrops = capDrops;
		lastSampleDecodeStalls = stalls;
		lastSampleAudioStale = audioStale;
	}
	
	public void clear() {
		streamsByWindow.clear();
		decodedByWindow.clear();
		totalBufferedBytes.set(0);
		SharePlayoutController.forgetAllWindows(); // 断线重连：所有窗口共享时钟清零重新收敛
		
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
		// ⑧ v1.4.8：transit 估计移交 SharePlayoutController 的每窗口共享时钟
		// （视频+音频两轨喂同一个估计 → 音画同锚；旧的逐轨 minTransitMs 是错拍根因）
		/** 实测码率 EMA（bytes/s，1/16）：动态缓冲容量公式输入 */
		long rateEmaBytesPerSec;
		/** ⑧ 解码超 1.5s 的次数（viewer 解码能力信号 → 自适应输入） */
		long decodeStalls;
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
	
	/** 工作线程解码结果（ARGB + captureTs），交由渲染线程按 playout 时钟上传。⑧ 包可见：上传选取纯函数单测 */
	static class DecodedFrame {
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
