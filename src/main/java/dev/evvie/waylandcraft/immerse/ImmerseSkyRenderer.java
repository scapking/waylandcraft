package dev.evvie.waylandcraft.immerse;

import com.mojang.blaze3d.pipeline.DepthStencilState;
import com.mojang.blaze3d.pipeline.RenderPipeline;
import com.mojang.blaze3d.vertex.DefaultVertexFormat;
import com.mojang.blaze3d.vertex.PoseStack.Pose;
import com.mojang.blaze3d.vertex.VertexConsumer;
import com.mojang.blaze3d.vertex.VertexFormat;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import net.minecraft.client.renderer.RenderPipelines;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.client.renderer.rendertype.RenderSetup;
import net.minecraft.client.renderer.rendertype.RenderType;
import net.minecraft.resources.Identifier;
import net.fabricmc.fabric.api.client.rendering.v1.level.LevelRenderContext;

/**
 * ⑩ v1.5.0 沉浸模式星空穹顶渲染器。
 *
 * 结构（沿用本项目 1.4.x 渲染基建的既有范式）：
 * - 管线：POSITION_TEX + QUADS、无纹理无自定义 uniform——时间经 UV0.x 传入
 *   fragment（submit 路径没有 setUniform 时机，顶点属性是零依赖通道）；
 * - 几何：相机为中心的**立方壳**（6 面 ×4 顶点），COLLECT_SUBMITS 的 poseStack
 *   基准就是相机原点（WindowDisplay.render 同款 ctx），顶点位置即方向向量×半径，
 *   fragment normalize 后全向采样——任意朝向都是星空；
 * - 提交：WaylandCraft.renderWorld 开头调用（先于窗口提交 → 穹顶在底），
 *   深度近于天空盒（覆盖 vanilla 天空）、远于窗口（窗口自然覆盖穹顶）；
 * - 关闭时零开销：isVisualActive() 一次 volatile 读。
 */
public final class ImmerseSkyRenderer {
	
	private ImmerseSkyRenderer() {}
	
	/** 立方壳半径（格）：远于一切窗口交互距离，近于 vanilla 天空盒深度 */
	private static final float SKY_RADIUS = 150.0f;
	
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
	
	/** RenderType = 管线 + 无纹理绑定（星空全程序化，fragment 不采样） */
	private static final class SkyTypeHolder {
		static final RenderType INSTANCE = RenderType.create("immerse_sky",
			RenderSetup.builder(SKY_PIPELINE).createRenderSetup());
	}
	
	private static RenderType skyRenderType() {
		return SkyTypeHolder.INSTANCE;
	}
	
	/**
	 * COLLECT_SUBMITS 入口（WaylandCraft.renderWorld 开头调用，渲染线程）。
	 */
	public static void renderSky(LevelRenderContext ctx) {
		if(!ImmerseManager.isVisualActive()) return;
		float timeSec = (System.currentTimeMillis() % 3_600_000L) / 1000.0f;
		ctx.submitNodeCollector().submitCustomGeometry(ctx.poseStack(), skyRenderType(), new SkyInstance(timeSec));
	}
	
	/**
	 * 穹顶几何实例：6 面立方壳。顶点位置 = 相机相对坐标，UV0.x = 时间（秒）。
	 */
	public record SkyInstance(float timeSec) implements SubmitNodeCollector.CustomGeometryRenderer {
		
		@Override
		public void render(Pose pose, VertexConsumer buffer) {
			float r = SKY_RADIUS;
			// 6 面立方壳（cull=false，绕序仅影响诊断）
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
}
