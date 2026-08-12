package dev.evvie.waylandcraft.render;

import org.jetbrains.annotations.Nullable;

import com.mojang.blaze3d.vertex.PoseStack;

import dev.evvie.waylandcraft.WindowDisplay;
import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.bridge.WLCAbstractWindow;
import dev.evvie.waylandcraft.math.WorldPlane;
import dev.evvie.waylandcraft.shared.RemoteWindowRenderer;
import dev.evvie.waylandcraft.shared.WindowPermission;
import net.fabricmc.fabric.api.client.rendering.v1.level.LevelRenderContext;
import net.minecraft.client.Camera;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.client.renderer.rendertype.RenderType;
import net.minecraft.resources.Identifier;
import net.minecraft.world.phys.Vec3;

/**
 * 共享窗口显示类
 * 用于显示远程玩家共享的窗口
 */
public class SharedWindowDisplay {
	
	private final long windowHandle;
	private final String windowTitle;
	private final String ownerName;
	
	// 窗口位置和方向
	private Vec3 pivot = new Vec3(0, 0, 0);
	private Vec3 normal = new Vec3(0, 0, 1);
	private Vec3 down = new Vec3(0, -1, 0);
	
	// 窗口尺寸（framebuffer 尺寸，用于四边形渲染）
	private int width;
	private int height;
	
	// 视觉缩放倍数（与本地 WindowDisplay.viewScale 一致）
	private double viewScale = 1.0;
	// geometry 尺寸（与本地 WindowDisplay.updateGeometry 的 width/height 一致，用于 origin 居中）
	private int geometryWidth;
	private int geometryHeight;
	
	// 发送端自己的 pixelsPerBlock（0=未收到，退回本地设置）：
	// 共享窗口必须用发送端的 PPB 渲染，否则两端 PPB 不同 → 世界尺寸不同
	private int senderPixelsPerBlock = 0;
	
	// framebuffer 内容偏移（与本地 WindowDisplay.render 的 xoff/yoff 语义一致）
	private int xoff;
	private int yoff;
	
	// 权限
	private WindowPermission permission = WindowPermission.VIEW;
	
	// 渲染器
	private final RemoteWindowRenderer renderer;
	
	// 是否可见
	private boolean visible = true;
	
	// 锚定距离
	public double anchorDistance = 2.0;
	
	// 上次触发垂直钳制时的窗口尺寸（用于检测 resize 后重新钳制）
	private int lastClampWidth = -1;
	private int lastClampHeight = -1;
	
	// 窗口底部距地面的最小净空（方块），与本地 WindowDisplay 一致
	public static final double GROUND_CLEARANCE = 0.4;
	
	public SharedWindowDisplay(long windowHandle, String windowTitle, String ownerName, RemoteWindowRenderer renderer) {
		this.windowHandle = windowHandle;
		this.windowTitle = windowTitle;
		this.ownerName = ownerName;
		this.renderer = renderer;
	}
	
	/**
	 * 获取窗口句柄
	 */
	public long getWindowHandle() {
		return windowHandle;
	}
	
	/**
	 * 获取窗口 framebuffer 宽度（像素；未更新时返回 0）
	 */
	public int getWidth() {
		return width;
	}
	
	/**
	 * 获取窗口 framebuffer 高度（像素；未更新时返回 0）
	 */
	public int getHeight() {
		return height;
	}
	
	/**
	 * 获取窗口标题
	 */
	public String getWindowTitle() {
		return windowTitle;
	}
	
	/**
	 * 获取所有者名称
	 */
	public String getOwnerName() {
		return ownerName;
	}
	
	/**
	 * 设置权限
	 */
	public void setPermission(WindowPermission permission) {
		this.permission = permission;
	}
	
	/**
	 * 获取权限
	 */
	public WindowPermission getPermission() {
		return permission;
	}
	
	/**
	 * 设置可见性
	 */
	public void setVisible(boolean visible) {
		this.visible = visible;
	}
	
	/**
	 * 是否可见
	 */
	public boolean isVisible() {
		return visible;
	}
	
	/**
	 * 更新窗口位置
	 */
	public void updatePosition(int x, int y) {
		// 将屏幕坐标转换为世界坐标
		// 这里简化处理，实际需要根据窗口朝向计算
	}
	
	/**
	 * 设置窗口变换（来自发送者的原始WindowDisplay的pivot/normal/down）
	 */
	public void setTransform(Vec3 pivot, Vec3 normal, Vec3 down) {
		this.pivot = pivot;
		this.normal = normal;
		this.down = down;
	}
	
	/**
	 * 设置所有者世界坐标（窗口显示在该位置）— 兼容旧接口
	 */
	public void setWorldPosition(double x, double y, double z) {
		this.pivot = new Vec3(x, y, z);
	}
	
	/**
	 * 更新窗口大小（原始 framebuffer 尺寸，非缩放）
	 */
	public void updateSize(int width, int height) {
		this.width = width;
		this.height = height;
	}
	
	/**
	 * 设置 framebuffer 内容偏移（xoff/yoff），与本地 WindowDisplay.render 的 bufOffset 对齐
	 */
	public void setBufferOffset(int xoff, int yoff) {
		this.xoff = xoff;
		this.yoff = yoff;
	}
	
	/**
	 * 设置视觉缩放倍数（与本地 WindowDisplay.viewScale 一致）
	 */
	public void setViewScale(double viewScale) {
		this.viewScale = viewScale;
	}
	
	/**
	 * 设置 geometry 尺寸（与本地 WindowDisplay.updateGeometry 的 width/height 一致）
	 */
	public void setGeometrySize(int width, int height) {
		this.geometryWidth = width;
		this.geometryHeight = height;
	}
	
	/**
	 * 设置发送端 pixelsPerBlock（来自 ImagePayload），接收端用它渲染保证尺寸一致
	 */
	public void setSenderPixelsPerBlock(int ppb) {
		if(ppb > 0) this.senderPixelsPerBlock = ppb;
	}
	
	/**
	 * 获取像素缩放比例 — 优先用发送端 PPB（保证共享窗口世界尺寸与发送端一致），
	 * 未收到时退回本地设置；native 不可用的纯查看端 settings 可能为 null，退回默认 500 ppb
	 */
	public float pixelScale() {
		if(senderPixelsPerBlock > 0) return 1.0f / senderPixelsPerBlock;
		var s = WaylandCraft.instance.settings;
		return 1.0f / (s != null ? s.getPixelsPerBlock() : 500);
	}
	
	/**
	 * 获取局部X轴方向 — 与本地 WindowDisplay.localX() 一致（含 viewScale）
	 */
	public Vec3 localX() {
		return normal.cross(down).scale(pixelScale() * viewScale);
	}
	
	/**
	 * 获取局部Y轴方向 — 与本地 WindowDisplay.localY() 一致（含 viewScale）
	 */
	public Vec3 localY() {
		return down.scale(pixelScale() * viewScale);
	}
	
	/**
	 * 获取原点位置 — 与本地 WindowDisplay.origin() 一致：
	 * 使用 geometry 尺寸（而非 framebuffer 尺寸）居中，
	 * 保证与本地窗口在世界上完全对齐。
	 */
	public Vec3 origin() {
		int w = geometryWidth > 0 ? geometryWidth : width;
		int h = geometryHeight > 0 ? geometryHeight : height;
		return pivot.add(localX().scale(-w/2)).add(localY().scale(-h/2));
	}
	
	/**
	 * 获取世界平面
	 */
	public WorldPlane getPlane() {
		return new WorldPlane(origin(), localX(), localY(), normal);
	}
	
	/**
	 * 射线检测：返回窗口内像素坐标（相对窗口左上角，含 xoff/yoff 修正）与距离。
	 * 与本地 WindowDisplay.intersect 一致：只命中正面（dist>=0 由 WorldPlane 处理），
	 * 且落在 framebuffer 范围内才算命中。
	 */
	public @Nullable SharedHit intersect(Vec3 pos, Vec3 dir) {
		WorldPlane.Intersection inter = getPlane().intersect(pos, dir);
		if(inter == null) return null;
		
		Vec3 local = inter.local();
		// 窗口渲染从 origin + bufOffset 开始，所以窗口内像素坐标 = 相对 origin 的像素 + xoff/yoff
		double px = local.x + xoff;
		double py = local.y + yoff;
		int w = width > 0 ? width : 1;
		int h = height > 0 ? height : 1;
		if(px < 0 || py < 0 || px > w || py > h) return null;
		
		return new SharedHit(this, px, py, inter.dist());
	}
	
	/**
	 * 世界坐标 → 窗口内像素坐标（相对窗口左上角，含 xoff/yoff 修正）。
	 * 超出窗口范围时返回 null。
	 */
	public @Nullable Vec3 worldToWindowPixel(Vec3 world) {
		Vec3 local = getPlane().worldToLocal(world);
		double px = local.x + xoff;
		double py = local.y + yoff;
		int w = width > 0 ? width : 1;
		int h = height > 0 ? height : 1;
		if(px < 0 || py < 0 || px > w || py > h) return null;
		return new Vec3(px, py, local.z);
	}
	
	/**
	 * 获取当前 pivot（世界坐标）
	 */
	public Vec3 getPivot() {
		return pivot;
	}
	
	/**
	 * 平移窗口（世界坐标增量），并保持朝向不变。
	 */
	public void moveBy(Vec3 delta) {
		this.pivot = this.pivot.add(delta);
	}
	
	/**
	 * 射线命中结果
	 */
	public static record SharedHit(SharedWindowDisplay display, double x, double y, double dist) {
	}
	
	/**
	 * 旋转窗口
	 */
	public void rotate(Vec3 normal, Vec3 down) {
		this.normal = normal;
		this.down = down;
	}
	
	/**
	 * 移动原点
	 */
	public void moveOrigin(Vec3 pos) {
		pivot = pos.add(localX().scale(width/2)).add(localY().scale(height/2));
	}
	
	/**
	 * 锚定到位置和视角
	 */
	public void anchorToPosView(Vec3 pos, Vec3 look, Vec3 up) {
		this.pivot = pos.add(look.scale(this.anchorDistance));
		this.rotate(look.reverse(), up.reverse());
	}
	
	/**
	 * 锚定到相机
	 */
	public void anchorToCamera(Camera camera) {
		anchorToPosView(camera.position(), new Vec3(camera.forwardVector()), new Vec3(camera.upVector()));
	}
	
	/**
	 * 调整锚定距离
	 */
	public void adjustAnchorDistance(double delta) {
		this.anchorDistance = Math.clamp(this.anchorDistance + delta * 0.1d, 0.5d, 20d);
	}
	
	/**
	 * 垂直约束 — 共享窗口不执行！
	 * 发送端本地 WindowDisplay 每帧已做垂直钳制，传过来的 pivot/normal/down
	 * 就是钳制后的最终摆放。接收端若再钳制一次，会把发送端贴在墙/天花板
	 * 或任意角度摆放的窗口强制拉回竖直+贴地 → x/y/z 与本地不一致。
	 */
	public void clampVertical() {
		// no-op：共享窗口位置/朝向完全由发送端决定
	}
	
	/**
	 * 窗口分辨率变化后自动重新执行垂直钳制（尺寸变化才触发）。
	 * no-op：共享窗口不钳制，见 clampVertical()。
	 */
	public void clampIfResized() {
		// no-op：共享窗口位置/朝向完全由发送端决定
	}
	
	/**
	 * 渲染共享窗口 — 与WindowDisplay.render()完全相同的渲染逻辑
	 * 使用renderFramebufferTexture（同一套WINDOW_CUTOUT/WINDOW_TRANSLUCENT管线）
	 */
	public void render(LevelRenderContext ctx) {
		if(!visible) return;
		if(!renderer.hasTexture(windowHandle)) return;
		
		Identifier textureLocation = renderer.getTextureLocation_obj(windowHandle);
		if(textureLocation == null) return;
		
		// 始终使用原始 framebuffer 尺寸（与本地 WindowDisplay 一致），
		// 纹理（可能被发送端缩放）通过 UV 0..1 拉伸到整个四边形。
		// 若用纹理尺寸渲染，发送端 scale<1 时窗口会变小 → 与本地不一致。
		int renderWidth = this.width;
		int renderHeight = this.height;
		if(renderWidth <= 0 || renderHeight <= 0) {
			// 兜底：纹理尺寸
			int[] dims = renderer.getTextureDimensions(windowHandle);
			if(dims != null && dims[0] > 0 && dims[1] > 0) {
				renderWidth = dims[0];
				renderHeight = dims[1];
			} else {
				return;
			}
		}
		
		// 与WindowDisplay.render()完全一致的向量计算
		Vec3 localX = localX();
		Vec3 localY = localY();

		Vec3 cameraPos = ctx.levelState().cameraRenderState.pos;
		Vec3 originRel = origin().subtract(cameraPos);

		// framebuffer 内容偏移（xoff/yoff），与本地 WindowDisplay.render 的 bufOffset 一致
		Vec3 bufOffset = localX.scale(-xoff).add(localY.scale(-yoff));

		Vec3 tl = bufOffset;
		Vec3 bl = bufOffset.add(localY.scale(renderHeight));
		Vec3 br = bl.add(localX.scale(renderWidth));
		Vec3 tr = tl.add(localX.scale(renderWidth));
		
		PoseStack poseStack = ctx.poseStack();
		poseStack.pushPose();
		poseStack.translate(originRel.x, originRel.y, originRel.z);
		// 使用V-flip版本的渲染管线（远程纹理是top-down的，需要翻转V坐标）
		RenderUtils.renderRemoteFramebufferTexture(textureLocation, poseStack, ctx.submitNodeCollector(), true, tl, bl, br, tr);
		poseStack.popPose();
	}
	
	/**
	 * 窗口是否有效
	 */
	public boolean isValid() {
		return visible && renderer.hasTexture(windowHandle);
	}
	
	@Override
	public boolean equals(Object obj) {
		if(this == obj) return true;
		if(!(obj instanceof SharedWindowDisplay)) return false;
		SharedWindowDisplay other = (SharedWindowDisplay) obj;
		return windowHandle == other.windowHandle;
	}
	
	@Override
	public int hashCode() {
		return Long.hashCode(windowHandle);
	}
}
