import java.util.*;

/**
 * 独立逻辑测试：验证 WindowLayoutManager.swapCore 的纯算法部分。
 * 不依赖 Minecraft，模拟 ordered 列表 + layerSizes（每层窗口数）。
 *
 * 复刻 swapCore 的方向计算（与源码一致的算法），验证：
 * 1. 单层 3 窗口：左右交换正确，最左按右→环绕到最右，最右按左→环绕到最左
 * 2. 单窗口：无邻居 → false
 * 3. 两层 [3,2]：上/下跨层同槽位交换
 * 4. 环绕：第一层按上 → 环绕到最底层同槽位；最后一层按下 → 环绕到最上层同槽位
 * 5. swap 后 ordered 顺序 + layoutAltIndex 同时互换（cube/sphere 都生效）
 */
public class TestSwapCore {

	static class Win {
		final String name;
		int altIndex;
		double pivotAngle; // 相对中心的方位角（模拟 angleOf）
		Win(String name, int altIndex, double angle) { this.name = name; this.altIndex = altIndex; this.pivotAngle = angle; }
		public String toString() { return name + "(alt=" + altIndex + ")"; }
	}

	static class Layout {
		List<Win> ordered = new ArrayList<>();
		int coreIdx;
		List<Integer> layerSizes = new ArrayList<>();

		int indexOfCore() { return coreIdx; }

		int layerStartOf(int idx) {
			int acc = 0;
			for(int size : layerSizes) {
				if(idx < acc + size) return acc;
				acc += size;
			}
			return 0;
		}

		int layerSizeAt(int idx) {
			int acc = 0;
			for(int size : layerSizes) {
				if(idx < acc + size) return size;
				acc += size;
			}
			return layerSizes.isEmpty() ? 1 : layerSizes.get(layerSizes.size() - 1);
		}

		int prevLayerStart(int start) {
			int acc = 0;
			for(int size : layerSizes) {
				if(acc + size >= start) return acc;
				acc += size;
			}
			return 0;
		}

		/** findLayerNeighbor 复刻：同层内按方位角找左/右最近窗口（环绕） */
		int findLayerNeighbor(int idx, int start, int size, int dir) {
			if(size <= 1) return idx;
			Win core = ordered.get(idx);
			double a = core.pivotAngle;
			int best = -1;
			double bestDiff = 0;
			for(int i = start; i < start + size; i++) {
				if(i == idx) continue;
				double d = ordered.get(i).pivotAngle;
				double diff = dir > 0 ? d - a : a - d;
				if(diff < 0) diff += Math.PI * 2;
				if(best < 0 || diff < bestDiff) { best = i; bestDiff = diff; }
			}
			return best < 0 ? idx : best;
		}

		boolean swapCore(int dir) {
			if(ordered.isEmpty()) return false;
			int n = ordered.size();
			int idx = indexOfCore();
			if(idx < 0) return false;

			int start = layerStartOf(idx);
			int size = layerSizeAt(idx);
			int slot = idx - start;
			int next;

			switch(dir) {
				case 0: { // 上
					if(start == 0) {
						int lastSize = layerSizes.isEmpty() ? n : layerSizes.get(layerSizes.size() - 1);
						int lastStart = n - lastSize;
						next = lastStart + Math.min(slot, lastSize - 1);
					} else {
						int prevStart = prevLayerStart(start);
						int prevSize = start - prevStart;
						next = prevStart + Math.min(slot, prevSize - 1);
					}
					break;
				}
				case 1: { // 下
					int nextStart = start + size;
					if(nextStart >= n) {
						int firstSize = layerSizes.isEmpty() ? 1 : layerSizes.get(0);
						next = Math.min(slot, firstSize - 1);
					} else {
						int nextSize = layerSizeAt(nextStart);
						next = nextStart + Math.min(slot, nextSize - 1);
					}
					break;
				}
				case 2: next = findLayerNeighbor(idx, start, size, -1); break;
				default: next = findLayerNeighbor(idx, start, size, +1); break;
			}

			if(next < 0 || next >= n || next == idx) return false;

			Win a = ordered.get(idx);
			Win b = ordered.get(next);
			Collections.swap(ordered, idx, next);
			int tmpAlt = a.altIndex;
			a.altIndex = b.altIndex;
			b.altIndex = tmpAlt;
			coreIdx = next; // 源码中 coreHandle 不变（核心窗口跟随移动），这里模拟核心窗口位置变化
			return true;
		}
	}

	static int passed = 0, failed = 0;

	static void check(String name, boolean cond) {
		if(cond) { passed++; System.out.println("PASS: " + name); }
		else { failed++; System.out.println("FAIL: " + name); }
	}

	static Layout singleLayer3() {
		Layout l = new Layout();
		l.ordered.add(new Win("W0核心", 0, 0.0));
		l.ordered.add(new Win("W1右", 1, 0.8));
		l.ordered.add(new Win("W2左", 2, -0.8));
		l.coreIdx = 0;
		l.layerSizes = Arrays.asList(3);
		return l;
	}

	static Layout twoLayer() {
		Layout l = new Layout();
		// 层0: W0(核心,alt0) W1(alt1) W2(alt2)
		// 层1: W3(alt3) W4(alt4)
		l.ordered.add(new Win("W0核心", 0, 0.0));
		l.ordered.add(new Win("W1", 1, 0.8));
		l.ordered.add(new Win("W2", 2, -0.8));
		l.ordered.add(new Win("W3", 3, 0.4));
		l.ordered.add(new Win("W4", 4, -0.4));
		l.coreIdx = 0;
		l.layerSizes = Arrays.asList(3, 2);
		return l;
	}

	public static void main(String[] args) {
		// 1. 单层 3 窗口：核心在 0，右 → 与 W1 交换
		Layout l = singleLayer3();
		boolean ok = l.swapCore(3); // 右
		check("单层3窗: 核心右交换成功", ok);
		check("单层3窗: 右换后 W1 到核心位(ordered[0]=W1右)", l.ordered.get(0).name.equals("W1右"));
		check("单层3窗: 右换后 核心到右位(ordered[1]=W0核心)", l.ordered.get(1).name.equals("W0核心"));
		check("单层3窗: altIndex 互换(W0.alt=1 W1.alt=0)", l.ordered.get(0).altIndex == 0 && l.ordered.get(1).altIndex == 1);

		// 2. 最右按右 → 环绕到同层最左
		Layout l2 = singleLayer3();
		l2.coreIdx = 1; // W1 在中间
		ok = l2.swapCore(3);
		check("单层3窗: 中间核心右换成功", ok);
		// 现在 W1 与 W0(角度0.0, 更小) ... findLayerNeighbor(+1) 从 W1(0.8) 找角度更大最近: W0 的角度0.0-0.8=-0.8+2π=5.48(大), W2 -0.8-0.8=-1.6+2π=4.68(大,更近) → 换 W2
		// swap 后：核心窗口 W1右 移到 ordered[2]（新核心位），W2左 到 ordered[1]（原核心位）
		check("单层3窗: 中间右换目标=W2左(邻居到原核心位)", l2.ordered.get(1).name.equals("W2左"));
		check("单层3窗: 核心窗口W1右移到右位", l2.ordered.get(2).name.equals("W1右"));

		// 3. 单窗口：无邻居 → false
		Layout l3 = new Layout();
		l3.ordered.add(new Win("only", 0, 0.0));
		l3.coreIdx = 0;
		l3.layerSizes = Arrays.asList(1);
		ok = l3.swapCore(3);
		check("单窗口: 右换返回false", !ok);
		ok = l3.swapCore(2);
		check("单窗口: 左换返回false", !ok);
		ok = l3.swapCore(0);
		check("单窗口: 上换返回false", !ok);
		ok = l3.swapCore(1);
		check("单窗口: 下换返回false", !ok);

		// 4. 两层 [3,2]：核心在层0槽0，上 → 无上层环绕到最底层同槽位 = 层1槽0 = W3
		Layout l4 = twoLayer();
		l4.coreIdx = 0;
		ok = l4.swapCore(0); // 上
		check("两层: 核心在层0按上 → 环绕到层1槽0", ok);
		check("两层: 上换后核心窗口到 W3 位置", l4.ordered.get(3).name.equals("W0核心") || l4.ordered.get(l4.indexOfCore()).name.equals("W3"));

		// 5. 两层：核心在层1槽0(W3)，下 → 无下层环绕到最上层同槽位 = 层0槽0 = W0
		Layout l5 = twoLayer();
		l5.coreIdx = 3; // W3 在层1槽0
		ok = l5.swapCore(1); // 下
		check("两层: 核心在层1按下 → 环绕到层0槽0", ok);
		check("两层: 下换后 W0 到核心位", l5.ordered.get(0).name.equals("W3") || l5.ordered.get(3).name.equals("W0核心"));

		// 6. 两层：核心在层1槽0(W3)，上 → 层0槽0(W0)
		Layout l6 = twoLayer();
		l6.coreIdx = 3;
		ok = l6.swapCore(0);
		check("两层: 核心在层1按上 → 层0槽0", ok);

		// 7. 两层：核心在层0槽1(W1)，下 → 层1槽1(W4)
		Layout l7 = twoLayer();
		l7.coreIdx = 1;
		ok = l7.swapCore(1);
		check("两层: 核心在层0槽1按下 → 层1槽1", ok);
		check("两层: 下换后 W4 到核心位", l7.ordered.get(1).name.equals("W4") && l7.ordered.get(4).name.equals("W1"));

		System.out.println("\n==== " + passed + " passed, " + failed + " failed ====");
		if(failed > 0) System.exit(1);
	}
}
