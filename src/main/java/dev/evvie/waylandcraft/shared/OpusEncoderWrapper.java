package dev.evvie.waylandcraft.shared;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.ByteBuffer;

/**
 * Opus 编码器包装器。
 * 使用 JNI 调用 libopus 进行音频编码。
 * 目标: 低延迟、适合实时流媒体 (VoIP 模式)。
 */
public class OpusEncoderWrapper {
    private static final Logger LOGGER = LoggerFactory.getLogger(OpusEncoderWrapper.class);
    
    private int sampleRate = 48000;
    private int channels = 2;
    private int application = 2049; // OPUS_APPLICATION_VOIP
    private int bitrate = 64000;
    private int frameSize = 960; // 20ms @ 48kHz
    private int complexity = 10;
    
    private long nativeEncoderHandle = 0;
    private long nativeDecoderHandle = 0;
    private volatile boolean initialized = false;
    
    public void configure(int sampleRate, int channels, int bitrate) {
        this.sampleRate = sampleRate;
        this.channels = channels;
        this.bitrate = bitrate;
        this.frameSize = (sampleRate / 50) * channels; // 20ms frames
        
        if (initialized) {
            shutdown();
            initialize();
        }
        
        LOGGER.info("Opus encoder configured: {}Hz, {}ch, {}bps, frameSize={}", 
            sampleRate, channels, bitrate, frameSize);
    }
    
    public synchronized void initialize() {
        if (initialized) return;
        
        nativeEncoderHandle = nativeCreateEncoder(sampleRate, channels, application, bitrate, complexity);
        nativeDecoderHandle = nativeCreateDecoder(sampleRate, channels);
        
        if (nativeEncoderHandle == 0 || nativeDecoderHandle == 0) {
            throw new IllegalStateException("Failed to create Opus encoder/decoder");
        }
        
        nativeSetBitrate(nativeEncoderHandle, bitrate);
        nativeSetComplexity(nativeEncoderHandle, complexity);
        nativeSetSignal(nativeEncoderHandle, 3001); // OPUS_SIGNAL_VOICE
        
        initialized = true;
        LOGGER.info("Opus encoder initialized");
    }
    
    /**
     * 编码 PCM 帧到 Opus。
     * 输入: 16-bit PCM 数据 (交织声道)
     * 返回: Opus 编码字节
     */
    public byte[] encodeFrame(byte[] pcmData) {
        if (!initialized) return new byte[0];
        if (pcmData == null || pcmData.length == 0) return new byte[0];
        
        int expectedBytes = getFrameSizeBytes();
        if (pcmData.length != expectedBytes) {
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
        
        return nativeEncode(nativeEncoderHandle, pcmData);
    }
    
    /**
     * 解码 Opus 数据回 PCM。
     */
    public byte[] decodeFrame(byte[] opusData) {
        if (!initialized) return new byte[0];
        if (opusData == null || opusData.length == 0) return new byte[0];
        
        return nativeDecode(nativeDecoderHandle, opusData);
    }
    
    /**
     * 获取推荐的帧大小 (字节)。
     * 对于 48kHz 16bit 立体声，20ms 帧 = 3840 字节。
     */
    public int getFrameSizeBytes() {
        return (sampleRate / 50) * channels * 2; // 20ms, 16bit = 2 bytes per sample
    }
    
    public int getSampleRate() { return sampleRate; }
    public int getChannels() { return channels; }
    public int getBitrate() { return bitrate; }
    public int getFrameSize() { return frameSize; }
    
    public void setBitrate(int bitrate) {
        this.bitrate = bitrate;
        if (initialized) {
            nativeSetBitrate(nativeEncoderHandle, bitrate);
        }
    }
    
    public void setComplexity(int complexity) {
        this.complexity = Math.clamp(complexity, 0, 10);
        if (initialized) {
            nativeSetComplexity(nativeEncoderHandle, this.complexity);
        }
    }
    
    public void shutdown() {
        if (!initialized) return;
        
        if (nativeEncoderHandle != 0) {
            nativeDestroyEncoder(nativeEncoderHandle);
            nativeEncoderHandle = 0;
        }
        if (nativeDecoderHandle != 0) {
            nativeDestroyDecoder(nativeDecoderHandle);
            nativeDecoderHandle = 0;
        }
        initialized = false;
        LOGGER.info("Opus encoder shutdown");
    }
    
    // ==================== Native Methods ====================
    
    private native long nativeCreateEncoder(int sampleRate, int channels, int application, int bitrate, int complexity);
    private native long nativeCreateDecoder(int sampleRate, int channels);
    private native byte[] nativeEncode(long handle, byte[] pcmData);
    private native byte[] nativeDecode(long handle, byte[] opusData);
    private native void nativeSetBitrate(long handle, int bitrate);
    private native void nativeSetComplexity(long handle, int complexity);
    private native void nativeSetSignal(long handle, int signalType);
    private native void nativeDestroyEncoder(long handle);
    private native void nativeDestroyDecoder(long handle);
}