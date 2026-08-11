package dev.evvie.waylandcraft;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;

import dev.evvie.waylandcraft.bridge.WLCToplevel;
import dev.evvie.waylandcraft.settings.WaylandCraftSettings;
import dev.evvie.waylandcraft.utils.WaylandCraftUtils;
import net.minecraft.client.Minecraft;
import net.minecraft.client.player.LocalPlayer;
import net.minecraft.world.phys.Vec3;

/**
 * 圆形自动布局：以玩家为圆心，窗口在水平面上环形排列、始终面向玩家。
 *
 * 行为：
 *  - 半径 layoutRadius（默认 6 格），第一层窗口中心高度 = 玩家眼睛高度。
 *  - 窗口按实际世界尺寸排布，相邻窗口最小间距 layoutSpacing（默认 0.5 格）。
 *  - 一圈排满后，新窗口在上一层窗口正上方继续堆叠（层间距 layoutStackSpacing），不向外扩。
 *  - 每 tick 全量重排：玩家移动时布局跟随，窗口尺寸/分辨率变化时自动自适应。
 *  - 默认所有顶层窗口自动加入（layoutAutoJoin），也可通过 /wl layout add/remove 手动指定。
 */
public class WindowCircleLayout {

	private final WaylandCraft wlc;

	private boolean enabled = true;

	/** 手动加入布局的窗口句柄（layoutAutoJoin=false 时只排这些窗口） */
	private final HashSet<Long> manualHandles = new HashSet<>();

	public WindowCircleLayout(WaylandCraft wlc) {
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

	/** 玩家视线正前方夹角（弧度）。第一个窗口放在正前方，后续按角度排开。 */
	private double baseAngle(Vec3 look) {
		Vec3 h = new Vec3(look.x, 0, look.z);
		if(h.lengthSqr() < 1e-6) h = new Vec3(0, 0, 1);
		h = h.normalize();
		return Math.atan2(h.x, h.z);
	}

	/** 每 tick 重排所有参与布局的窗口 */
	public void tick() {
		if(!enabled) return;
		if(wlc == null || wlc.settings == null) return;

		Minecraft mc = Minecraft.getInstance();
		LocalPlayer player = mc.player;
		if(player == null) return;

		List<WindowDisplay> list = participatingDisplays();
		if(list.isEmpty()) return;

		WaylandCraftSettings s = wlc.settings;
		double radius = s.getLayoutRadius();
		double spacing = s.getLayoutSpacing();
		double stackSpacing = s.getLayoutStackSpacing();
		if(radius < 0.5) radius = 0.5;
		if(spacing < 0) spacing = 0;

		Vec3 eye = player.getEyePosition();
		Vec3 look = WaylandCraftUtils.getLookVector(player);
		double base = baseAngle(look);

		double circumference = 2.0 * Math.PI * radius;
		double usedArc = 0.0;        // 当前层已用弧度
		double layerY = eye.y;       // 第一层窗口中心高度 = 玩家眼睛高度
		double layerMaxH = 0.0;      // 当前层最大窗口高度（用于换层时堆叠）
		boolean layerHasWindow = false;

		for(WindowDisplay d : list) {
			double w = worldWidth(d);
			double h = worldHeight(d);

			double arc = (w + spacing) / radius; // 该窗口占用的弧度（含间距）
			if(arc < 0.05) arc = 0.05;
			if(arc > circumference / radius) arc = circumference / radius;

			// 一层放不下 → 向上堆一层
			if(layerHasWindow && usedArc + arc > circumference) {
				layerY += layerMaxH + stackSpacing;
				usedArc = 0.0;
				layerMaxH = 0.0;
				layerHasWindow = false;
			}

			double angle = base + usedArc + arc / 2.0;
			double x = eye.x + Math.sin(angle) * radius;
			double z = eye.z + Math.cos(angle) * radius;

			Vec3 pivot = new Vec3(x, layerY, z);
			d.pivot = pivot;

			// 窗口始终面向玩家（法线指向玩家，保持竖直）
			Vec3 toPlayer = new Vec3(eye.x - x, 0, eye.z - z);
			if(toPlayer.lengthSqr() < 1e-6) toPlayer = new Vec3(0, 0, 1);
			d.rotate(toPlayer.normalize(), new Vec3(0, -1, 0));

			usedArc += arc;
			layerMaxH = Math.max(layerMaxH, h);
			layerHasWindow = true;
		}
	}

	/** 窗口世界宽度（格） */
	public static double worldWidth(WindowDisplay d) {
		return d.localX().length() * d.window.geometry.width();
	}

	/** 窗口世界高度（格） */
	public static double worldHeight(WindowDisplay d) {
		return d.localY().length() * d.window.geometry.height();
	}

}
