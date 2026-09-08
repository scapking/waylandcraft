package dev.evvie.waylandcraft.shared;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.nio.ByteBuffer;

/**
 * Opus 解码器包装器。
 * 使用 JNI 调用 libopus 进行音频解码。
 */
public class OpusDecoderWrapper {
    private static final Logger LOGGER = LoggerFactory.getLogger(OpusDecoderWrapper.class);
    
    private int sampleRate = 48000;
    private int channels = 2;
    
    private long nativeDecoderHandle = 0;
    private volatile boolean initialized = false;
    
    public void configure(int sampleRate, int channels) {
        this.sampleRate = sampleRate;
        this.channels = channels;
        
        if (initialized) {
            shutdown();
            initialize();
        }
        
        LOGGER.info("Opus decoder configured: {}Hz, {}ch", sampleRate, channels);
    }
    
    public synchronized void initialize() {
        if (initialized) return;
        
        nativeDecoderHandle = nativeCreateDecoder(sampleRate, channels);
        
        if (nativeDecoderHandle == 0) {
            throw new IllegalStateException("Failed to create Opus decoder");
        }
        
        initialized = true;
        LOGGER.info("Opus decoder initialized");
    }
    
    /**
     * 解码 Opus 数据回 PCM。
     * 输入: Opus 编码字节
     * 返回: 16-bit PCM 数据 (交织声道)
     */
    public byte[] decodeFrame(byte[] opusData) {
        if (!initialized) return new byte[0];
        if (opusData == null || opusData.length == 0) return new byte[0];
        
        return nativeDecode(nativeDecoderHandle, opusData);
    }
    
    public int getSampleRate() { return sampleRate; }
    public int getChannels() { return channels; }
    
    public void shutdown() {
        if (!initialized) return;
        
        if (nativeDecoderHandle != 0) {
            nativeDestroyDecoder(nativeDecoderHandle);
            nativeDecoderHandle = 0;
        }
        initialized = false;
        LOGGER.info("Opus decoder shutdown");
    }
    
    // ==================== Native Methods ====================
    
    private native long nativeCreateDecoder(int sampleRate, int channels);
    private native byte[] nativeDecode(long handle, byte[] opusData);
    private native void nativeDestroyDecoder(long handle);
}