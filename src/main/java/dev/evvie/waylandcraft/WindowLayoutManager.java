package dev.evvie.waylandcraft;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;

import dev.evvie.waylandcraft.bridge.WLCToplevel;
import dev.evvie.waylandcraft.settings.WaylandCraftSettings;
import net.minecraft.client.Minecraft;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.phys.Vec3;

/**
 * 窗口自动布局管理器（v0.3）：窗口固定在初始化坐标周围，不再跟随玩家。
 *
 * 模板：
 *  - cube（方块）：以初始化坐标为中心、初始化朝向为基准，4 个面（前/右/后/左），
 *    每面 layoutCubePerFace（默认 2）个窗口并排，第一层 4×perFace 个窗口，
 *    排满后向上堆叠。窗口法线水平指向中心（正对，不斜）。
 *  - sphere（圆球/VR）：以初始化坐标为球心，窗口在球面上排布（纬度圈），
 *    法线始终指向球心（从中心看每个窗口都是正对）。相邻窗口水平/垂直弧长
 *    均 ≥ 窗口尺寸 + 间距，保证不重合。
 *
 * 其他行为：
 *  - 默认关闭（layoutEnabled=false），开启前必须先 /wl layout init 初始化坐标。
 *  - 新加入的窗口自动 resize 到 layoutDefaultWidth×layoutDefaultHeight。
 *  - 窗口底部始终 ≥ 地面 + groundClearance。
 *  - 第一个窗口 = 核心窗口，Ctrl+方向键切换核心（左/右=同层相邻，上/下=换层）。
 */
public class WindowLayoutManager {

	private final WaylandCraft wlc;

	private boolean enabled = false;

	/** 手动加入布局的窗口句柄（layoutAutoJoin=false 时只排这些窗口） */
	private final HashSet<Long> manualHandles = new HashSet<>();

	/** 核心窗口句柄（0 = 未设置，自动选第一个） */
	private long coreHandle = 0;

	/** 最近一次排布顺序（用于核心窗口切换） */
	private final List<WindowDisplay> ordered = new ArrayList<>();

	/** 已自动 resize 过的窗口（避免每 tick 强制 resize） */
	private final HashSet<Long> resizedHandles = new HashSet<>();

	/** 每层窗口数（cube: perFace*4；sphere: 每个纬度圈数量），用于上/下换层 */
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

	/** 布局中心坐标 */
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
		ordered.clear();
		layerSizes.clear();
		if(list.isEmpty()) {
			coreHandle = 0;
			return;
		}

		// 稳定排序：按 handle 排序，保证排布顺序固定（核心窗口切换可预期）
		list.sort((a, b) -> Long.compare(((WLCToplevel) a.window).getHandle(), ((WLCToplevel) b.window).getHandle()));
		ordered.addAll(list);

		// 新窗口自动 resize 到默认分辨率
		for(WindowDisplay d : list) {
			resizeIfNeeded(d);
		}

		// 按模板排布
		String template = wlc.settings.getLayoutTemplate();
		if("sphere".equals(template)) {
			arrangeSphere(list);
		} else {
			arrangeCube(list);
		}

		// 高度钳制：窗口底部 ≥ 地面 + groundClearance
		for(WindowDisplay d : list) {
			clampToGround(d);
		}

		// 核心窗口保底：默认第一个
		if(coreHandle == 0 || !containsHandleIn(list, coreHandle)) {
			coreHandle = ((WLCToplevel) list.get(0).window).getHandle();
		}
	}

	/** 核心窗口切换。dir: 0=上 1=下 2=左 3=右。返回是否切换成功。 */
	public boolean cycleCore(int dir) {
		if(ordered.isEmpty()) return false;
		int n = ordered.size();
		int idx = indexOfCore();
		if(idx < 0) idx = 0;
		int layerSize = layerSizeAt(idx);
		int next = switch(dir) {
			case 0 -> (idx - layerSize + n) % n; // 上：上一层
			case 1 -> (idx + layerSize) % n;     // 下：下一层
			case 2 -> (idx - 1 + n) % n;         // 左：同层前一个
			default -> (idx + 1) % n;            // 右：同层后一个
		};
		coreHandle = ((WLCToplevel) ordered.get(next).window).getHandle();
		return true;
	}

	/** 当前核心窗口在 ordered 中的索引；不在列表中返回 -1 */
	private int indexOfCore() {
		for(int i = 0; i < ordered.size(); i++) {
			if(((WLCToplevel) ordered.get(i).window).getHandle() == coreHandle) return i;
		}
		return -1;
	}

	/** 核心窗口所在层的大小；未知时退回同层相邻 ±1 */
	private int layerSizeAt(int idx) {
		if(layerSizes.isEmpty()) return 1;
		int acc = 0;
		for(int size : layerSizes) {
			if(idx < acc + size) return size;
			acc += size;
		}
		return layerSizes.get(layerSizes.size() - 1);
	}

	private boolean containsHandleIn(List<WindowDisplay> list, long handle) {
		for(WindowDisplay d : list) {
			if(((WLCToplevel) d.window).getHandle() == handle) return true;
		}
		return false;
	}

	/** 新窗口自动 resize 到默认分辨率（仅一次） */
	private void resizeIfNeeded(WindowDisplay d) {
		if(wlc.bridge == null || wlc.settings == null) return;
		if(!(d.window instanceof WLCToplevel t)) return;
		long handle = t.getHandle();
		if(resizedHandles.contains(handle)) return;
		int w = wlc.settings.getLayoutDefaultWidth();
		int h = wlc.settings.getLayoutDefaultHeight();
		if(t.geometry.width() != w || t.geometry.height() != h) {
			wlc.bridge.resizeToplevel(t, w, h);
		}
		resizedHandles.add(handle);
	}

	/** 窗口世界宽度（格） */
	public static double worldWidth(WindowDisplay d) {
		return d.localX().length() * d.window.geometry.width();
	}

	/** 窗口世界高度（格） */
	public static double worldHeight(WindowDisplay d) {
		return d.localY().length() * d.window.geometry.height();
	}

	/** 窗口底部抬到地面 + groundClearance */
	private void clampToGround(WindowDisplay d) {
		Minecraft mc = Minecraft.getInstance();
		if(mc.level == null || wlc.settings == null) return;
		double clearance = wlc.settings.getGroundClearance();
		int groundY = mc.level.getHeight(Heightmap.Types.MOTION_BLOCKING, (int) Math.floor(d.pivot.x), (int) Math.floor(d.pivot.z));
		double halfHeight = worldHeight(d) / 2.0;
		double minY = groundY + clearance + halfHeight;
		if(d.pivot.y < minY) {
			d.pivot = new Vec3(d.pivot.x, minY, d.pivot.z);
		}
	}

	// ==================== cube 方块模板 ====================

	/**
	 * 方块布局：4 个面围绕中心，每面 perFace 个窗口并排，第一层 4×perFace 个，
	 * 排满后向上堆。窗口法线水平指向中心（正对，不斜）。
	 */
	private void arrangeCube(List<WindowDisplay> list) {
		WaylandCraftSettings s = wlc.settings;
		double radius = Math.max(0.5, s.getLayoutRadius());
		double spacing = Math.max(0, s.getLayoutSpacing());
		double stackSpacing = Math.max(0, s.getLayoutStackSpacing());
		int perFace = Math.max(1, s.getLayoutCubePerFace());
		Vec3 center = centerPos();
		double baseYaw = centerYawRad();

		int layer = 0;
		int layerSize = perFace * 4;
		double layerBaseY = Double.NaN;
		double layerMaxBottom = 0; // 当前层窗口底部 y 的最大值（决定下一层起始）
		int inLayer = 0;

		for(int i = 0; i < list.size(); i++) {
			WindowDisplay d = list.get(i);
			double w = worldWidth(d);
			double h = worldHeight(d);

			if(inLayer == 0) {
				// 新层：底部 = 上一层底部 + 上一层最大高度 + stackSpacing
				if(i == 0) {
					layerBaseY = firstLayerBaseY(center.y, h);
				} else {
					layerBaseY = layerMaxBottom + stackSpacing;
				}
				layerMaxBottom = layerBaseY + h;
				layerSizes.add(Math.min(layerSize, list.size() - i));
			}
			layerMaxBottom = Math.max(layerMaxBottom, layerBaseY + h);
			inLayer++;
			if(inLayer >= layerSize) {
				layer++;
				inLayer = 0;
			}

			int face = (i % layerSize) / perFace;          // 0前 1右 2后 3左
			int slot = (i % layerSize) % perFace;          // 面内第几个

			double faceYaw = baseYaw + face * Math.PI / 2.0;
			Vec3 faceDir = new Vec3(Math.sin(faceYaw), 0, Math.cos(faceYaw));
			Vec3 tangent = new Vec3(Math.cos(faceYaw), 0, -Math.sin(faceYaw)); // 面内右手方向

			// 面中心（在中心前方 radius 处）
			Vec3 faceCenter = center.add(faceDir.scale(radius));
			// 面内槽位偏移：槽位居中，间距 spacing
			double offset = (slot - (perFace - 1) / 2.0) * (w + spacing);
			Vec3 pivot = faceCenter.add(tangent.scale(offset));
			pivot = new Vec3(pivot.x, layerBaseY + h / 2.0, pivot.z);

			d.pivot = pivot;
			// 窗口法线水平指向中心（正对中心，不斜）
			Vec3 toCenter = new Vec3(center.x - pivot.x, 0, center.z - pivot.z);
			if(toCenter.lengthSqr() < 1e-6) toCenter = new Vec3(0, 0, 1);
			d.rotate(toCenter.normalize(), new Vec3(0, -1, 0));
		}
	}

	// ==================== sphere 圆球模板（VR） ====================

	/**
	 * 圆球布局（VR 屏墙）：以初始化坐标为圆心，窗口围绕中心分层排布。
	 *
	 * 数学保证（不重合）：
	 *  - 水平：相邻窗口中心连线的弦长 ≥ 窗口宽 + spacing。角度间隔
	 *    θ = 2·asin((w + spacing) / (2·radius))。
	 *  - 垂直：下一层 y = 上一层 y + 上一层最大窗口高 + stackSpacing，
	 *    窗口为竖直矩形，垂直方向必然不重叠。
	 *  - 半径固定（不向外扩）：一层排满后窗口在上一层正上方继续堆叠（向上堆）。
	 * 窗口始终竖直放置（法线水平指向中心、down 向下），从中心平视时正对，不斜。
	 */
	private void arrangeSphere(List<WindowDisplay> list) {
		WaylandCraftSettings s = wlc.settings;
		double radius = Math.max(1.0, s.getLayoutRadius());
		double spacing = Math.max(0, s.getLayoutSpacing());
		double stackSpacing = Math.max(0, s.getLayoutStackSpacing());
		double baseYaw = centerYawRad();
		Vec3 center = centerPos();

		double layerY = center.y;      // 当前层窗口中心 y
		double layerMaxH = 0;          // 当前层最大窗口高
		int i = 0;
		while(i < list.size()) {
			double lon = 0;            // 层内累计角度
			int countInLayer = 0;
			while(i < list.size()) {
				WindowDisplay d = list.get(i);
				double w = worldWidth(d);
				double h = worldHeight(d);
				// 弦长公式：保证相邻窗口中心弦长 ≥ w + spacing
				double half = (w + spacing) / (2.0 * radius);
				if(half >= 1.0) half = 0.999; // 半径太小放不下，压缩到近乎占满
				double step = 2.0 * Math.asin(half);
				if(lon + step > 2 * Math.PI + 1e-9 && countInLayer > 0) break; // 圈满，升层

				double angle = baseYaw + lon + step / 2.0;
				double x = center.x + radius * Math.sin(angle);
				double z = center.z + radius * Math.cos(angle);
				Vec3 pivot = new Vec3(x, layerY, z);
				d.pivot = pivot;
				// 法线水平指向中心（竖直窗口，不斜）
				Vec3 toCenter = new Vec3(center.x - x, 0, center.z - z);
				if(toCenter.lengthSqr() < 1e-6) toCenter = new Vec3(0, 0, 1);
				d.rotate(toCenter.normalize(), new Vec3(0, -1, 0));

				lon += step;
				countInLayer++;
				layerMaxH = Math.max(layerMaxH, h);
				i++;
			}
			layerSizes.add(countInLayer);
			// 升层：下一层 y = 当前层 y + 最大高 + 层距（垂直严格不重叠，向上堆）
			layerY += layerMaxH + stackSpacing;
			layerMaxH = 0;
		}
	}

	private double firstLayerBaseY(double centerY, double firstWindowH) {
		// 第一层窗口底部 = 中心高度 - 第一窗口半高（保证与中心在同一水平线附近）
		return centerY - firstWindowH / 2.0;
	}

	/** 窗口世界尺寸统计（给命令用） */
	public static String describeWindow(WindowDisplay d) {
		return String.format("%.2f×%.2f", worldWidth(d), worldHeight(d));
	}

}
