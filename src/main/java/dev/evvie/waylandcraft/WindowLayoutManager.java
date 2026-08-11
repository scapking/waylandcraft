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
 *  - Ctrl+方向键 = 核心窗口与该方向相邻窗口互换实际位置（不是切换标记）。
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

	/** 持久排布顺序（Ctrl+方向键交换位置；新窗口追加，消失移除，不按 handle 重排） */
	private final List<WindowDisplay> ordered = new ArrayList<>();

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
		if(!wlc.settings.getLayoutInitialized()) return;
		Minecraft mc = Minecraft.getInstance();
		if(mc.level == null) return;

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

		// 高度钳制：窗口底部 ≥ 地面 + groundClearance
		for(WindowDisplay d : ordered) {
			clampToGround(d);
		}

		// 核心窗口保底：默认第一个
		if(coreHandle == 0 || !containsHandleIn(ordered, coreHandle)) {
			coreHandle = ((WLCToplevel) ordered.get(0).window).getHandle();
		}
	}

	/** 同步持久顺序 ordered 与当前参与窗口列表：保留既有顺序，新增追加，消失移除 */
	private void syncOrdered(List<WindowDisplay> list) {
		ordered.removeIf(d -> !list.contains(d));
		// 新窗口：基于核心窗口左右交替扩散插入（第一个在核心右，第二个在核心左，以此类推）
		List<WindowDisplay> fresh = new ArrayList<>();
		for(WindowDisplay d : list) {
			if(!ordered.contains(d)) fresh.add(d);
		}
		if(fresh.isEmpty()) return;

		int ci = indexOfCore();
		if(ci < 0) {
			// 尚无核心（或核心不在列表）：新窗口追加末尾，首个窗口随后会被设为核心
			ordered.addAll(fresh);
			return;
		}
		int leftPos = ci;      // 核心左侧插入位置（插在核心前面）
		int rightPos = ci + 1; // 核心右侧插入位置
		boolean goRight = true;
		for(WindowDisplay d : fresh) {
			if(goRight) {
				ordered.add(rightPos, d);
				rightPos++;
			} else {
				ordered.add(leftPos, d);
				rightPos++; // 左侧插入使核心及右侧整体右移
			}
			goRight = !goRight;
		}
	}

	/**
	 * 核心窗口与该方向相邻窗口互换实际位置（窗口真的移动）。
	 * dir: 0=上 1=下 2=左 3=右。核心窗口跟随移动（coreHandle 不变）。
	 * 无上限：左/右在 ordered 中全局环绕，上/下跨层，无上层/下层时环绕到对侧，可一直切换。
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
			case 0: { // 上：上一层同槽位；无上层则环绕到最底层同槽位
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
			case 1: { // 下：下一层同槽位；无下层则环绕到最上层同槽位
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
			case 2: // 左：前一个，最左环绕到最右
				next = (idx - 1 + n) % n;
				break;
			default: // 右：后一个，最右环绕到最左
				next = (idx + 1) % n;
				break;
		}

		if(next < 0 || next >= n || next == idx) return false;
		Collections.swap(ordered, idx, next);
		return true;
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
	 * 按分层结果设置每层窗口渲染中心 Y：层 1 = 眼睛高度，之后严格按下层最高区域累加。
	 * 层基准：上层窗口底部 = 下层窗口最高点 + stackSpacing（用户要求的"最高区域 + 0.4"）。
	 * pivot 按 renderCenterOffset 补偿，保证渲染中心落在目标 Y。
	 */
	private void applyLayerHeights(List<WindowDisplay> list, List<Integer> sizes, List<Double> maxHeights, double firstCenterY, double stackSpacing) {
		int pos = 0;
		double layerCenterY = firstCenterY;
		for(int l = 0; l < sizes.size(); l++) {
			int count = sizes.get(l);
			double thisMaxH = maxHeights.get(l);
			for(int j = 0; j < count; j++) {
				WindowDisplay d = list.get(pos + j);
				Vec3 off = renderCenterOffset(d);
				d.pivot = new Vec3(d.pivot.x - off.x, layerCenterY - off.y, d.pivot.z - off.z);
			}
			pos += count;
			if(l + 1 < sizes.size()) {
				double nextMaxH = maxHeights.get(l + 1);
				// 下一层中心 = 本层最高点 + stackSpacing + 下一层半高
				layerCenterY = (layerCenterY + thisMaxH / 2.0) + stackSpacing + nextMaxH / 2.0;
			}
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

		// 半径自适应：最宽窗口的角宽 ≤ 槽位角宽 → 均匀排布永不重叠
		double maxW = 0;
		for(int j = 0; j < Math.min(layerSize, list.size()); j++) {
			maxW = Math.max(maxW, worldWidth(list.get(j)));
		}
		double need = (maxW + spacing) / (2.0 * Math.sin(slotAngle / 2.0));
		double radius = Math.max(s.getLayoutRadius(), need);

		List<Integer> sizes = new ArrayList<>();
		List<Double> maxHeights = new ArrayList<>();
		int idx = 0;
		while(idx < list.size()) {
			int count = Math.min(layerSize, list.size() - idx);
			sizes.add(count);
			double layerMaxH = 0;
			for(int j = 0; j < count; j++) {
				WindowDisplay d = list.get(idx + j);
				double h = worldHeight(d);
				int face = j / perFace;            // 0前 1右 2后 3左
				int slot = j % perFace;            // 面内第几个
				double angle = baseYaw + face * Math.PI / 2.0 + (slot - (perFace - 1) / 2.0) * slotAngle;
				double x = center.x + radius * Math.sin(angle);
				double z = center.z + radius * Math.cos(angle);
				d.pivot = new Vec3(x, center.y, z); // Y 由 applyLayerHeights 统一设置
				orientToCenter(d, center);
				layerMaxH = Math.max(layerMaxH, h);
			}
			maxHeights.add(layerMaxH);
			idx += count;
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
