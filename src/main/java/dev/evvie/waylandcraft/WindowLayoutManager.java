package dev.evvie.waylandcraft;

import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;

import dev.evvie.waylandcraft.bridge.WLCToplevel;
import dev.evvie.waylandcraft.settings.WaylandCraftSettings;
import net.minecraft.client.Minecraft;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.phys.Vec3;

/**
 * 窗口自动布局管理器（v0.4）：窗口固定在初始化坐标周围，不再跟随玩家。
 *
 * 模板：
 *  - cube（方块）：以初始化坐标为中心、初始化朝向为基准，每层 layoutCubePerFace×4
 *    （默认 2×4 = 8）个窗口，按角度制排布（围绕中心一圈，宽窗口占角度大）。
 *  - sphere（圆球/VR）：以初始化坐标为圆心，窗口在球面上排布（纬度圈），
 *    法线始终指向圆心（从中心看每个窗口都是正对）。
 *
 * 核心改动（v0.4，用户实测反馈）：
 *  - 按"数量"排布，不按固定长宽高缩放：窗口保持原尺寸（不缩放），
 *    槽位/角度按窗口实际宽度自适应，相邻窗口弦长 ≥ 窗口宽 + spacing，
 *    数学上永不重叠（含 cube 拐角处）。
 *  - 半径自适应：如果 layoutRadius 太小放不下一层窗口，自动增大半径
 *    （接受边界宽），而不是缩小窗口。
 *  - 窗口中心对齐眼睛高度（/wl layout init 存眼睛高度），站在中心平视正对，不斜。
 *  - 向上堆叠严格：下一层中心 Y = 上一层中心 Y + (上一层最大高 + 下一层最大高)/2
 *    + stackSpacing，层与层之间保证间距 stackSpacing，不重叠。
 *  - Ctrl+方向键 = 核心标记移动到该方向相邻窗口（核心身份转移，窗口位置不动）。
 *
 * 其他行为：
 *  - 默认关闭（layoutEnabled=false），开启前必须先 /wl layout init 初始化坐标。
 *  - 新加入的窗口自动 resize 到 layoutDefaultWidth×layoutDefaultHeight。
 *  - 窗口底部始终 ≥ 地面 + groundClearance。
 */
public class WindowLayoutManager {

	private final WaylandCraft wlc;

	private boolean enabled = false;

	/** 手动加入布局的窗口句柄（layoutAutoJoin=false 时只排这些窗口） */
	private final HashSet<Long> manualHandles = new HashSet<>();

	/** 核心窗口句柄（0 = 未设置，自动选第一个） */
	private long coreHandle = 0;

	/** 持久排布顺序（新窗口追加，消失移除，不按 handle 重排） */
	private final List<WindowDisplay> ordered = new ArrayList<>();

	/** 下一个新窗口的交替序号（0=核心锚已预留给首个窗口；1=右1, 2=左1, 3=右2, 4=左2…，奇右偶左） */
	private int nextAltIndex = 1;

	/** 每层窗口数（cube: perFace×4；sphere: 每个纬度圈数量），用于上/下换层 */
	private final List<Integer> layerSizes = new ArrayList<>();

	public WindowLayoutManager(WaylandCraft wlc) {
		this.wlc = wlc;
	}

	public boolean isEnabled() {
		return enabled;
	}

	public void setEnabled(boolean enabled) {
		this.enabled = enabled;
	}

	public void addHandle(long handle) {
		manualHandles.add(handle);
	}

	public void removeHandle(long handle) {
		manualHandles.remove(handle);
	}

	public boolean containsHandle(long handle) {
		return manualHandles.contains(handle);
	}

	public long getCoreHandle() {
		return coreHandle;
	}

	public void setCoreHandle(long handle) {
		coreHandle = handle;
	}

	/** 布局是否已初始化坐标（未初始化不可用） */
	public boolean isInitialized() {
		return wlc.settings != null && wlc.settings.getLayoutInitialized();
	}

	/** 布局中心坐标（y = 眼睛高度） */
	public Vec3 centerPos() {
		return new Vec3(wlc.settings.getLayoutInitX(), wlc.settings.getLayoutInitY(), wlc.settings.getLayoutInitZ());
	}

	/** 布局基准朝向（弧度） */
	public double centerYawRad() {
		return Math.toRadians(wlc.settings.getLayoutInitYaw());
	}

	public List<WindowDisplay> participatingDisplays() {
		List<WindowDisplay> result = new ArrayList<>();
		if(wlc == null || wlc.bridge == null) return result;

		boolean autoJoin = wlc.settings != null && wlc.settings.getLayoutAutoJoin();
		for(WindowDisplay d : wlc.displays) {
			if(!(d.window instanceof WLCToplevel)) continue;
			long handle = ((WLCToplevel) d.window).getHandle();
			if(autoJoin || manualHandles.contains(handle)) {
				result.add(d);
			}
		}
		return result;
	}

	/** 每 tick 重排所有参与布局的窗口（围绕初始化坐标，不跟随玩家） */
	public void tick() {
		if(!enabled) return;
		if(wlc == null || wlc.settings == null) return;
		Minecraft mc = Minecraft.getInstance();
		if(mc.level == null) return;

		// 未初始化坐标：自动用玩家当前脚部位置+眼睛高度+朝向初始化（开箱即用，
		// 与 /wl layout init 无参行为一致；之后中心固定，不再跟随玩家）
		if(!wlc.settings.getLayoutInitialized()) {
			autoInit(mc);
		}

		List<WindowDisplay> list = participatingDisplays();
		// 同步持久顺序：移除消失/退出的窗口，新窗口追加到末尾（保留用户交换过的顺序）
		syncOrdered(list);
		layerSizes.clear();
		if(list.isEmpty()) {
			coreHandle = 0;
			return;
		}

		// 窗口保持用户自定义分辨率，布局按实际渲染尺寸自适应（不强制 resize）

		// 按模板排布（用持久顺序 ordered，Ctrl+方向键交换过的位置在这里生效）
		String template = wlc.settings.getLayoutTemplate();
		if("sphere".equals(template)) {
			arrangeSphere(ordered);
		} else {
			arrangeCube(ordered);
		}
		// 贴地钳制已并入 applyLayerHeights（逐层：下一层基于上一层实际最高点，含钳制抬升）

		// 核心窗口保底：默认第一个
		if(coreHandle == 0 || !containsHandleIn(ordered, coreHandle)) {
			coreHandle = ((WLCToplevel) ordered.get(0).window).getHandle();
		}
	}

	/** 用玩家当前脚部位置 + 眼睛高度 + 朝向初始化布局中心（与 /wl layout init 无参行为一致） */
	private void autoInit(Minecraft mc) {
		var player = mc.player;
		if(player == null || wlc.settingsManager == null) return;
		var pos = player.position();
		double yaw = -player.getYRot(); // MC yaw 逆时针 → 布局约定顺时针（0=朝+Z, 90=朝+X）
		wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_X, pos.x);
		wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_Y, pos.y + 1.62);
		wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_Z, pos.z);
		wlc.settingsManager.setDoubleSetting(WaylandCraftSettings.LAYOUT_INIT_YAW, yaw);
		wlc.settingsManager.setBooleanSetting(WaylandCraftSettings.LAYOUT_INITIALIZED, true);
		WaylandCraftCommon.LOGGER.info("[layout] auto-init center=({}, {}, {}) yaw={}° (player position + eye height)",
			String.format("%.2f", pos.x), String.format("%.2f", pos.y + 1.62), String.format("%.2f", pos.z), String.format("%.1f", yaw));
	}

	/**
	 * 同步持久顺序 ordered 与当前参与窗口列表：保留既有顺序，新窗口分配交替序号，消失移除。
	 *
	 * 布局锚点 = 初始化坐标 + 朝向（固定中心），核心标记只在窗口之间转移（moveCore），
	 * 窗口位置一旦分配就**永不移动**（绝不重排已有窗口）——模板稳定、切换无死循环。
	 * 新窗口按交替序号（0=核心锚正面中心，1=右1, 2=左1, 3=右2, 4=左2…）继续扩散。
	 */
	private void syncOrdered(List<WindowDisplay> list) {
		ordered.removeIf(d -> !list.contains(d));
		// 新窗口
		List<WindowDisplay> fresh = new ArrayList<>();
		for(WindowDisplay d : list) {
			if(!ordered.contains(d)) fresh.add(d);
		}

		if(fresh.isEmpty()) return;

		// 交替序号分配：首窗=0（核心锚），之后递增（1=右1, 2=左1, 3=右2, 4=左2…）。
		// 序号即左右交替位置，与 arrangeCube 的交替角度 [0°, +1, -1, +2, -2, …] 严格对应，
		// 保证每次只开一个窗口也左右交替；窗口关闭后序号保留，已有窗口位置不动，新窗口继续扩散。
		for(WindowDisplay d : fresh) {
			if(ordered.isEmpty() && coreHandle == 0) {
				d.layoutAltIndex = 0; // 首个窗口 = 核心锚
			} else {
				d.layoutAltIndex = nextAltIndex++;
			}
			ordered.add(d);
		}
	}

	/**
	 * 核心标记移动到该方向相邻窗口（核心身份转移，窗口位置不动）。
	 * dir: 0=上 1=下 2=左 3=右。
	 * 左/右 = 同层内按世界角度几何相邻（右=顺时针/角度更大的窗口，左=逆时针/角度更小的窗口），
	 * 不受"交替序号插入顺序"约束——向右移动就是向右移动；同层内环绕，可一直切换。
	 * 上/下跨层（同槽位），无上层/下层时环绕到对侧。
	 * 返回是否移动成功。
	 */
	public boolean moveCore(int dir) {
		if(ordered.isEmpty()) return false;
		int n = ordered.size();
		int idx = indexOfCore();
		if(idx < 0) return false;

		int start = layerStartOf(idx);
		int size = layerSizeAt(idx);
		int slot = idx - start;
		int next;

		switch(dir) {
			case 0: { // 上：下一层同槽位（layer index 越大 = Y 越高，见 applyLayerHeights）；最高层环绕到第一层
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
			case 1: { // 下：上一层同槽位（Y 更低）；最底层环绕到最后一层
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
			case 2: // 左：angleOf 更小的窗口（视觉左侧，玩家面向 baseYaw 时左 = 角度更小）；最左环绕到同层最右
				next = findLayerNeighbor(idx, start, size, -1);
				break;
			default: // 右：angleOf 更大的窗口（视觉右侧）；最右环绕到同层最左
				next = findLayerNeighbor(idx, start, size, +1);
				break;
		}

		if(next < 0 || next >= n || next == idx) return false;
		coreHandle = ((WLCToplevel) ordered.get(next).window).getHandle();
		WaylandCraftCommon.LOGGER.info("[move] 核心 -> {} (dir={}, layerIdx={}/{})",
			WaylandCraft.getWindowName((WLCToplevel) ordered.get(next).window), dir, next - start + 1, size);
		return true;
	}

	/**
	 * 核心窗口与该方向相邻窗口互换排序（位置互换，窗口真的移动）。
	 * dir: 0=上 1=下 2=左 3=右。
	 *
	 * 无任何范围限制（用户拍板：怎么排序都可以，不能有限制）：
	 *  - 左/右 = 同层内按世界方位角几何相邻（顺时针/逆时针最近），最左/最右环绕到对侧，可一直切换；
	 *  - 上/下 = 跨层同槽位，无上层/下层时环绕到对侧；
	 *  - 单窗口无邻居时返回 false，除此之外不做任何边界/数量限制。
	 *
	 * 交换 = ordered 顺序 + layoutAltIndex 同时交换：
	 *  - cube 模板按 layoutAltIndex 算槽位 → altIndex 互换即位置互换；
	 *  - sphere 模板按 ordered 顺序连续排布 → 顺序互换即位置互换。
	 * 核心身份跟随核心窗口（coreHandle 不变），交换后下一 tick 按新排序重排生效。
	 * 返回是否交换成功。
	 */
	public boolean swapCore(int dir) {
		if(ordered.isEmpty()) return false;
		int n = ordered.size();
		int idx = indexOfCore();
		if(idx < 0) return false;

		int start = layerStartOf(idx);
		int size = layerSizeAt(idx);
		int slot = idx - start;
		int next;

		switch(dir) {
			case 0: { // 上：下一层同槽位（layer index 越大 = Y 越高，见 applyLayerHeights）；最高层环绕到第一层
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
			case 1: { // 下：上一层同槽位（Y 更低）；最底层环绕到最后一层
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
			case 2: // 左：angleOf 更小的窗口（视觉左侧，玩家面向 baseYaw 时左 = 角度更小）；最左环绕到同层最右
				next = findLayerNeighbor(idx, start, size, -1);
				break;
			default: // 右：angleOf 更大的窗口（视觉右侧）；最右环绕到同层最左
				next = findLayerNeighbor(idx, start, size, +1);
				break;
		}

		if(next < 0 || next >= n || next == idx) return false;

		WindowDisplay a = ordered.get(idx);
		WindowDisplay b = ordered.get(next);
		Collections.swap(ordered, idx, next);
		// layoutAltIndex 跟着窗口走：cube 按它算槽位，交换后位置互换
		int tmpAlt = a.layoutAltIndex;
		a.layoutAltIndex = b.layoutAltIndex;
		b.layoutAltIndex = tmpAlt;

		WaylandCraftCommon.LOGGER.info("[swap] 核心 {} <-> {} (dir={}, alt {}<->{})",
			WaylandCraft.getWindowName((WLCToplevel) a.window),
			WaylandCraft.getWindowName((WLCToplevel) b.window), dir,
			b.layoutAltIndex, a.layoutAltIndex);
		return true;
	}

	/**
	 * 同层内按世界方位角找左/右相邻窗口。
	 * dir=+1：角度更大的最近窗口（顺时针/向右）；dir=-1：角度更小的最近窗口（逆时针/向左）。
	 * 取"从核心沿该方向绕一圈的最小角差"，最右/最左自动环绕到对侧，可一直切换。
	 */
	private int findLayerNeighbor(int idx, int start, int size, int dir) {
		if(size <= 1) return idx;
		WindowDisplay core = ordered.get(idx);
		double a = angleOf(core);
		int best = -1;
		double bestDiff = 0;
		for(int i = start; i < start + size; i++) {
			if(i == idx) continue;
			double d = angleOf(ordered.get(i));
			double diff = dir > 0 ? d - a : a - d;
			if(diff < 0) diff += Math.PI * 2; // 绕一圈到另一侧
			if(best < 0 || diff < bestDiff) {
				best = i;
				bestDiff = diff;
			}
		}
		return best < 0 ? idx : best;
	}

	/** 窗口相对布局中心的方位角（与 arrange 的 x=center.x+r*sin(a), z=center.z+r*cos(a) 一致） */
	private double angleOf(WindowDisplay d) {
		Vec3 center = centerPos();
		return Math.atan2(d.pivot.x - center.x, d.pivot.z - center.z);
	}

	/** 当前核心窗口在 ordered 中的索引；不在列表中返回 -1 */
	private int indexOfCore() {
		for(int i = 0; i < ordered.size(); i++) {
			if(((WLCToplevel) ordered.get(i).window).getHandle() == coreHandle) return i;
		}
		return -1;
	}

	/** idx 所在层的起始索引 */
	private int layerStartOf(int idx) {
		int acc = 0;
		for(int size : layerSizes) {
			if(idx < acc + size) return acc;
			acc += size;
		}
		return 0;
	}

	/** idx 所在层的大小 */
	private int layerSizeAt(int idx) {
		int acc = 0;
		for(int size : layerSizes) {
			if(idx < acc + size) return size;
			acc += size;
		}
		return layerSizes.isEmpty() ? 1 : layerSizes.get(layerSizes.size() - 1);
	}

	/** 给定某层起始索引 start，返回上一层起始索引 */
	private int prevLayerStart(int start) {
		int acc = 0;
		for(int size : layerSizes) {
			if(acc + size >= start) return acc;
			acc += size;
		}
		return 0;
	}

	private boolean containsHandleIn(List<WindowDisplay> list, long handle) {
		for(WindowDisplay d : list) {
			if(((WLCToplevel) d.window).getHandle() == handle) return true;
		}
		return false;
	}

	/** 窗口世界宽度（格）：按实际渲染像素（framebuffer 优先，兜底 geometry） */
	public static double worldWidth(WindowDisplay d) {
		return d.localX().length() * renderWidthPx(d);
	}

	/** 窗口世界高度（格）：按实际渲染像素（framebuffer 优先，兜底 geometry） */
	public static double worldHeight(WindowDisplay d) {
		return d.localY().length() * renderHeightPx(d);
	}

	/** 实际渲染像素宽：framebuffer（物理像素，含 scale/子窗口）优先；未创建时兜底 geometry */
	private static int renderWidthPx(WindowDisplay d) {
		if(d.window.framebuffer != null && d.window.framebuffer.getWidth() > 0) return d.window.framebuffer.getWidth();
		return d.window.geometry.width();
	}

	/** 实际渲染像素高：framebuffer（物理像素，含 scale/子窗口）优先；未创建时兜底 geometry */
	private static int renderHeightPx(WindowDisplay d) {
		if(d.window.framebuffer != null && d.window.framebuffer.getHeight() > 0) return d.window.framebuffer.getHeight();
		return d.window.geometry.height();
	}

	/**
	 * 渲染中心相对 pivot（几何中心）的世界偏移。
	 * render() 用 geometry 半尺寸锚定 origin、用 framebuffer 尺寸绘制：
	 * 当 framebuffer 与 geometry 不一致（HiDPI scale、子窗口偏移）时，
	 * 渲染中心 ≠ pivot。布局用渲染中心排布，视觉才不重叠。
	 */
	private static Vec3 renderCenterOffset(WindowDisplay d) {
		int gw = d.window.geometry.width();
		int gh = d.window.geometry.height();
		int bw = renderWidthPx(d);
		int bh = renderHeightPx(d);
		int xoff = (d.window.framebuffer != null) ? d.window.framebuffer.getXOff() : 0;
		int yoff = (d.window.framebuffer != null) ? d.window.framebuffer.getYOff() : 0;
		return d.localX().scale(bw / 2.0 - gw / 2.0 - xoff)
			.add(d.localY().scale(bh / 2.0 - gh / 2.0 - yoff));
	}

	/** 窗口底部抬到地面 + groundClearance（基于实际渲染中心/高度） */
	private void clampToGround(WindowDisplay d) {
		Minecraft mc = Minecraft.getInstance();
		if(mc.level == null || wlc.settings == null) return;
		double clearance = wlc.settings.getGroundClearance();
		int groundY = mc.level.getHeight(Heightmap.Types.MOTION_BLOCKING, (int) Math.floor(d.pivot.x), (int) Math.floor(d.pivot.z));
		double halfHeight = worldHeight(d) / 2.0;
		Vec3 off = renderCenterOffset(d);
		double renderCenterY = d.pivot.y + off.y;
		double minY = groundY + clearance + halfHeight;
		if(renderCenterY < minY) {
			d.pivot = new Vec3(d.pivot.x, minY - off.y, d.pivot.z);
		}
	}

	// ==================== 角度制通用工具 ====================

	/** 窗口在半径 radius 的圆上所需的角度跨度（弧度），保证相邻窗口中心弦长 ≥ w + spacing */
	private double angleSpan(WindowDisplay d, double radius, double spacing) {
		double w = worldWidth(d);
		double half = (w + spacing) / (2.0 * radius);
		if(half >= 1.0) half = 0.9999; // 防御性压缩（正常由半径自适应保证 half < 1）
		return 2.0 * Math.asin(half);
	}

	/** 设置窗口朝向：法线水平指向中心、down 向下（竖直窗口，从中心平视正对不斜） */
	private void orientToCenter(WindowDisplay d, Vec3 center) {
		Vec3 toCenter = new Vec3(center.x - d.pivot.x, 0, center.z - d.pivot.z);
		if(toCenter.lengthSqr() < 1e-6) toCenter = new Vec3(0, 0, 1);
		d.rotate(toCenter.normalize(), new Vec3(0, -1, 0));
	}

	/**
	 * 逐层设置窗口渲染中心 Y，并逐层贴地钳制。
	 * 层基准：上一层"实际渲染最高点"（含贴地钳制抬升） + stackSpacing，再 + 本层半高。
	 * 即上层窗口底部 = 下层窗口最高区域 + 0.4（用户要求的语义），任何分辨率/scale/地面都永不重叠。
	 * pivot 按 renderCenterOffset 补偿，保证渲染中心落在目标 Y。
	 */
	private void applyLayerHeights(List<WindowDisplay> list, List<Integer> sizes, List<Double> maxHeights, double firstCenterY, double stackSpacing) {
		int pos = 0;
		double prevMaxTop = Double.NaN; // 上一层实际渲染最高点
		for(int l = 0; l < sizes.size(); l++) {
			int count = sizes.get(l);
			double thisMaxH = maxHeights.get(l);
			double layerCenterY = (l == 0) ? firstCenterY : prevMaxTop + stackSpacing + thisMaxH / 2.0;
			double layerMaxTop = Double.NEGATIVE_INFINITY;
			for(int j = 0; j < count; j++) {
				WindowDisplay d = list.get(pos + j);
				Vec3 off = renderCenterOffset(d);
				d.pivot = new Vec3(d.pivot.x - off.x, layerCenterY - off.y, d.pivot.z - off.z);
				// 贴地钳制（只抬升）：第一层大窗口不会插地，且下一层基准会含抬升效果
				clampToGround(d);
				Vec3 off2 = renderCenterOffset(d);
				double renderCenterY = d.pivot.y + off2.y;
				double top = renderCenterY + worldHeight(d) / 2.0;
				if(top > layerMaxTop) layerMaxTop = top;
			}
			prevMaxTop = layerMaxTop;
			pos += count;
		}
	}

	// ==================== cube 方块模板 ====================

	/**
	 * 方块布局（角度制/VR 屏墙）：每层 layoutCubePerFace×4 个窗口，
	 * 围绕中心均匀分布一整圈（4 面，每面 perFace 个，面中心朝 baseYaw 的 4 个方向）。
	 * 半径自适应：若窗口角宽大于槽位角宽（会重叠），自动增大半径（接受边界宽），
	 * 不缩放窗口。相邻窗口（含拐角）弦长 ≥ 窗口宽 + spacing，永不重叠。
	 */
	private void arrangeCube(List<WindowDisplay> list) {
		WaylandCraftSettings s = wlc.settings;
		double spacing = Math.max(0, s.getLayoutSpacing());
		double stackSpacing = Math.max(0, s.getLayoutStackSpacing());
		int perFace = Math.max(1, s.getLayoutCubePerFace());
		Vec3 center = centerPos();
		double baseYaw = centerYawRad();

		int layerSize = perFace * 4;
		double slotAngle = (Math.PI / 2.0) / perFace; // 每面内槽位角宽（面 = 90°）

		// 层内交替槽位角（以核心为锚左右扩散，与 syncOrdered 的交替插入顺序严格对应）：
		//   0°(前中=核心) → +slotAngle(右1) → -slotAngle(左1) → +2*slotAngle(右2) → -2*slotAngle(左2) → … → 180°(后中)
		// 最小相邻角差 = slotAngle（核心-右1、右1-右2、核心-左1、左1-左2、…），radius 自适应公式不变。
		double[] alt = new double[layerSize];
		alt[0] = 0;
		int p = 1;
		for(int k = 1; k <= perFace * 2 - 1 && p < layerSize; k++) {
			if(p < layerSize) alt[p++] = k * slotAngle;   // 右 k
			if(p < layerSize) alt[p++] = -k * slotAngle;  // 左 k
		}
		if(p < layerSize) alt[p] = Math.PI; // 后中心

		// 半径自适应：最宽窗口的角宽 ≤ 槽位角宽 → 均匀排布永不重叠（只看首层，序号 < layerSize）
		double maxW = 0;
		for(WindowDisplay d : list) {
			int si = d.layoutAltIndex;
			if(si < 0) si = 0;
			if(si >= layerSize) continue;
			maxW = Math.max(maxW, worldWidth(d));
		}
		double need = (maxW + spacing) / (2.0 * Math.sin(slotAngle / 2.0));
		double radius = Math.max(s.getLayoutRadius(), need);

		// 按交替序号分组到层：层 = 序号 / layerSize，层内槽位 = 序号 % layerSize。
		// 窗口关闭后序号保留（空洞），已有窗口角度不变，新窗口继续按序号扩散。
		List<Integer> sizes = new ArrayList<>();
		List<Double> maxHeights = new ArrayList<>();
		int maxLayer = 0;
		for(WindowDisplay d : list) {
			int si = d.layoutAltIndex;
			if(si < 0) si = 0;
			maxLayer = Math.max(maxLayer, si / layerSize);
		}
		for(int l = 0; l <= maxLayer; l++) {
			int count = 0;
			double layerMaxH = 0;
			for(WindowDisplay d : list) {
				int si = d.layoutAltIndex;
				if(si < 0) si = 0;
				if(si / layerSize != l) continue;
				count++;
				double h = worldHeight(d);
				double angle = baseYaw + alt[si % layerSize]; // 交替角度（核心→0° 前中，右1→+，左1→-）
				double x = center.x + radius * Math.sin(angle);
				double z = center.z + radius * Math.cos(angle);
				d.pivot = new Vec3(x, center.y, z); // Y 由 applyLayerHeights 统一设置
				orientToCenter(d, center);
				layerMaxH = Math.max(layerMaxH, h);
			}
			if(count > 0) {
				sizes.add(count);
				maxHeights.add(layerMaxH);
			}
		}

		layerSizes.addAll(sizes);
		// 第一层窗口中心 = 眼睛高度（center.y = /wl layout init 存的 y + 1.62）
		applyLayerHeights(list, sizes, maxHeights, center.y, stackSpacing);
	}

	// ==================== sphere 圆球模板（VR） ====================

	/**
	 * 圆球布局（VR 屏墙）：以初始化坐标为圆心，窗口围绕中心分层排布。
	 * 水平：窗口按角度连续排布，相邻窗口中心弦长 ≥ 窗口宽 + spacing，永不重叠。
	 * 垂直：下一层中心 Y = 本层中心 Y + (本层最大高 + 下一层最大高)/2 + stackSpacing。
	 * 半径自适应：若 layoutRadius 放不下最宽窗口，自动增大半径。
	 * 窗口始终竖直放置（法线水平指向中心、down 向下），中心对齐眼睛高度，不斜。
	 */
	private void arrangeSphere(List<WindowDisplay> list) {
		WaylandCraftSettings s = wlc.settings;
		double spacing = Math.max(0, s.getLayoutSpacing());
		double stackSpacing = Math.max(0, s.getLayoutStackSpacing());
		double baseYaw = centerYawRad();
		Vec3 center = centerPos();

		// 半径自适应：至少使最宽窗口 half < 1（能放下）
		double radius = Math.max(1.0, s.getLayoutRadius());
		double maxW = 0;
		for(WindowDisplay d : list) {
			maxW = Math.max(maxW, worldWidth(d));
		}
		double need = (maxW + spacing) / 2.0;
		if(need >= radius) radius = need / 0.9999;

		List<Integer> sizes = new ArrayList<>();
		List<Double> maxHeights = new ArrayList<>();
		int i = 0;
		while(i < list.size()) {
			double lon = 0;
			int count = 0;
			double layerMaxH = 0;
			while(i < list.size()) {
				WindowDisplay d = list.get(i);
				double h = worldHeight(d);
				double span = angleSpan(d, radius, spacing);
				if(lon + span > 2 * Math.PI + 1e-9 && count > 0) break; // 圈满，升层

				double angle = baseYaw + lon + span / 2.0;
				double x = center.x + radius * Math.sin(angle);
				double z = center.z + radius * Math.cos(angle);
				d.pivot = new Vec3(x, center.y, z); // Y 由 applyLayerHeights 统一设置
				orientToCenter(d, center);

				lon += span;
				count++;
				layerMaxH = Math.max(layerMaxH, h);
				i++;
			}
			sizes.add(count);
			maxHeights.add(layerMaxH);
		}

		layerSizes.addAll(sizes);
		applyLayerHeights(list, sizes, maxHeights, center.y, stackSpacing);
	}

	/** 窗口世界尺寸统计（给命令用） */
	public static String describeWindow(WindowDisplay d) {
		return String.format("%.2f×%.2f", worldWidth(d), worldHeight(d));
	}

}
