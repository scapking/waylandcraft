package dev.evvie.waylandcraft.grabs;

import org.lwjgl.glfw.GLFW;

import com.mojang.blaze3d.platform.InputConstants;

import dev.evvie.waylandcraft.WindowDisplay;
import dev.evvie.waylandcraft.grabs.PointerGrabMap.ImplicitGrab;
import dev.evvie.waylandcraft.math.WorldPlane;
import dev.evvie.waylandcraft.math.WorldPlane.Intersection;
import dev.evvie.waylandcraft.utils.CursorShape;
import net.minecraft.client.Minecraft;
import net.minecraft.world.phys.Vec3;

public class MoveGrab extends PointerGrab {

	public final WindowDisplay window;
	public final WorldPlane plane;
	public final Vec3 initialLocal;
	public final Vec3 initialPivot;
	public final Vec3 initialWorld;
	public boolean snapActive = false;

	public MoveGrab(ImplicitGrab implicit) {
		super(implicit.button());
		this.window = implicit.window();
		this.initialLocal = implicit.startGeometryLocal();
		this.initialPivot = window.pivot;
		this.initialWorld = implicit.startWorldPos();
		this.plane = window.getPlane();
	}

	@Override
	public void init() throws GrabDroppedException {
		if(!window.isValid()) this.drop();
		// 抓取时先把窗口转正（竖直放置、高度合规）
		window.clampVertical();
	}

	@Override
	public void release(boolean force) throws GrabDroppedException {
		if(!window.isValid()) this.drop();
		window.clampVertical();
	}

	@Override
	public void moveWorld(Vec3 pos, Vec3 view, Vec3 up, float yRot, float xRot) throws GrabDroppedException {
		if(!window.isValid()) this.drop();

		wlc.cursorShape = CursorShape.ALL_RESIZE;

		// 上游 d2047ca：对抓取开始时冻结的平面求交（而不是每帧重建的窗口平面），
		// 光标移出窗口区域也能继续拖动，且位移锚定 initialPivot 避免逐帧累加漂移。
		Intersection intersect = plane.intersect(pos, view);
		if(intersect == null) return;

		Vec3 worldDiff = initialWorld.subtract(intersect.world());

		Vec3 diff = intersect.local().subtract(initialLocal);
		snapActive = InputConstants.isKeyDown(Minecraft.getInstance().getWindow(), GLFW.GLFW_KEY_LEFT_CONTROL);
		if(snapActive) {
			// 上游单轴吸附：靠近起点时归零，否则按局部平面内占优轴单轴移动
			double minDist = 0.25;
			if(max3(Math.abs(worldDiff.x), Math.abs(worldDiff.y), Math.abs(worldDiff.z)) < minDist) {
				diff = Vec3.ZERO;
			}
			else if(Math.abs(diff.x) > Math.abs(diff.y)) {
				diff = new Vec3(diff.x, 0, 0);
			}
			else {
				diff = new Vec3(0, diff.y, 0);
			}
		}
		else {
			// fork 现行为：垂直轴固定，只允许水平方向（localX）拖动，y 保持不变
			diff = new Vec3(diff.x, 0, 0);
		}

		window.pivot = initialPivot.add(plane.localX.scale(diff.x)).add(plane.localY.scale(diff.y));
		window.clampVertical();
	}

	private static double max3(double a, double b, double c) {
		return Math.max(a, Math.max(b, c));
	}

	@Override
	public void onScroll(double scrollX, double scrollY) throws GrabDroppedException {
		if(!window.isValid()) this.drop();

		boolean ctrl = InputConstants.isKeyDown(Minecraft.getInstance().getWindow(), GLFW.GLFW_KEY_LEFT_CONTROL);
		boolean alt = InputConstants.isKeyDown(Minecraft.getInstance().getWindow(), GLFW.GLFW_KEY_LEFT_ALT);

		if(ctrl && !alt) {
			// Ctrl+滚轮 = 绕法线轴旋转（与悬停时一致）
			window.rotateBy(scrollY * 0.1);
		} else if(alt && !ctrl) {
			// Alt+滚轮 = 调整缩放
			window.adjustScale(scrollY);
		} else {
			// 普通滚轮 = 沿法线方向前后移动（保持垂直）
			window.pivot = window.pivot.add(window.normal().scale(scrollY * 0.1));
			window.clampVertical();
		}
	}

}
