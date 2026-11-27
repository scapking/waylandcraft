package dev.evvie.waylandcraft.shared;

import java.nio.ByteBuffer;
import java.nio.IntBuffer;
import java.util.ArrayDeque;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

import org.lwjgl.BufferUtils;
import org.lwjgl.openal.AL10;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import net.minecraft.client.Minecraft;
import net.minecraft.world.phys.Vec3;

import dev.evvie.waylandcraft.network.SharedWindowClientHandler;

/**
 * 共享窗口音频播放管理器（接收端）。
 * 
 * 收到 SharedWindowAudioPayload 后，把 PCM 交给 OpenAL（Minecraft 自带的 lwjgl
 * OpenAL，复用其 device/context）流式播放：每窗口一个 source，持续 queue buffer。
 * 
 * ⑦ 两项核心改造：
 * 1. playout 时钟对齐：音频包携带发送端 captureTs，按与视频相同的
 *    "captureTs + minTransit + sharePlayoutDelayMs" 到点才喂 OpenAL。
 *    视频预缓冲 4s 后，若不延迟音频，音画会错开整整一个 D——对齐后同帧同播。
 * 2. 可听范围衰减：每包喂入前按"玩家眼睛 ↔ 窗口 pivot"距离重算 AL_GAIN：
 *    距离 ≤ near（窗口物理尺寸近似）恒 1.0，到 range（audioShareRangeBlocks）
 *    线性衰减到 0；range=0 保持旧的全局广播行为。此前"范围不对"的根因就是
 *    完全没有衰减（任何距离全音量）+ 捕获整机声音被当成窗口声音广播。
 * 
 * 线程：OpenAL context 在 Minecraft 主线程创建，AL10 调用必须在主线程 ——
 * 网络线程收到包后通过 Minecraft.getInstance().execute() 排队到主线程处理。
 */
public class AudioPlaybackManager {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-audio-playback");
	
	/** OpenAL 格式常量（避免依赖混淆） */
	private static final int AL_FORMAT_MONO16 = 0x1101;
	private static final int AL_FORMAT_STEREO16 = 0x1103;
	
	/** source 上排队超过该数量（~6s @10包/s）时丢新包，保持实时（D=0 低延迟模式） */
	private static final int MAX_QUEUED_BUFFERS = 60;
	/** ⑧ pending 上限改由 SharePlayoutController.audioPendingChunks(目标深度) 动态给出 */
	private static final int MAX_PENDING_CHUNKS = 300; // 兜底下限（controller 公式同源钳制）
	/** 喂入 lookahead：播放时刻前这么多毫秒即可预排队进 OpenAL */
	private static final long FEED_LOOKAHEAD_MS = 150;
	/**
	 * ⑧ 迟到判据：到点 chunk 若已过期超过该毫秒数，丢弃而非补灌。
	 * 旧实现把欠账期间全部到点 chunk 一次性灌进 OpenAL 串行队列，断流恢复后
	 * 音频永久落后 playout 时钟（"疑似同步很久才好"的机制根源）；
	 * 丢弃重对齐 = 听感上一小段缺失，但音画相位不再累积。
	 */
	private static final long STALE_FEED_MS = 2000;
	/** ⑧ OpenAL 连续异常达到该次数才永久降级（旧版一次异常即毒杀整个会话音频） */
	private static final int AL_FATAL_CONSECUTIVE_FAILURES = 50;
	
	/** windowHandle -> 播放流 */
	private final Map<Long, StreamHandle> streams = new ConcurrentHashMap<>();
	
	private boolean alAvailable = true;
	private boolean firstAudioLogged = false;
	private long totalAudioBytes = 0;
	private long nextAudioLogBytes = 1_000_000;
	private long receivedPackets = 0;
	private long droppedStalePackets = 0;
	/** ⑧ 迟到丢弃的 chunk 总数（自适应采样的拥塞输入；渲染线程跨线程读 → volatile） */
	private volatile long lateDroppedPackets = 0;
	/** ⑧ OpenAL 连续失败计数（成功一次清零；达到阈值才永久降级） */
	private int consecutiveAlFailures = 0;
	
	/**
	 * 网络线程调用：入队到主线程播放。
	 * 
	 * @param captureTimestampMs 发送端捕获时刻（⑦ v2 协议字段）；&lt;=0 视为无时间戳，
	 *                           退化为本地接收即播（兼容/兜底）
	 */
	public void enqueue(long windowHandle, int sampleRate, int channels, byte[] pcm, long captureTimestampMs) {
		if(!alAvailable) return;
		receivedPackets++;
		
		if(!firstAudioLogged) {
			LOGGER.info("Audio playback: first packet received ({} bytes, {} Hz, {} ch, window {}, ts={})",
				pcm.length, sampleRate, channels, Long.toHexString(windowHandle), captureTimestampMs);
			firstAudioLogged = true;
		}
		totalAudioBytes += pcm.length;
		if(totalAudioBytes >= nextAudioLogBytes) {
			LOGGER.info("Audio playback: {} bytes received so far", totalAudioBytes);
			nextAudioLogBytes += 1_000_000;
		}
		
		Minecraft mc = Minecraft.getInstance();
		if(mc == null) return;
		
		byte[] data = pcm; // 拷贝一份，避免 netty 缓冲复用
		mc.execute(() -> playOnMain(windowHandle, sampleRate, channels, data, captureTimestampMs));
	}
	
	/**
	 * 主线程调用：OpenAL 流式播放（playout 到点喂入 + 范围衰减）。
	 */
	private void playOnMain(long windowHandle, int sampleRate, int channels, byte[] pcm, long captureTimestampMs) {
		if(!alAvailable) return;
		
		try {
			StreamHandle stream = streams.computeIfAbsent(windowHandle, h -> new StreamHandle(sampleRate, channels));
			
			// 采样率/声道变化（几乎不会）：重建
			if(stream.sampleRate != sampleRate || stream.channels != channels) {
				close(windowHandle);
				stream = streams.computeIfAbsent(windowHandle, h -> new StreamHandle(sampleRate, channels));
			}
			
			// 清理已播完的 buffer
			int processed = AL10.alGetSourcei(stream.source, AL10.AL_BUFFERS_PROCESSED);
			while(processed > 0 && !stream.buffers.isEmpty()) {
				int buf = stream.buffers.removeFirst();
				IntBuffer one = BufferUtils.createIntBuffer(1).put(buf);
				one.flip();
				AL10.alSourceUnqueueBuffers(stream.source, one);
				AL10.alDeleteBuffers(buf);
				processed--;
			}
			
			long now = System.currentTimeMillis();
			long playoutDelayMs = RemoteWindowRenderer.playoutDelayMs();
			boolean hasTs = captureTimestampMs > 0;
			// ⑧ 每窗口共享 playout 时钟：音频 transit 样本同样喂入（min-tracking 收敛
			// 更快，首包恰逢中继批量到达的尖峰会被后续好样本立即拉回——"音频卡很久
			// 才出现"的接收端放大器就此消除）
			long clockTransit = SharePlayoutController.transitMs(windowHandle);
			if(clockTransit < 0) clockTransit = 0;
			
			if(hasTs && playoutDelayMs > 0) {
				// === 预缓冲模式：与视频同一 playout 时钟（同一 X + 同一 D → 同刻播） ===
				SharePlayoutController.observeTransit(windowHandle, Math.max(0, now - captureTimestampMs), now);
				clockTransit = Math.max(clockTransit, 0); // 首样本后本包按新估计排期
				stream.pending.addLast(new PendingChunk(pcm, sampleRate, channels, captureTimestampMs));
				// ⑧ pending 上限随目标深度伸缩（32s 档 ≈360 包；固定 300 会把最深档钳回 30s）
				int pendingCap = SharePlayoutController.audioPendingChunks((int)Math.min(playoutDelayMs, Integer.MAX_VALUE));
				while(stream.pending.size() > pendingCap) {
					stream.pending.removeFirst(); // 极端积压：丢最旧（音画一起滞后不如局部丢）
					droppedStalePackets++;
				}
			}
			else {
				// === 低延迟/无时间戳兜底：直接排队（旧行为，含 60 buffer 封顶丢新包） ===
				if(stream.buffers.size() >= MAX_QUEUED_BUFFERS) {
					return;
				}
				queueToOpenAL(stream, pcm, sampleRate, channels);
			}
			
			// 到点的 pending 全部喂入（head 未到点则停——保序）
			if(!stream.pending.isEmpty()) {
				while(!stream.pending.isEmpty()) {
					PendingChunk head = stream.pending.peekFirst();
					long dueAt = head.captureTimestampMs + clockTransit + playoutDelayMs;
					if(now + FEED_LOOKAHEAD_MS < dueAt) break;
					stream.pending.removeFirst();
					// ⑧ 迟到超过 STALE_FEED_MS 的 chunk 丢弃重对齐（旧：全部补灌 → OpenAL
					// 串行队列永久落后 playout 时钟 = 断流一次、错拍到天荒地老）
					if(now - dueAt > STALE_FEED_MS) {
						lateDroppedPackets++;
						continue;
					}
					queueToOpenAL(stream, head.pcm, head.sampleRate, head.channels);
				}
			}
			
			// 范围衰减：每次喂包（~10Hz）刷新一次 gain，空间移动的可懂度足够
			updateGain(stream, windowHandle);
			
			// 若 source 停了但还有排队数据 → 继续播放
			int state = AL10.alGetSourcei(stream.source, AL10.AL_SOURCE_STATE);
			if(state != AL10.AL_PLAYING && !stream.buffers.isEmpty()) {
				AL10.alSourcePlay(stream.source);
			}
			consecutiveAlFailures = 0;
		} catch(Throwable t) {
			// ⑧ 一次性异常不再永久毒杀本会话音频（旧行为：任何 Throwable →
			// alAvailable=false 且无恢复路径，一次瞬时 AL 错误=整局无声）。
			// 仅 LinkageError（OpenAL 真不存在）或连续 ≥50 次失败才降级。
			consecutiveAlFailures++;
			if(t instanceof LinkageError || consecutiveAlFailures >= AL_FATAL_CONSECUTIVE_FAILURES) {
				LOGGER.warn("Audio playback unavailable (degraded after {} consecutive failures): {}",
					consecutiveAlFailures, t.toString());
				alAvailable = false;
			}
			else if(consecutiveAlFailures == 1 || consecutiveAlFailures % 20 == 0) {
				LOGGER.warn("Audio playback hiccup ({} consecutive): {} — retrying",
					consecutiveAlFailures, t.toString());
			}
		}
	}
	
	private void queueToOpenAL(StreamHandle stream, byte[] pcm, int sampleRate, int channels) {
		int format = channels >= 2 ? AL_FORMAT_STEREO16 : AL_FORMAT_MONO16;
		int buffer = AL10.alGenBuffers();
		ByteBuffer bb = ByteBuffer.allocateDirect(pcm.length).put(pcm);
		bb.flip();
		AL10.alBufferData(buffer, format, bb, sampleRate);
		IntBuffer one = BufferUtils.createIntBuffer(1).put(buffer);
		one.flip();
		AL10.alSourceQueueBuffers(stream.source, one);
		stream.buffers.addLast(buffer);
	}
	
	/**
	 * ⑦ "范围不对"修复：按玩家与窗口 pivot 的距离做线性衰减。
	 * range=0（settings 缺失或显式关闭）→ 恒 1.0，保持旧全局广播行为。
	 * near = 窗口锚定距离量级（2 格），窗内恒满量避免近距闪烁。
	 */
	private void updateGain(StreamHandle stream, long windowHandle) {
		double range = audioRangeBlocks();
		if(range <= 0) {
			AL10.alSourcef(stream.source, AL10.AL_GAIN, 1.0f);
			return;
		}
		Minecraft mc = Minecraft.getInstance();
		Vec3 windowPos = SharedWindowClientHandler.getWindowPivot(windowHandle);
		if(mc.player == null || windowPos == null) return; // 信息不足保持上次 gain
		
		Vec3 ear = mc.player.getEyePosition();
		double dist = ear.distanceTo(windowPos);
		double near = 2.0;
		float gain = dist <= near ? 1.0f : (float)Math.max(0.0, 1.0 - (dist - near) / (range - near));
		AL10.alSourcef(stream.source, AL10.AL_GAIN, gain);
	}
	
	private static double audioRangeBlocks() {
		dev.evvie.waylandcraft.WaylandCraft wlc = dev.evvie.waylandcraft.WaylandCraft.instance;
		if(wlc != null && wlc.settings != null) return wlc.settings.getAudioShareRangeBlocks();
		return 32.0;
	}
	
	/**
	 * 停止并释放某个窗口的播放流（窗口注销/断线时调用）。
	 */
	public void close(long windowHandle) {
		StreamHandle stream = streams.remove(windowHandle);
		if(stream == null) return;
		try {
			AL10.alSourceStop(stream.source);
			for(int buf : stream.buffers) {
				AL10.alDeleteBuffers(buf);
			}
			stream.buffers.clear();
			stream.pending.clear();
			AL10.alDeleteSources(stream.source);
		} catch(Throwable t) {
			LOGGER.debug("Audio cleanup failed for {}", Long.toHexString(windowHandle));
		}
	}
	
	/**
	 * 清空全部播放流（断线时调用）。
	 */
	public void closeAll() {
		for(long handle : streams.keySet()) {
			close(handle);
		}
		streams.clear();
	}
	
	/**
	 * 接收端全链路状态（供 /wl audio status 展示）。
	 * 覆盖：接口(收到包数/字节) → 缓冲(pending/minTransit) → 播放(OpenAL 可用性、活跃流、积压)。
	 */
	public String getStatusSummary() {
		StringBuilder sb = new StringBuilder();
		sb.append("接收端 (playback):\n");
		sb.append("  OpenAL: ").append(alAvailable ? "available" : "UNAVAILABLE (degraded)").append("\n");
		sb.append("  packets received: ").append(receivedPackets).append("\n");
		sb.append("  bytes received: ").append(totalAudioBytes).append("\n");
		sb.append("  stale dropped: ").append(droppedStalePackets).append("\n");
		sb.append("  ⑧ late dropped (realign): ").append(lateDroppedPackets).append("\n");
		sb.append("  playoutDelayMs: ").append(RemoteWindowRenderer.playoutDelayMs()).append("\n");
		sb.append("  audioRangeBlocks: ").append(audioRangeBlocks()).append("\n");
		sb.append("  active streams: ").append(streams.size()).append("\n");
		for(Map.Entry<Long, StreamHandle> e : streams.entrySet()) {
			StreamHandle h = e.getValue();
			sb.append("    window 0x").append(Long.toHexString(e.getKey()))
				.append(": ").append(h.sampleRate).append("Hz/").append(h.channels)
				.append("ch, queued buffers=").append(h.buffers.size())
				.append(", pending=").append(h.pending.size())
				// ⑧ minTransit 来自每窗口共享 playout 时钟（视频/音频同估计=同锚）
				.append(", sharedTransit=").append(SharePlayoutController.transitMs(e.getKey())).append("ms")
				.append("\n");
		}
		return sb.toString();
	}
	
	/** ⑧ 自适应采样输入：迟到丢弃累计（渲染线程读，主线程写，volatile 语义足够） */
	public long lateDroppedPackets() {
		return lateDroppedPackets;
	}
	
	private static class PendingChunk {
		final byte[] pcm;
		final int sampleRate;
		final int channels;
		final long captureTimestampMs;
		PendingChunk(byte[] pcm, int sampleRate, int channels, long captureTimestampMs) {
			this.pcm = pcm;
			this.sampleRate = sampleRate;
			this.channels = channels;
			this.captureTimestampMs = captureTimestampMs;
		}
	}
	
	private static class StreamHandle {
		final int source;
		final java.util.ArrayDeque<Integer> buffers = new java.util.ArrayDeque<>();
		final ArrayDeque<PendingChunk> pending = new ArrayDeque<>();
		int sampleRate;
		int channels;
		// ⑧ v1.4.8：逐轨 minTransit 估计已废除——视频/音频共用
		// SharePlayoutController 的每窗口共享 playout 时钟（音画同锚，消除恒定错拍）
		
		StreamHandle(int sampleRate, int channels) {
			this.source = AL10.alGenSources();
			this.sampleRate = sampleRate;
			this.channels = channels;
			// 初始增益 1.0；⑦ 起实际 gain 由 updateGain 按窗口距离每包刷新
			AL10.alSourcef(source, AL10.AL_GAIN, 1.0f);
			AL10.alSourcei(source, AL10.AL_LOOPING, AL10.AL_FALSE);
		}
	}
}
