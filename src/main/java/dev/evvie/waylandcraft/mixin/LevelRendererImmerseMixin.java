package dev.evvie.waylandcraft.mixin;

import com.mojang.blaze3d.textures.GpuSampler;

import dev.evvie.waylandcraft.immerse.ImmerseManager;
import net.minecraft.client.Camera;
import net.minecraft.client.DeltaTracker;
import net.minecraft.client.renderer.chunk.ChunkSectionLayerGroup;
import net.minecraft.client.renderer.chunk.ChunkSectionsToRender;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
import com.llamalad7.mixinextras.injector.wrapoperation.Operation;
import com.llamalad7.mixinextras.injector.wrapoperation.WrapOperation;

/**
 * ⑩ v1.5.0 沉浸模式：跳过世界内容渲染（"没有任何内容，只有特效空间"）。
 *
 * 勘查结论（javap LevelRenderer + fabric-rendering-v1 LevelRendererMixin）：
 * - **不可 cancel renderLevel 整体**：fabric 的 COLLECT_SUBMITS（窗口/穹顶提交事件）
 *   经 beforeRender 挂在 renderLevel 内部——cancel 会连窗口一起杀掉；
 * - 26.1.2 渲染两阶段：extractLevel（实体/blockentity/特效 render state 提取）→
 *   renderLevel（提交绘制）。地形不经 extractLevel（SectionRenderDispatcher 独立），
 *   绘制入口是 ChunkSectionsToRender.renderGroup（renderLevel 字节码 2 个调用点：
 *   不透明组 + 半透明组，javap -c 实证）。
 *
 * 因此双短路：
 * 1) extractLevel HEAD cancel → 实体/方块实体/特效不再提取（不绘制，性能顺带省）；
 * 2) WrapOperation 包住 renderGroup 调用 → 沉浸时不调原方法（地形全消失；
 *    一次 wrap 覆盖两个调用点）。
 *
 * 保留：vanilla 天空盒/云（穹顶立方壳深度覆盖；云若穿壳属小瑕疵，实测调）、
 * 第一人称手（GameRenderer 层，不在本 mixin 范围）。
 * 关闭时零开销：一次 volatile 读。
 */
@Mixin(net.minecraft.client.renderer.LevelRenderer.class)
public class LevelRendererImmerseMixin {
	
	@Inject(method = "extractLevel", at = @At("HEAD"), cancellable = true)
	private void wc$immerseSkipExtract(DeltaTracker deltaTracker, Camera camera, float partialTick,
			CallbackInfo ci) {
		if(ImmerseManager.isVisualActive()) {
			ci.cancel();
		}
	}
	
	@WrapOperation(method = "renderLevel", at = @At(value = "INVOKE",
			target = "Lnet/minecraft/client/renderer/chunk/ChunkSectionsToRender;renderGroup(Lnet/minecraft/client/renderer/chunk/ChunkSectionLayerGroup;Lcom/mojang/blaze3d/textures/GpuSampler;)V"))
	private void wc$immerseSkipTerrain(ChunkSectionsToRender instance, ChunkSectionLayerGroup group,
			GpuSampler sampler, Operation<Void> original) {
		// 沉浸激活时不调原地形绘制（renderGroup 的不透明/半透明两个调用点都被本 wrap 覆盖）
		if(!ImmerseManager.isVisualActive()) {
			original.call(instance, group, sampler);
		}
	}
}
