package dev.evvie.waylandcraft.render;

import java.util.function.Function;
import java.util.function.Supplier;

import com.mojang.blaze3d.pipeline.BlendFunction;
import com.mojang.blaze3d.pipeline.ColorTargetState;
import com.mojang.blaze3d.pipeline.DepthStencilState;
import com.mojang.blaze3d.pipeline.RenderPipeline;
import com.mojang.blaze3d.systems.RenderSystem;
import com.mojang.blaze3d.textures.AddressMode;
import com.mojang.blaze3d.textures.FilterMode;
import com.mojang.blaze3d.textures.GpuSampler;
import com.mojang.blaze3d.vertex.DefaultVertexFormat;
import com.mojang.blaze3d.vertex.PoseStack;
import com.mojang.blaze3d.vertex.PoseStack.Pose;
import com.mojang.blaze3d.vertex.VertexConsumer;
import com.mojang.blaze3d.vertex.VertexFormat;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.mixin.IGuiGraphicsExtractor;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.renderer.RenderPipelines;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.client.renderer.SubmitNodeCollector.CustomGeometryRenderer;
import net.minecraft.client.renderer.rendertype.RenderSetup;
import net.minecraft.client.renderer.rendertype.RenderType;
import net.minecraft.client.renderer.rendertype.RenderTypes;
import net.minecraft.client.renderer.texture.OverlayTexture;
import net.minecraft.resources.Identifier;
import net.minecraft.util.Util;
import net.minecraft.world.phys.Vec3;

public class RenderUtils {
	
	private static final RenderPipeline.Snippet WINDOW_PIPELINE_SNIPPET = RenderPipeline.builder(RenderPipelines.MATRICES_PROJECTION_SNIPPET)
			.withVertexShader(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "core/rendertype_window"))
			.withFragmentShader(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "core/rendertype_window"))
			.withSampler("Sampler0")
			.withDepthStencilState(DepthStencilState.DEFAULT)
			.withVertexFormat(DefaultVertexFormat.POSITION_TEX, VertexFormat.Mode.QUADS)
			.buildSnippet();
	
	private static final RenderPipeline WINDOW_CUTOUT_PIPELINE = RenderPipeline.builder(WINDOW_PIPELINE_SNIPPET)
			.withLocation(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "pipeline/window_cutout"))
			.withShaderDefine("ALPHA_CUTOUT")
			.build();
	
	private static final RenderPipeline WINDOW_TRANSLUCENT_PIPELINE = RenderPipeline.builder(WINDOW_PIPELINE_SNIPPET)
			.withLocation(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "pipeline/window_translucent"))
			.withColorTargetState(new ColorTargetState(BlendFunction.TRANSLUCENT))
			.build();
	
	private static final RenderPipeline WINDOW_CUTOUT_ANTIALIASING_PIPELINE = RenderPipeline.builder(WINDOW_PIPELINE_SNIPPET)
			.withLocation(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "pipeline/window_cutout"))
			.withShaderDefine("ALPHA_CUTOUT")
			.withShaderDefine("RGSS")
			.build();
	
	private static final RenderPipeline WINDOW_TRANSLUCENT_ANTIALIASING_PIPELINE = RenderPipeline.builder(WINDOW_PIPELINE_SNIPPET)
			.withLocation(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "pipeline/window_translucent"))
			.withColorTargetState(new ColorTargetState(BlendFunction.TRANSLUCENT))
			.withShaderDefine("RGSS")
			.build();
	
	private static final RenderPipeline WINDOW_CUTOUT_BACKGROUND_PIPELINE = RenderPipeline.builder(WINDOW_PIPELINE_SNIPPET)
			.withLocation(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "pipeline/window_cutout_background"))
			.withShaderDefine("ALPHA_CUTOUT")
			.withShaderDefine("NO_COLOR")
			.build();
	
	private static final RenderPipeline WINDOW_TRANSLUCENT_BACKGROUND_PIPELINE = RenderPipeline.builder(WINDOW_PIPELINE_SNIPPET)
			.withLocation(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "pipeline/window_translucent_background"))
			.withShaderDefine("NO_COLOR")
			.withColorTargetState(new ColorTargetState(BlendFunction.TRANSLUCENT))
			.build();
	
	public static final Supplier<GpuSampler> WINDOW_SAMPLER = () -> RenderSystem.getSamplerCache().getSampler(AddressMode.CLAMP_TO_EDGE, AddressMode.CLAMP_TO_EDGE, FilterMode.LINEAR, FilterMode.NEAREST, false);
	
	public static final Function<Identifier, RenderType> WINDOW_CUTOUT = Util.memoize(
		(identifier) -> {
			RenderSetup setup = RenderSetup.builder(WINDOW_CUTOUT_PIPELINE)
					.withTexture("Sampler0", identifier, WINDOW_SAMPLER)
					.createRenderSetup();
			return RenderType.create("window_cutout", setup);
		}
	);
	
	public static final Function<Identifier, RenderType> WINDOW_TRANSLUCENT = Util.memoize(
		(identifier) -> {
			RenderSetup setup = RenderSetup.builder(WINDOW_TRANSLUCENT_PIPELINE)
					.withTexture("Sampler0", identifier, WINDOW_SAMPLER)
					.createRenderSetup();
			return RenderType.create("window_translucent", setup);
		}
	);
	
	public static final Function<Identifier, RenderType> WINDOW_CUTOUT_ANTIALIAS = Util.memoize(
		(identifier) -> {
			RenderSetup setup = RenderSetup.builder(WINDOW_CUTOUT_ANTIALIASING_PIPELINE)
					.withTexture("Sampler0", identifier, WINDOW_SAMPLER)
					.createRenderSetup();
			return RenderType.create("window_cutout_antialias", setup);
		}
	);
	
	public static final Function<Identifier, RenderType> WINDOW_TRANSLUCENT_ANTIALIAS = Util.memoize(
		(identifier) -> {
			RenderSetup setup = RenderSetup.builder(WINDOW_TRANSLUCENT_ANTIALIASING_PIPELINE)
					.withTexture("Sampler0", identifier, WINDOW_SAMPLER)
					.createRenderSetup();
			return RenderType.create("window_translucent_antialias", setup);
		}
	);
	
	public static final Function<Identifier, RenderType> WINDOW_BACKGROUND_CUTOUT = Util.memoize(
		(identifier) -> {
			RenderSetup setup = RenderSetup.builder(WINDOW_CUTOUT_BACKGROUND_PIPELINE)
					.withTexture("Sampler0", identifier, WINDOW_SAMPLER)
					.createRenderSetup();
			return RenderType.create("window_cutout_background", setup);
		}
	);
	
	public static final Function<Identifier, RenderType> WINDOW_BACKGROUND_TRANSLUCENT = Util.memoize(
		(identifier) -> {
			RenderSetup setup = RenderSetup.builder(WINDOW_TRANSLUCENT_BACKGROUND_PIPELINE)
					.withTexture("Sampler0", identifier, WINDOW_SAMPLER)
					.createRenderSetup();
		return RenderType.create("window_translucent_background", setup);
		}
	);
	
	public static final RenderPipeline WINDOW_BLIT = RenderPipeline.builder(RenderPipelines.MATRICES_PROJECTION_SNIPPET)
		.withLocation(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "pipeline/window_blit"))
		.withVertexShader(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "core/window_blit"))
			.withFragmentShader(Identifier.fromNamespaceAndPath(WaylandCraftCommon.MOD_ID, "core/window_blit"))
			.withSampler("Sampler0")
			.withColorTargetState(new ColorTargetState(BlendFunction.TRANSLUCENT))
			.withVertexFormat(DefaultVertexFormat.POSITION_TEX_COLOR, VertexFormat.Mode.QUADS)
			.build();
	
	// === Iris（光影）兼容回退：使用原版管线替代自定义管线 ===
	// Iris 拦截所有自定义 RenderPipeline（iris$redirectIrisProgram），自定义 rendertype_window
	// 不被认识 → 抛异常。原版 entity 管线 Iris 认识，可正常渲染。
	// 注意：原版 entity shader 会乘 diffuse light + lightmap，这里用全亮 lightmap(0xF000F0)
	// 保证窗口内容始终满亮度显示（与自定义管线一致）。
	private static final Function<Identifier, RenderType> VANILLA_ENTITY_CUTOUT = Util.memoize(
		(Identifier identifier) -> RenderTypes.entityCutout(identifier)
	);
	private static final Function<Identifier, RenderType> VANILLA_ENTITY_TRANSLUCENT = Util.memoize(
		(Identifier identifier) -> RenderTypes.entityTranslucent(identifier)
	);
	
	/** entity 顶点格式的写顶点辅助（Position+Color+UV0+UV1(overlay)+UV2(light)+Normal） */
	private static void writeVanillaVertex(VertexConsumer buffer, Pose pose, Vec3 p, float u, float v, int r, int g, int b, int a) {
		buffer.addVertex(pose, p.toVector3f())
			.setColor(r, g, b, a)
			.setUv(u, v)
			.setOverlay(OverlayTexture.NO_OVERLAY)
			.setLight(0xF000F0) // 全亮 lightmap（block 15 + sky 15）
			.setNormal(0.0f, 0.0f, 1.0f);
	}
	
	/**
	 * Iris 兼容模式下的窗口几何实例 — 用原版 entity 管线渲染。
	 * 支持双面：正面（reverse=false）贴图白色，背面（reverse=true）反向绕序纯色，
	 * 与本地自定义管线的 front/back 双四边形行为一致。
	 */
	public static final record VanillaWindowRenderInstance(Vec3 tl, Vec3 bl, Vec3 br, Vec3 tr, boolean flipV, boolean reverse, int r, int g, int b, int a) implements CustomGeometryRenderer {
		
		@Override
		public void render(Pose pose, VertexConsumer buffer) {
			if(!reverse) {
				if(!flipV) {
					writeVanillaVertex(buffer, pose, tl, 0.0f, 0.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, bl, 0.0f, 1.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, br, 1.0f, 1.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, tr, 1.0f, 0.0f, r, g, b, a);
				}
				else {
					writeVanillaVertex(buffer, pose, tl, 0.0f, 1.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, bl, 0.0f, 0.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, br, 1.0f, 0.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, tr, 1.0f, 1.0f, r, g, b, a);
				}
			}
			else {
				if(!flipV) {
					writeVanillaVertex(buffer, pose, tr, 1.0f, 0.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, br, 1.0f, 1.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, bl, 0.0f, 1.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, tl, 0.0f, 0.0f, r, g, b, a);
				}
				else {
					writeVanillaVertex(buffer, pose, tr, 1.0f, 1.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, br, 1.0f, 0.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, bl, 0.0f, 0.0f, r, g, b, a);
					writeVanillaVertex(buffer, pose, tl, 0.0f, 1.0f, r, g, b, a);
				}
			}
		}
		
	}
	
	public static void renderFramebuffer(WindowFramebuffer framebuffer, PoseStack poseStack, SubmitNodeCollector collector, boolean cutout, Vec3 tl, Vec3 bl, Vec3 br, Vec3 tr) {
		if(!framebuffer.isValid()) return;
		renderWindowTexture(framebuffer.getTextureLocation(), poseStack, collector, cutout, false, tl, bl, br, tr);
	}
	
	/**
	 * 统一窗口纹理渲染入口 — 本地帧缓冲与远程共享纹理共用同一套渲染逻辑
	 * 
	 * 同一管线（WINDOW_CUTOUT/WINDOW_TRANSLUCENT + BACKGROUND），同一几何实例
	 * （WindowRenderInstance），仅通过 flipV 区分纹理来源：
	 * - flipV=false: 本地 Wayland framebuffer（bottom-up）
	 * - flipV=true:  远程共享纹理（glReadPixels 捕获为 top-down，需翻转 V）
	 * 
	 * cutout=true: WINDOW_CUTOUT管线（不透明内容）
	 * cutout=false: WINDOW_TRANSLUCENT管线（半透明内容）
	 */
	public static void renderWindowTexture(Identifier textureLocation, PoseStack poseStack, SubmitNodeCollector collector, boolean cutout, boolean flipV, Vec3 tl, Vec3 bl, Vec3 br, Vec3 tr) {
		if(textureLocation == null) return;
		
		// Iris 光影兼容：自定义管线会被 Iris 拦截抛异常，改用原版 entity 管线
		if(IrisCompat.isIrisLoaded()) {
			Function<Identifier, RenderType> rt = cutout ? VANILLA_ENTITY_CUTOUT : VANILLA_ENTITY_TRANSLUCENT;
			// 正面：贴图白色
			collector.submitCustomGeometry(poseStack, rt.apply(textureLocation), new VanillaWindowRenderInstance(tl, bl, br, tr, flipV, false, 255, 255, 255, 255));
			// 背面：纯黑（与本地 NO_COLOR 的 vec4(vec3(0.0)) 一致）。
			// 原版 entity 管线无 cull，若不偏移两面会 z-fighting；沿法线反向退后一点，
			// 从前面看正面获胜、从后面看背面纯黑获胜。
			Vec3 n = bl.subtract(tl).cross(br.subtract(tl)).normalize();
			Vec3 off = n.scale(-0.01);
			collector.submitCustomGeometry(poseStack, rt.apply(textureLocation), new VanillaWindowRenderInstance(
				tl.add(off), bl.add(off), br.add(off), tr.add(off), flipV, true, 0, 0, 0, 255));
			return;
		}
		
		Function<Identifier, RenderType> renderType;
		
		// Front quad
		// native 不可用的纯查看端（Android 手机）settings 可能为 null，退回默认（无抗锯齿）
		boolean antialias = WaylandCraft.instance.settings != null && WaylandCraft.instance.settings.getAntialiasing();
		if(antialias) renderType = cutout ? WINDOW_CUTOUT_ANTIALIAS : WINDOW_TRANSLUCENT_ANTIALIAS;
		else renderType = cutout ? WINDOW_CUTOUT : WINDOW_TRANSLUCENT;
		collector.submitCustomGeometry(poseStack, renderType.apply(textureLocation), new WindowRenderInstance(tl, bl, br, tr, false, flipV));
		
		// Back quad — 沿法线反向退 0.01 block：
		// 共面双四边形 + 严格 LESS 深度测试时，背面 quad 会因深度相等被 front 挡住，
		// 从背面看到的是 front 的镜像画面（与 Iris 单面时同样的问题）。偏移后背面
		// 从背面视角更近 → 深度测试通过 → NO_COLOR 纯黑正确显示；正面视角则被 front 挡掉。
		Vec3 n = bl.subtract(tl).cross(br.subtract(tl)).normalize();
		Vec3 off = n.scale(-0.01);
		renderType = cutout ? WINDOW_BACKGROUND_CUTOUT : WINDOW_BACKGROUND_TRANSLUCENT;
		collector.submitCustomGeometry(poseStack, renderType.apply(textureLocation), new WindowRenderInstance(tl.add(off), bl.add(off), br.add(off), tr.add(off), true, flipV));
	}
	
	/**
	 * 渲染远程共享纹理（薄封装）— V坐标翻转版本
	 * 远程纹理通过glReadPixels捕获是top-down的，但shader UV假设bottom-up
	 * 需要翻转V坐标(0↔1)来纠正上下方向
	 */
	public static void renderRemoteFramebufferTexture(Identifier textureLocation, PoseStack poseStack, SubmitNodeCollector collector, boolean cutout, Vec3 tl, Vec3 bl, Vec3 br, Vec3 tr) {
		renderWindowTexture(textureLocation, poseStack, collector, cutout, true, tl, bl, br, tr);
	}
	
	/**
	 * 统一窗口几何实例 — 本地（flipV=false）与远程（flipV=true）共用
	 * reverse=true 渲染背面（内容翻转），flipV=true 时 UV 的 V 坐标 0↔1 翻转
	 */
	public static final record WindowRenderInstance(Vec3 tl, Vec3 bl, Vec3 br, Vec3 tr, boolean reverse, boolean flipV) implements CustomGeometryRenderer {
		
		@Override
		public void render(Pose pose, VertexConsumer buffer) {
			if(!reverse) {
				if(!flipV) {
					buffer.addVertex(pose, tl.toVector3f()).setUv(0.0f, 0.0f);
					buffer.addVertex(pose, bl.toVector3f()).setUv(0.0f, 1.0f);
					buffer.addVertex(pose, br.toVector3f()).setUv(1.0f, 1.0f);
					buffer.addVertex(pose, tr.toVector3f()).setUv(1.0f, 0.0f);
				}
				else {
					buffer.addVertex(pose, tl.toVector3f()).setUv(0.0f, 1.0f);
					buffer.addVertex(pose, bl.toVector3f()).setUv(0.0f, 0.0f);
					buffer.addVertex(pose, br.toVector3f()).setUv(1.0f, 0.0f);
					buffer.addVertex(pose, tr.toVector3f()).setUv(1.0f, 1.0f);
				}
			}
			else {
				if(!flipV) {
					buffer.addVertex(pose, tr.toVector3f()).setUv(1.0f, 0.0f);
					buffer.addVertex(pose, br.toVector3f()).setUv(1.0f, 1.0f);
					buffer.addVertex(pose, bl.toVector3f()).setUv(0.0f, 1.0f);
					buffer.addVertex(pose, tl.toVector3f()).setUv(0.0f, 0.0f);
				}
				else {
					buffer.addVertex(pose, tr.toVector3f()).setUv(1.0f, 1.0f);
					buffer.addVertex(pose, br.toVector3f()).setUv(1.0f, 0.0f);
					buffer.addVertex(pose, bl.toVector3f()).setUv(0.0f, 0.0f);
					buffer.addVertex(pose, tl.toVector3f()).setUv(0.0f, 1.0f);
				}
			}
		}
		
	}
	
	/**
	 * 统一 2D 纹理渲染入口 — 本地帧缓冲与远程共享纹理共用 WINDOW_BLIT 管线
	 * 
	 * flipV=false: 本地 framebuffer（bottom-up）
	 * flipV=true:  远程共享纹理（top-down，需翻转 V）
	 */
	public static void renderTexture2D(GuiGraphicsExtractor context, Identifier textureLocation, int x, int y, int w, int h, boolean flipV) {
		if(textureLocation == null) return;
		// Iris 兼容：2D GUI 渲染改用原版 GUI_TEXTURED 管线（Iris 认识）
		RenderPipeline pipeline = IrisCompat.isIrisLoaded() ? RenderPipelines.GUI_TEXTURED : WINDOW_BLIT;
		if(!flipV) {
			((IGuiGraphicsExtractor) context).invokeInnerBlit(pipeline, textureLocation, x, x + w, y, y + h, 0.0f, 1.0f, 0.0f, 1.0f, -1);
		}
		else {
			((IGuiGraphicsExtractor) context).invokeInnerBlit(pipeline, textureLocation, x, x + w, y, y + h, 0.0f, 1.0f, 1.0f, 0.0f, -1);
		}
	}
	
	public static void renderFramebuffer2D(GuiGraphicsExtractor context, WindowFramebuffer framebuffer, int x, int y, int w, int h) {
		if(!framebuffer.isValid()) return;
		renderTexture2D(context, framebuffer.getTextureLocation(), x, y, w, h, false);
	}
	
}
