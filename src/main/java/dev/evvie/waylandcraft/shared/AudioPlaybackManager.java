package dev.evvie.waylandcraft.shared;

import java.nio.ByteBuffer;
import java.nio.IntBuffer;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

import org.lwjgl.BufferUtils;
import org.lwjgl.openal.AL10;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import net.minecraft.client.Minecraft;

/**
 * 共享窗口音频播放管理器（接收端）。
 * 
 * 收到 SharedWindowAudioPayload 后，把 PCM 交给 OpenAL（Minecraft 自带的 lwjgl
 * OpenAL，复用其 device/context）流式播放：每窗口一个 source，持续 queue buffer。
 * 
 * 线程：OpenAL context 在 Minecraft 主线程创建，AL10 调用必须在主线程 ——
 * 网络线程收到包后通过 Minecraft.getInstance().execute() 排队到主线程处理。
 * 
 * 非定位播放（不设置 source 位置，像全局广播）；后续可做世界内 3D 定位。
 */
public class AudioPlaybackManager {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-audio-playback");
	
	/** OpenAL 格式常量（避免依赖混淆） */
	private static final int AL_FORMAT_MONO16 = 0x1101;
	private static final int AL_FORMAT_STEREO16 = 0x1103;
	
	/** source 上排队超过该数量（~6s @10包/s）时丢新包，保持实时 */
	private static final int MAX_QUEUED_BUFFERS = 60;
	
	/** windowHandle -> 播放流 */
	private final Map<Long, StreamHandle> streams = new ConcurrentHashMap<>();
	
	private boolean alAvailable = true;
	private boolean firstAudioLogged = false;
	private long totalAudioBytes = 0;
	private long nextAudioLogBytes = 1_000_000;
	private long receivedPackets = 0;
	
	/**
	 * 网络线程调用：入队到主线程播放。
	 */
	public void enqueue(long windowHandle, int sampleRate, int channels, byte[] pcm) {
		if(!alAvailable) return;
		receivedPackets++;
		
		if(!firstAudioLogged) {
			LOGGER.info("Audio playback: first packet received ({} bytes, {} Hz, {} ch, window {})",
				pcm.length, sampleRate, channels, Long.toHexString(windowHandle));
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
		mc.execute(() -> playOnMain(windowHandle, sampleRate, channels, data));
	}
	
	/**
	 * 主线程调用：OpenAL 流式播放。
	 */
	private void playOnMain(long windowHandle, int sampleRate, int channels, byte[] pcm) {
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
			
			// 排队过多：丢新包（接收端/播放跟不上时保持实时，不无限积压）
			if(stream.buffers.size() >= MAX_QUEUED_BUFFERS) {
				return;
			}
			
			int format = channels >= 2 ? AL_FORMAT_STEREO16 : AL_FORMAT_MONO16;
			int buffer = AL10.alGenBuffers();
			ByteBuffer bb = ByteBuffer.allocateDirect(pcm.length).put(pcm);
			bb.flip();
			AL10.alBufferData(buffer, format, bb, sampleRate);
			IntBuffer one = BufferUtils.createIntBuffer(1).put(buffer);
			one.flip();
			AL10.alSourceQueueBuffers(stream.source, one);
			stream.buffers.addLast(buffer);
			
			// 若 source 停了但还有排队数据 → 继续播放
			int state = AL10.alGetSourcei(stream.source, AL10.AL_SOURCE_STATE);
			if(state != AL10.AL_PLAYING && !stream.buffers.isEmpty()) {
				AL10.alSourcePlay(stream.source);
			}
		} catch(Throwable t) {
			// OpenAL 不可用（如某些环境未初始化）→ 静默降级，不影响共享画面
			LOGGER.warn("Audio playback unavailable: {}", t.toString());
			alAvailable = false;
		}
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
	 * 覆盖：接口(收到包数/字节) → 播放(OpenAL 是否可用、活跃流数、每流积压 buffer)。
	 */
	public String getStatusSummary() {
		StringBuilder sb = new StringBuilder();
		sb.append("接收端 (playback):\n");
		sb.append("  OpenAL: ").append(alAvailable ? "available" : "UNAVAILABLE (degraded)").append("\n");
		sb.append("  packets received: ").append(receivedPackets).append("\n");
		sb.append("  bytes received: ").append(totalAudioBytes).append("\n");
		sb.append("  active streams: ").append(streams.size()).append("\n");
		for(Map.Entry<Long, StreamHandle> e : streams.entrySet()) {
			StreamHandle h = e.getValue();
			sb.append("    window 0x").append(Long.toHexString(e.getKey()))
				.append(": ").append(h.sampleRate).append("Hz/").append(h.channels)
				.append("ch, queued buffers=").append(h.buffers.size())
				.append("/").append(MAX_QUEUED_BUFFERS).append("\n");
		}
		return sb.toString();
	}
	
	private static class StreamHandle {
		final int source;
		final java.util.ArrayDeque<Integer> buffers = new java.util.ArrayDeque<>();
		int sampleRate;
		int channels;
		
		StreamHandle(int sampleRate, int channels) {
			this.source = AL10.alGenSources();
			this.sampleRate = sampleRate;
			this.channels = channels;
			// 非定位：不设位置，音量 1.0
			AL10.alSourcef(source, AL10.AL_GAIN, 1.0f);
			AL10.alSourcei(source, AL10.AL_LOOPING, AL10.AL_FALSE);
		}
	}
}
