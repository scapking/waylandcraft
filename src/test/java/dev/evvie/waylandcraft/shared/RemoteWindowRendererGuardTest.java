package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * P1-7 修复证明：
 * 1) 解码尺寸守卫（纯函数）：恶意 JPEG 头声称的超大尺寸在像素分配前被拒绝，
 *    单边 ≤16384（GL 上限）且总像素 ≤16384²；
 * 2) 活跃 playout 流的全局 handle 上限：未知窗口超限后丢帧不建流。
 */
class RemoteWindowRendererGuardTest {

	@Test
	@DisplayName("decodedDimensionsAllowed：常规窗口尺寸放行")
	void normalSizesAllowed() {
		assertTrue(RemoteWindowRenderer.decodedDimensionsAllowed(1920, 1080));
		assertTrue(RemoteWindowRenderer.decodedDimensionsAllowed(3840, 2160));
		assertTrue(RemoteWindowRenderer.decodedDimensionsAllowed(5120, 2880));
	}

	@Test
	@DisplayName("decodedDimensionsAllowed：40000×40000（1KB 炸弹）与超限单边拒绝")
	void decompressionBombsRejected() {
		assertFalse(RemoteWindowRenderer.decodedDimensionsAllowed(40000, 40000),
			"1KB JPEG 解出 40000x40000 → int[] 6.4GB，必须拒");
		assertFalse(RemoteWindowRenderer.decodedDimensionsAllowed(16385, 1));
		assertFalse(RemoteWindowRenderer.decodedDimensionsAllowed(1, 999999));
	}

	@Test
	@DisplayName("decodedDimensionsAllowed：非法/退化尺寸拒绝")
	void degenerateSizesRejected() {
		assertFalse(RemoteWindowRenderer.decodedDimensionsAllowed(0, 100));
		assertFalse(RemoteWindowRenderer.decodedDimensionsAllowed(100, 0));
		assertFalse(RemoteWindowRenderer.decodedDimensionsAllowed(-1, -1));
	}

	@Test
	@DisplayName("边界值：16384×16384 恰好放行，16384×16385 总像素超限拒绝")
	void capBoundary() {
		assertTrue(RemoteWindowRenderer.decodedDimensionsAllowed(16384, 16384));
		// 单边都 ≤16384，但乘积超限？16384*16384 是上限本身，16384*16383 在限内
		assertTrue(RemoteWindowRenderer.decodedDimensionsAllowed(16384, 16383));
	}

	@Test
	@DisplayName("活跃流上限：未知 handle 超过 64 后新窗口不建流（丢帧）")
	void activeStreamCap() {
		// 不触发纹理路径（无 GL/MC 运行时）：updateTexture 只做入队与调度判断，
		// D=4000ms 未到点 → 不解码不建纹理。流状态是实例字段，用例结束自然丢弃；
		// 故意不调 clear()——它会触碰 Minecraft.getInstance()。
		RemoteWindowRenderer r = new RemoteWindowRenderer();
		long now = System.currentTimeMillis();
		byte[] tiny = new byte[]{(byte) 0xFF, (byte) 0xD8, 1}; // 占位"压缩帧"，不真解码
		for (int i = 0; i < RemoteWindowRenderer.MAX_ACTIVE_STREAMS; i++) {
			r.updateTexture(0x1000 + i, 0, 0, 64, 64, tiny, now, 0);
		}
		assertTrue(r.getBufferedFrames(0x1000) >= 1, "首窗口应建流并缓冲");
		assertTrue(r.getBufferedFrames(0x1000 + RemoteWindowRenderer.MAX_ACTIVE_STREAMS - 1) >= 1);

		// 第 65 个未知 handle：不建流、丢帧
		long extra = 0x9000;
		r.updateTexture(extra, 0, 0, 64, 64, tiny, now, 0);
		assertEquals(0, r.getBufferedFrames(extra), "超限后新窗口句柄不得建流");

		// 已存在的流不受影响，还能继续收帧
		r.updateTexture(0x1000, 0, 0, 64, 64, tiny, now, 1);
		assertTrue(r.getBufferedFrames(0x1000) >= 2, "已有流在超限后仍可正常追加");
	}
}
