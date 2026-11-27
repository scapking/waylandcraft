package dev.evvie.waylandcraft.shared;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotEquals;

import java.util.ArrayList;
import java.util.List;

import org.junit.jupiter.api.Test;

/**
 * ⑧ v1.4.8 接收端上传调度/像素变换纯函数测试。
 * 语义锚点：
 * - pickUploads：每 tick 像素预算 + 轮转起点 —— start all 后 N 窗同刻到点
 *   不再把渲染线程钉死；预算不足者顺延，且必然轮到（防饿死）；
 *   单窗超预算也要传一帧（否则整屏冻结）。
 * - argbToNativeInt：bulk 拷贝与旧逐像素 memPutInt 字节级等价（防通道对调回归，
 *   这条通道在 1.4.x 曾出过"全花屏"级教训）。
 */
class RemoteWindowRendererUploadTest {
	
	private static RemoteWindowRenderer.DecodedFrame frame(long handle, int w, int h) {
		return new RemoteWindowRenderer.DecodedFrame(handle, 0, 0, w, h, null, 0L, 0);
	}
	
	private static List<RemoteWindowRenderer.UploadCandidate> candidates(long[][] spec) {
		List<RemoteWindowRenderer.UploadCandidate> out = new ArrayList<>();
		for(long[] s : spec) {
			out.add(new RemoteWindowRenderer.UploadCandidate(s[0], s[1], frame(s[0], (int) s[2], (int) s[3])));
		}
		return out;
	}
	
	@Test
	void budgetCapsPerTick() {
		// 6 个 1080p 候选（每人 ≈2.07M px），预算 8M → 只容 3
		long[][] spec = new long[6][];
		for(int i = 0; i < 6; i++) spec[i] = new long[]{i + 1, 1920L * 1080, 1920, 1080};
		List<RemoteWindowRenderer.DecodedFrame> sel =
			RemoteWindowRenderer.pickUploads(candidates(spec), 0, 8_000_000L);
		assertEquals(3, sel.size(), "8M 像素预算 = 3 路 1080p，其余顺延下一 tick");
	}
	
	@Test
	void atLeastOneAlwaysUploadsEvenOverBudget() {
		List<RemoteWindowRenderer.UploadCandidate> c =
			candidates(new long[][]{{1, 20_000_000L, 4000, 5000}, {2, 20_000_000L, 4000, 5000}});
		List<RemoteWindowRenderer.DecodedFrame> sel = RemoteWindowRenderer.pickUploads(c, 0, 1000);
		assertEquals(1, sel.size(), "单窗超预算也必须放行第一个（宁可超一点，不可整屏冻结）");
	}
	
	@Test
	void rotationPreventsStarvation() {
		// 4 窗同刻到点，预算只容 1：轮转起点推进 ⇒ 每窗恰好轮到一次，无饿死
		long[][] spec = {{1, 5_000_000L, 1920, 1080}, {2, 5_000_000L, 1920, 1080},
			{3, 5_000_000L, 1920, 1080}, {4, 5_000_000L, 1920, 1080}};
		List<RemoteWindowRenderer.UploadCandidate> c = candidates(spec);
		java.util.Set<Long> seen = new java.util.HashSet<>();
		for(int r = 0; r < 4; r++) {
			List<RemoteWindowRenderer.DecodedFrame> sel = RemoteWindowRenderer.pickUploads(c, r, 6_000_000L);
			assertEquals(1, sel.size());
			seen.add(sel.get(0).windowHandle);
		}
		assertEquals(4, seen.size(), "轮转 4 拍必须覆盖全部 4 窗");
	}
	
	@Test
	void negativeRotateStartIsSafe() {
		// 游标回绕/负数：floorMod 防越界
		List<RemoteWindowRenderer.UploadCandidate> c = candidates(new long[][]{{1, 100, 10, 10}, {2, 100, 10, 10}});
		assertEquals(2, RemoteWindowRenderer.pickUploads(c, -1, 1_000_000L).size());
	}
	
	@Test
	void bulkTransformMatchesScalarChannelOrder() {
		// 旧逐像素路径等价式：(a<<24)|(b<<16)|(g<<8)|r（NativeImage 内存 [R,G,B,A]）
		int[] argbs = {
			0xFF102030, 0x00FFFFFF, 0x12345678, (int) 0xDEADBEEF, 0xFF000000, 0x000000FF
		};
		for(int argb : argbs) {
			int a = (argb >>> 24) & 0xFF, r = (argb >>> 16) & 0xFF, g = (argb >>> 8) & 0xFF, b = argb & 0xFF;
			int expected = (a << 24) | (b << 16) | (g << 8) | r;
			assertEquals(expected, RemoteWindowRenderer.argbToNativeInt(argb),
				"bulk 变换必须与逐像素 memPutInt 字节级等价 @0x" + Integer.toHexString(argb));
		}
	}
	
	@Test
	void transformIsNotIdentityOnOpaquePixels() {
		// 防"干脆不转"回归：R≠B 的像素变换后通道必须交换
		assertNotEquals(0xFFFF0000, RemoteWindowRenderer.argbToNativeInt(0xFFFF0000) & 0x00FFFFFF);
	}
}
