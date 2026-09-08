// Audio capture buffer management - implements 4-8s client/server buffering strategy
// to solve audio stutter issues (reference: Discord Go Live architecture)
package dev.evvie.waylandcraft.shared;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicReference;

public class AudioBufferManager {
    private static final int TARGET_BUFFER_MS = 6000;   // 6s target buffer
    private static final int MIN_BUFFER_MS = 4000;      // 4s minimum
    private static final int MAX_BUFFER_MS = 8000;      // 8s maximum
    private static final int SAMPLE_RATE = 48000;
    private static final int CHANNELS = 2;
    private static final int FRAME_SIZE_MS = 20;        // 20ms frames
    private static final int FRAME_SAMPLES = SAMPLE_RATE * FRAME_SIZE_MS / 1000; // 960
    private static final int FRAME_BYTES = FRAME_SAMPLES * CHANNELS * 2; // 3840 bytes (16-bit)

    private final ConcurrentLinkedQueue<AudioFrame> captureQueue = new ConcurrentLinkedQueue<>();
    private final ConcurrentLinkedQueue<EncodedAudioPacket> outputQueue = new ConcurrentLinkedQueue<>();
    private final AtomicLong totalCapturedBytes = new AtomicLong(0);
    private final AtomicLong totalSentBytes = new AtomicLong(0);
    private final AtomicLong lastPollTime = new AtomicLong(0);
    private final AtomicReference<BufferState> state = new AtomicReference<>(new BufferState());

    public record AudioFrame(byte[] data, long timestampMs, int sampleRate, int channels) {}
    public record EncodedAudioPacket(byte[] data, long timestampMs, int sequence) {}

    public static class BufferState {
        long bufferedMs = 0;
        int queuedFrames = 0;
        long lastFrameTimestamp = 0;
        boolean underrun = false;
        boolean overrun = false;
    }

    public void enqueueFrame(byte[] pcmData, long timestampMs, int sampleRate, int channels) {
        if (pcmData == null || pcmData.length == 0) return;
        
        // Validate frame size
        int expectedBytes = (sampleRate / 50) * channels * 2; // 20ms frame
        if (pcmData.length != expectedBytes) {
            // Resize if needed
            if (pcmData.length < expectedBytes) {
                byte[] padded = new byte[expectedBytes];
                System.arraycopy(pcmData, 0, padded, 0, pcmData.length);
                pcmData = padded;
            } else {
                byte[] truncated = new byte[expectedBytes];
                System.arraycopy(pcmData, 0, truncated, 0, expectedBytes);
                pcmData = truncated;
            }
        }

        AudioFrame frame = new AudioFrame(pcmData, timestampMs, sampleRate, channels);
        captureQueue.offer(frame);
        totalCapturedBytes.addAndGet(pcmData.length);
        
        // Check buffer overrun
        long bufferedMs = getBufferedDurationMs();
        if (bufferedMs > MAX_BUFFER_MS) {
            // Drop oldest frames to maintain buffer within limits
            AudioFrame dropped;
            while ((dropped = captureQueue.poll()) != null && getBufferedDurationMs() > MAX_BUFFER_MS) {
                // Dropped
            }
            state.getAndUpdate(s -> { s.overrun = true; return s; });
        }
        
        updateState();
    }

    public AudioFrame pollFrame() {
        AudioFrame frame = captureQueue.poll();
        if (frame != null) {
            updateState();
        }
        return frame;
    }

    public EncodedAudioPacket pollPacket() {
        return outputQueue.poll();
    }

    public void enqueuePacket(EncodedAudioPacket packet) {
        outputQueue.offer(packet);
        totalSentBytes.addAndGet(packet.data().length);
    }

    public long getBufferedDurationMs() {
        int frames = captureQueue.size();
        return (long) frames * FRAME_SIZE_MS;
    }

    public int getOutputQueueSize() {
        return outputQueue.size();
    }

    public BufferState getState() {
        return state.get();
    }

    public long getTotalCapturedBytes() {
        return totalCapturedBytes.get();
    }

    public long getTotalSentBytes() {
        return totalSentBytes.get();
    }

    public void reset() {
        captureQueue.clear();
        outputQueue.clear();
        totalCapturedBytes.set(0);
        totalSentBytes.set(0);
        state.set(new BufferState());
    }

    private void updateState() {
        long bufferedMs = getBufferedDurationMs();
        state.getAndUpdate(s -> {
            s.bufferedMs = bufferedMs;
            s.queuedFrames = captureQueue.size();
            if (!captureQueue.isEmpty()) {
                s.lastFrameTimestamp = captureQueue.peek().timestampMs();
            }
            s.underrun = bufferedMs < MIN_BUFFER_MS;
            return s;
        });
    }
}