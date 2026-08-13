package dev.evvie.waylandcraft.shared;

import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * 帧率控制器
 * 控制窗口图像更新的帧率，优化网络带宽
 */
public class FrameRateController {
	
	private static final Logger LOGGER = LoggerFactory.getLogger("waylandcraft-framerate");
	
	// 窗口帧率控制: windowHandle -> FrameData
	private final Map<Long, FrameData> frameDataMap = new ConcurrentHashMap<>();
	
	// 默认最大帧率
	private static final int DEFAULT_MAX_FPS = 30;
	
	// 最小帧率（仅用于 getRecommendedFps/adjustFpsByDiff 的动态档位下限）
	private static final int MIN_FPS = 5;
	
	// 硬上限（可自定义帧率的上限；0 表示无限制，由调用方特殊处理）
	private static final int MAX_FPS = 240;
	
	/**
	 * 检查是否应该更新指定窗口
	 * @param windowHandle 窗口句柄
	 * @return true如果应该更新
	 */
	public boolean shouldUpdate(long windowHandle) {
		return shouldUpdate(windowHandle, DEFAULT_MAX_FPS);
	}
	
	/**
	 * 检查是否应该更新指定窗口
	 * @param windowHandle 窗口句柄
	 * @param maxFps 最大帧率
	 * @return true如果应该更新
	 */
	public boolean shouldUpdate(long windowHandle, int maxFps) {
		// maxFps <= 0：无限制 —— 跟随渲染帧率与编码能力，每帧都允许更新。
		// 实际吞吐仍受 diff 检测 + 码率限速自然节流，不会无脑每帧发。
		if(maxFps <= 0) {
			return true;
		}
		
		long now = System.currentTimeMillis();
		FrameData data = frameDataMap.computeIfAbsent(windowHandle, k -> new FrameData());
		
		long minInterval = 1000L / Math.max(MIN_FPS, Math.min(MAX_FPS, maxFps));
		
		if(now - data.lastUpdateTime < minInterval) {
			return false;
		}
		
		// 记录帧间隔
		if(data.lastUpdateTime > 0) {
			data.lastInterval = now - data.lastUpdateTime;
		}
		data.lastUpdateTime = now;
		data.frameCount++;
		
		return true;
	}
	
	/**
	 * 获取推荐的帧率
	 * 基于网络状况和窗口大小动态调整
	 */
	public int getRecommendedFps(long windowHandle, int width, int height, int subscriberCount) {
		// 基础帧率
		int baseFps = DEFAULT_MAX_FPS;
		
		// 根据窗口大小调整
		int pixelCount = width * height;
		if(pixelCount > 1920 * 1080) {
			baseFps = 15; // 大窗口：低帧率
		} else if(pixelCount > 1280 * 720) {
			baseFps = 20; // 中等窗口
		} else {
			baseFps = 30; // 小窗口：高帧率
		}
		
		// 根据订阅者数量调整
		if(subscriberCount > 10) {
			baseFps = Math.max(MIN_FPS, baseFps - 10);
		} else if(subscriberCount > 5) {
			baseFps = Math.max(MIN_FPS, baseFps - 5);
		}
		
		return baseFps;
	}
	
	/**
	 * 计算帧间差异
	 * @return 差异百分比 (0.0 - 1.0)
	 */
	public float calculateFrameDiff(long windowHandle, byte[] previousFrame, byte[] currentFrame) {
		if(previousFrame == null || currentFrame == null) {
			return 1.0f;
		}
		
		if(previousFrame.length != currentFrame.length) {
			return 1.0f;
		}
		
		// 采样计算差异（提高性能）
		int sampleSize = Math.min(1000, previousFrame.length);
		int step = previousFrame.length / sampleSize;
		
		int diffCount = 0;
		for(int i = 0; i < previousFrame.length; i += step) {
			if(previousFrame[i] != currentFrame[i]) {
				diffCount++;
			}
		}
		
		return (float)diffCount / sampleSize;
	}
	
	/**
	 * 根据差异调整帧率
	 */
	public int adjustFpsByDiff(int currentFps, float diffPercent) {
		if(diffPercent > 0.5f) {
			// 大量变化：提高帧率以跟上更新
			return Math.min(MAX_FPS, currentFps + 5);
		} else if(diffPercent < 0.1f) {
			// 几乎无变化：降低帧率节省带宽
			return Math.max(MIN_FPS, currentFps - 2);
		}
		return currentFps;
	}
	
	/**
	 * 重置窗口帧数据
	 */
	public void reset(long windowHandle) {
		frameDataMap.remove(windowHandle);
	}
	
	/**
	 * 清理所有帧数据
	 */
	public void clear() {
		frameDataMap.clear();
	}
	
	/**
	 * 获取当前窗口的实际帧率（基于最近帧间隔估算）
	 */
	public int getCurrentFps(long windowHandle) {
		FrameData data = frameDataMap.get(windowHandle);
		if(data == null || data.frameCount < 2) return 0;
		long elapsed = System.currentTimeMillis() - data.lastUpdateTime;
		if(elapsed <= 0) return 0;
		// 简单估算：用最近的帧间隔
		return (int)(1000.0 / Math.max(1, data.lastInterval));
	}
	
	/**
	 * 帧数据内部类
	 */
	private static class FrameData {
		long lastUpdateTime = 0;
		long frameCount = 0;
		long lastInterval = 50; // 最近一次帧间隔（ms）
	}
}
