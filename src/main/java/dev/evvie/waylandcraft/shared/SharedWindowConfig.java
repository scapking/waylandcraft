package dev.evvie.waylandcraft.shared;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * 共享窗口配置
 * 管理窗口共享的各种设置
 */
public class SharedWindowConfig {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-shared-config");
	
	// 默认配置
	private static final float DEFAULT_SCALE = 0.5f;
	private static final float DEFAULT_QUALITY = 0.7f;
	private static final int DEFAULT_MAX_FPS = 24;
	private static final boolean DEFAULT_SHARING_ENABLED = true;
	private static final boolean DEFAULT_AUTO_SHARE = false;
	private static final int DEFAULT_MAX_SHARED_WINDOWS = 10;
	private static final int DEFAULT_MAX_SUBSCRIBERS = 20;
	
	// 配置项
	private float scale = DEFAULT_SCALE;
	private float quality = DEFAULT_QUALITY;
	private int maxFps = DEFAULT_MAX_FPS;
	private boolean sharingEnabled = DEFAULT_SHARING_ENABLED;
	private boolean autoShare = DEFAULT_AUTO_SHARE;
	private int maxSharedWindows = DEFAULT_MAX_SHARED_WINDOWS;
	private int maxSubscribers = DEFAULT_MAX_SUBSCRIBERS;
	
	// 性能设置
	private boolean enableDiffUpdate = true;
	private boolean enableFrameRateControl = true;
	private boolean enableImageCompression = true;
	
	// 网络设置
	private int networkTimeout = 5000; // 毫秒
	private int retryCount = 3;
	private int bufferSize = 65536; // 64KB
	
	/**
	 * 构造函数
	 */
	public SharedWindowConfig() {
		loadDefaults();
	}
	
	/**
	 * 加载默认配置
	 */
	private void loadDefaults() {
		LOGGER.info("Loaded default shared window config");
	}
	
	/**
	 * 获取图像缩放比例
	 */
	public float getScale() {
		return scale;
	}
	
	/**
	 * 设置图像缩放比例
	 */
	public void setScale(float scale) {
		this.scale = Math.max(0.1f, Math.min(1.0f, scale));
		LOGGER.info("Scale set to {}", this.scale);
	}
	
	/**
	 * 获取JPEG质量
	 */
	public float getQuality() {
		return quality;
	}
	
	/**
	 * 设置JPEG质量
	 */
	public void setQuality(float quality) {
		this.quality = Math.max(0.1f, Math.min(1.0f, quality));
		LOGGER.info("Quality set to {}", this.quality);
	}
	
	/**
	 * 获取最大帧率
	 */
	public int getMaxFps() {
		return maxFps;
	}
	
	/**
	 * 设置最大帧率
	 */
	public void setMaxFps(int maxFps) {
		// 0 = 无限制（跟随性能上限）；>0 为硬上限（最高 240）
		this.maxFps = Math.max(0, Math.min(240, maxFps));
		LOGGER.info("Max FPS set to {}", this.maxFps);
	}
	
	/**
	 * 检查是否启用共享
	 */
	public boolean isSharingEnabled() {
		return sharingEnabled;
	}
	
	/**
	 * 启用/禁用共享
	 */
	public void setSharingEnabled(boolean enabled) {
		this.sharingEnabled = enabled;
		LOGGER.info("Sharing {}", enabled ? "enabled" : "disabled");
	}
	
	/**
	 * 检查是否自动共享新窗口
	 */
	public boolean isAutoShare() {
		return autoShare;
	}
	
	/**
	 * 设置自动共享
	 */
	public void setAutoShare(boolean autoShare) {
		this.autoShare = autoShare;
		LOGGER.info("Auto share {}", autoShare ? "enabled" : "disabled");
	}
	
	/**
	 * 获取最大共享窗口数
	 */
	public int getMaxSharedWindows() {
		return maxSharedWindows;
	}
	
	/**
	 * 设置最大共享窗口数
	 */
	public void setMaxSharedWindows(int maxSharedWindows) {
		this.maxSharedWindows = Math.max(1, Math.min(50, maxSharedWindows));
		LOGGER.info("Max shared windows set to {}", this.maxSharedWindows);
	}
	
	/**
	 * 获取最大订阅者数
	 */
	public int getMaxSubscribers() {
		return maxSubscribers;
	}
	
	/**
	 * 设置最大订阅者数
	 */
	public void setMaxSubscribers(int maxSubscribers) {
		this.maxSubscribers = Math.max(1, Math.min(100, maxSubscribers));
		LOGGER.info("Max subscribers set to {}", this.maxSubscribers);
	}
	
	/**
	 * 检查是否启用差分更新
	 */
	public boolean isDiffUpdateEnabled() {
		return enableDiffUpdate;
	}
	
	/**
	 * 启用/禁用差分更新
	 */
	public void setDiffUpdateEnabled(boolean enabled) {
		this.enableDiffUpdate = enabled;
		LOGGER.info("Diff update {}", enabled ? "enabled" : "disabled");
	}
	
	/**
	 * 检查是否启用帧率控制
	 */
	public boolean isFrameRateControlEnabled() {
		return enableFrameRateControl;
	}
	
	/**
	 * 启用/禁用帧率控制
	 */
	public void setFrameRateControlEnabled(boolean enabled) {
		this.enableFrameRateControl = enabled;
		LOGGER.info("Frame rate control {}", enabled ? "enabled" : "disabled");
	}
	
	/**
	 * 检查是否启用图像压缩
	 */
	public boolean isImageCompressionEnabled() {
		return enableImageCompression;
	}
	
	/**
	 * 启用/禁用图像压缩
	 */
	public void setImageCompressionEnabled(boolean enabled) {
		this.enableImageCompression = enabled;
		LOGGER.info("Image compression {}", enabled ? "enabled" : "disabled");
	}
	
	/**
	 * 获取网络超时时间
	 */
	public int getNetworkTimeout() {
		return networkTimeout;
	}
	
	/**
	 * 设置网络超时时间
	 */
	public void setNetworkTimeout(int timeout) {
		this.networkTimeout = Math.max(1000, Math.min(30000, timeout));
		LOGGER.info("Network timeout set to {}ms", this.networkTimeout);
	}
	
	/**
	 * 获取重试次数
	 */
	public int getRetryCount() {
		return retryCount;
	}
	
	/**
	 * 设置重试次数
	 */
	public void setRetryCount(int count) {
		this.retryCount = Math.max(0, Math.min(10, count));
		LOGGER.info("Retry count set to {}", this.retryCount);
	}
	
	/**
	 * 获取缓冲区大小
	 */
	public int getBufferSize() {
		return bufferSize;
	}
	
	/**
	 * 设置缓冲区大小
	 */
	public void setBufferSize(int size) {
		this.bufferSize = Math.max(1024, Math.min(1048576, size)); // 1KB - 1MB
		LOGGER.info("Buffer size set to {} bytes", this.bufferSize);
	}
	
	/**
	 * 获取捕获配置
	 */
	public ImageCapture.CaptureConfig getCaptureConfig() {
		return new ImageCapture.CaptureConfig(scale, quality, maxFps);
	}
	
	/**
	 * 重置为默认配置
	 */
	public void resetToDefaults() {
		this.scale = DEFAULT_SCALE;
		this.quality = DEFAULT_QUALITY;
		this.maxFps = DEFAULT_MAX_FPS;
		this.sharingEnabled = DEFAULT_SHARING_ENABLED;
		this.autoShare = DEFAULT_AUTO_SHARE;
		this.maxSharedWindows = DEFAULT_MAX_SHARED_WINDOWS;
		this.maxSubscribers = DEFAULT_MAX_SUBSCRIBERS;
		this.enableDiffUpdate = true;
		this.enableFrameRateControl = true;
		this.enableImageCompression = true;
		this.networkTimeout = 5000;
		this.retryCount = 3;
		this.bufferSize = 65536;
		
		LOGGER.info("Reset to default configuration");
	}
	
	/**
	 * 获取配置摘要
	 */
	public String getSummary() {
		return String.format(
			"Scale: %.1f, Quality: %.1f, MaxFPS: %d, Sharing: %s, AutoShare: %s, MaxWindows: %d, MaxSubs: %d",
			scale, quality, maxFps, sharingEnabled, autoShare, maxSharedWindows, maxSubscribers
		);
	}
}
