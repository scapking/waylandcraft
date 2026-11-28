package dev.evvie.waylandcraft.immerse;

import java.util.concurrent.ThreadLocalRandom;

import com.mojang.blaze3d.pipeline.BlendFunction;
import com.mojang.blaze3d.pipeline.ColorTargetState;
import com.mojang.blaze3d.platform.DestFactor;
import com.mojang.blaze3d.platform.SourceFactor;
import com.mojang.blaze3d.pipeline.DepthStencilState;
import com.mojang.blaze3d.pipeline.RenderPipeline;
import com.mojang.blaze3d.vertex.DefaultVertexFormat;
import com.mojang.blaze3d.vertex.PoseStack.Pose;
import com.mojang.blaze3d.vertex.VertexConsumer;
import com.mojang.blaze3d.vertex.VertexFormat;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.client.renderer.RenderPipelines;
import net.minecraft.client.renderer.rendertype.RenderSetup;
import net.minecraft.client.renderer.rendertype.RenderType;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.resources.Identifier;
import net.minecraft.world.phys.Vec3;
import net.fabricmc.fabric.api.client.rendering.v1.level.LevelRenderContext;
import org.joml.Quaternionf;
import org.joml.Vector3f;

/**
 * ⑩ v1.5.1 沉浸模式星空渲染器（双层结构，与用户验收的 HTML 预览 v3.2 同源）：
 *
 * 1. 远景天球（sky.vsh/fsh）：相机为中心的立方壳，方向场——银河（动态自转+尘埃
 *    流动）、星云、月亮、双远景恒星、黑洞吸积盘。方向场对位置无感 = 无限远，
 *    玩家移动时远景不动是物理正确的。
 * 2. 近星星点层（star.vsh/fsh）：**世界固定的 3D 星域**（以进入沉浸时的锚点为
 *    中心，半径 30..260 球壳体积；相对相机坐标按周期 520 折返 = 朝任何方向飞都
 *    有真实视差且永无边界）。星点为屏幕对齐 billboard（右/上取相机 orientation），
 *    加色混合，柔核+微辉+亮度驱动衍射尖峰。
 *
 * 提交：COLLECT_SUBMITS 里先于窗口提交（穹顶在底、星点其次、窗口最上）。
 * 关闭时零开销：一次 volatile 读。
 */
public final class ImmerseSkyRenderer {
	
	private ImmerseSkyRenderer() {}
	
	/**
	 * 立方壳半径（格）。⑩ v1.5.2 语义变更：壳是**贴身不透明遮挡壳**——
	 * 8 格外的一切世界内容（地形/实体/天空盒）被壳的深度遮挡"视觉消失"
	 * （等效沉浸，且与 Sodium/Iris 的地形路径零冲突——1.5.1 的
	 * LevelRendererImmerseMixin WrapOperation 在 Sodium 环境注入失败导致
	 * 启动崩溃，已删除）。窗口摆位默认 ≤6 格，在壳内自然可见。
	 * 贴脸（<8 格）的世界方块会穿出壳——如遇场景可将半径调大。
	 */
	private static final float SKY_RADIUS = 14.0f;
	/** 星域周期（格）：星域贴身化（半径 3..7.5 < 壳 8）→ 周期同步缩到 16，
	 *  飞行时星快速掠过（星际穿越式流动感），且永远在壳内可见 */
	private static final float STAR_PERIOD = 14.0f;   // 折返立方对角 12.1 < 壳 14：星永远在壳内
	private static final int STAR_COUNT = 700;
	private static final int BRIGHT_STAR_COUNT = 40;
	
	public static final RenderPipeline SKY_PIPELINE = RenderPipelines.register(
		RenderPipeline.builder()
		.withLocation(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "pipeline/immerse_sky"))
		.withVertexShader(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "immerse/sky"))
		.withFragmentShader(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "immerse/sky"))
		.withVertexFormat(DefaultVertexFormat.POSITION_TEX, VertexFormat.Mode.QUADS)
		.withDepthStencilState(DepthStencilState.DEFAULT)
		.withCull(false)
		.build()
	);
	
	public static final RenderPipeline STAR_PIPELINE = RenderPipelines.register(
		RenderPipeline.builder()
		.withLocation(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "pipeline/immerse_star"))
		.withVertexShader(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "immerse/star"))
		.withFragmentShader(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "immerse/star"))
		.withVertexFormat(DefaultVertexFormat.POSITION_TEX_COLOR, VertexFormat.Mode.QUADS)
		.withDepthStencilState(DepthStencilState.DEFAULT)
		.withColorTargetState(new ColorTargetState(
			new BlendFunction(
				SourceFactor.ONE,
				DestFactor.ONE)))
		.withCull(false)
		.build()
	);
	
	private static final class SkyTypeHolder {
		static final RenderType INSTANCE = RenderType.create("immerse_sky",
			RenderSetup.builder(SKY_PIPELINE).createRenderSetup());
		static final RenderType STAR_INSTANCE = RenderType.create("immerse_star",
			RenderSetup.builder(STAR_PIPELINE).createRenderSetup());
	}
	
	private static RenderType skyRenderType() {
		return SkyTypeHolder.INSTANCE;
	}
	
	private static RenderType starRenderType() {
		return SkyTypeHolder.STAR_INSTANCE;
	}
	
	// ===== 近星星域（世界固定；锚点变更时重新生成） =====
	/** 每星 8 float：世界 x/y/z、尺寸（格）、tint r/g/b、亮度（尖峰强度） */
	private static float[] starField = null;
	private static double anchorX, anchorY, anchorZ;
	
	/**
	 * 以锚点为中心重新生成星域（进入沉浸/锚点变更时由 ImmerseManager 调用，
	 * 任意线程——只写不可变数组后换引用）。
	 */
	public static void regenerateStarField(double ax, double ay, double az) {
		anchorX = ax;
		anchorY = ay;
		anchorZ = az;
		ThreadLocalRandom rnd = ThreadLocalRandom.current();
		float[] field = new float[(STAR_COUNT + BRIGHT_STAR_COUNT) * 8];
		int f = 0;
		for(int i = 0; i < STAR_COUNT + BRIGHT_STAR_COUNT; i++) {
			boolean bright = i >= STAR_COUNT;
			double th = rnd.nextDouble() * Math.PI * 2;
			double ph = Math.acos(2 * rnd.nextDouble() - 1);
			double rr = bright ? 4.0 + rnd.nextDouble() * 3.0 : 3.0 + Math.pow(rnd.nextDouble(), 0.6) * 4.5;
			float x = (float)(ax + rr * Math.sin(ph) * Math.cos(th));
			float y = (float)(ay + rr * Math.cos(ph));
			float z = (float)(az + rr * Math.sin(ph) * Math.sin(th));
			float size = bright ? (float)(0.22 + rnd.nextDouble() * 0.18) : (float)(0.05 + rnd.nextDouble() * 0.09);
			float t = rnd.nextFloat();
			float r, g, b;
			if(t < 0.20f)      { r = 0.72f; g = 0.82f; b = 1.00f; }  // 蓝白
			else if(t < 0.85f) { r = 0.93f; g = 0.95f; b = 1.00f; }  // 白
			else               { r = 1.00f; g = 0.85f; b = 0.62f; }  // 金橙
			float bright2 = bright ? 0.55f + rnd.nextFloat() * 0.45f : 0.10f + rnd.nextFloat() * 0.30f;
			field[f++] = x; field[f++] = y; field[f++] = z;
			field[f++] = size;
			field[f++] = r; field[f++] = g; field[f++] = b;
			field[f++] = bright2;
		}
		starField = field;
	}
	
	/**
	 * COLLECT_SUBMITS 入口（WaylandCraft.renderWorld 开头调用，渲染线程）：
	 * 先穹顶、后星点（窗口在更后提交 → 最上层）。
	 */
	public static void renderSky(LevelRenderContext ctx) {
		if(!ImmerseManager.isVisualActive()) return;
		float timeSec = (System.currentTimeMillis() % 3_600_000L) / 1000.0f;
		SubmitNodeCollector collector = ctx.submitNodeCollector();
		collector.submitCustomGeometry(ctx.poseStack(), skyRenderType(), new SkyInstance(timeSec));
		Vec3 cam = ctx.levelState().cameraRenderState.pos;
		Quaternionf rot = ctx.levelState().cameraRenderState.orientation;
		if(cam != null && rot != null && starField != null) {
			collector.submitCustomGeometry(ctx.poseStack(), starRenderType(),
				new StarFieldInstance(starField, cam, rot, timeSec));
		}
	}
	
	/**
	 * 穹顶几何实例：6 面立方壳。顶点位置 = 相机相对世界坐标，UV0.x = 时间（秒）。
	 */
	public record SkyInstance(float timeSec) implements SubmitNodeCollector.CustomGeometryRenderer {
		
		@Override
		public void render(Pose pose, VertexConsumer buffer) {
			float r = SKY_RADIUS;
			quad(buffer, pose, timeSec, -r, -r,  r,  r, -r,  r,  r,  r,  r, -r,  r,  r);
			quad(buffer, pose, timeSec,  r, -r, -r, -r, -r, -r, -r,  r, -r,  r,  r, -r);
			quad(buffer, pose, timeSec,  r, -r,  r,  r, -r, -r,  r,  r, -r,  r,  r,  r);
			quad(buffer, pose, timeSec, -r, -r, -r, -r, -r,  r, -r,  r,  r, -r,  r, -r);
			quad(buffer, pose, timeSec, -r,  r,  r,  r,  r,  r,  r,  r, -r, -r,  r, -r);
			quad(buffer, pose, timeSec, -r, -r, -r,  r, -r, -r,  r, -r,  r, -r, -r,  r);
		}
		
		private static void quad(VertexConsumer buffer, Pose pose, float time,
				float ax, float ay, float az, float bx, float by, float bz,
				float cx, float cy, float cz, float dx, float dy, float dz) {
			buffer.addVertex(pose, ax, ay, az).setUv(time, 0.0f);
			buffer.addVertex(pose, bx, by, bz).setUv(time, 0.0f);
			buffer.addVertex(pose, cx, cy, cz).setUv(time, 0.0f);
			buffer.addVertex(pose, dx, dy, dz).setUv(time, 0.0f);
		}
	}
	
	/**
	 * 近星星点实例：每星一个屏幕对齐 billboard quad。
	 * 相对相机坐标按 STAR_PERIOD 折返（无限星域）；右/上取相机 orientation
	 * （世界空间 billboard —— 星点在世界中固定朝向不随滚转）。
	 */
	public record StarFieldInstance(float[] field, Vec3 camPos, Quaternionf orientation, float timeSec)
			implements SubmitNodeCollector.CustomGeometryRenderer {
		
		@Override
		public void render(Pose pose, VertexConsumer buffer) {
			Vector3f right = new Vector3f(1, 0, 0).rotate(orientation);
			Vector3f up = new Vector3f(0, 1, 0).rotate(orientation);
			float halfPeriod = STAR_PERIOD * 0.5f;
			for(int i = 0; i < field.length; i += 8) {
				float relX = (float)(field[i] - camPos.x);
				float relY = (float)(field[i + 1] - camPos.y);
				float relZ = (float)(field[i + 2] - camPos.z);
				// 无限环绕折返（对称 mod）：飞多远都有星，永无边界
				relX = wrap(relX, halfPeriod);
				relY = wrap(relY, halfPeriod);
				relZ = wrap(relZ, halfPeriod);
				float size = field[i + 3];
				float rx = right.x * size, ry = right.y * size, rz = right.z * size;
				float ux = up.x * size, uy = up.y * size, uz = up.z * size;
				float r = field[i + 4], g = field[i + 5], b = field[i + 6];
				float bright = field[i + 7];
				// 世界相对坐标直接进 Position（该 poseStack 基准=相机原点，无旋转；
				// MVP 由 ModelViewMat/ProjMat 在 vsh 完成）
				addStar(buffer, pose, relX, relY, relZ, rx, ry, rz, ux, uy, uz, r, g, b, bright);
			}
		}
		
		private static float wrap(float v, float half) {
			return ((v + half) % (half * 2) + half * 2) % (half * 2) - half;
		}
		
		private static void addStar(VertexConsumer buffer, Pose pose,
				float x, float y, float z, float rx, float ry, float rz,
				float ux, float uy, float uz, float r, float g, float b, float bright) {
			// 屏幕对齐 billboard：position(x,y,z) + corner(right,up) —— 但 poseStack 无旋转，
			// 需要世界空间右/上 —— 直接以世界偏移叠加（经 ModelViewMat 旋转后即屏幕对齐）
			buffer.addVertex(pose, x - rx - ux, y - ry - uy, z - rz - uz).setUv(0.0f, 0.0f).setColor(r, g, b, bright);
			buffer.addVertex(pose, x + rx - ux, y + ry - uy, z + rz - uz).setUv(1.0f, 0.0f).setColor(r, g, b, bright);
			buffer.addVertex(pose, x + rx + ux, y + ry + uy, z + rz + uz).setUv(1.0f, 1.0f).setColor(r, g, b, bright);
			buffer.addVertex(pose, x - rx + ux, y - ry + uy, z - rz + uz).setUv(0.0f, 1.0f).setColor(r, g, b, bright);
		}
	}
}
