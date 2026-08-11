package dev.evvie.waylandcraft.network;

import java.util.ArrayList;
import java.util.List;
import java.util.UUID;

import dev.evvie.waylandcraft.WaylandCraftCommon;
import dev.evvie.waylandcraft.shared.SharedWindowEntry;
import dev.evvie.waylandcraft.shared.SharedWindowManager;
import dev.evvie.waylandcraft.shared.WindowPermission;
import dev.evvie.waylandcraft.utils.IMyServerPlayer;
import net.fabricmc.fabric.api.networking.v1.PayloadTypeRegistry;
import net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking;
import net.minecraft.server.level.ServerPlayer;

public class WaylandCraftNetworking {
	
	/** 最新帧缓存 + tick 批量转发器（与 receiver 在同一初始化流程中注册） */
	private static final SharedWindowFrameRelay frameRelay = new SharedWindowFrameRelay();
	
	/** 共享窗口音频转发器（队列 + 尽力而为，积压丢旧保新） */
	private static final AudioFrameRelay audioRelay = new AudioFrameRelay();
	
	public static void register() {
		PayloadTypeRegistry.serverboundPlay().register(ServerboundGiveItemsPayload.TYPE, ServerboundGiveItemsPayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(ServerboundAliveWindowsPayload.TYPE, ServerboundAliveWindowsPayload.CODEC);
		
		// 注册多人显示功能的数据包
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowRegisterPayload.TYPE, SharedWindowRegisterPayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowUnregisterPayload.TYPE, SharedWindowUnregisterPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowUpdatePayload.TYPE, SharedWindowUpdatePayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowImagePayload.TYPE, SharedWindowImagePayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowImagePayload.TYPE, SharedWindowImagePayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowInteractionPayload.TYPE, SharedWindowInteractionPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowPermissionPayload.TYPE, SharedWindowPermissionPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowListPayload.TYPE, SharedWindowListPayload.CODEC);
		PayloadTypeRegistry.serverboundPlay().register(SharedWindowAudioPayload.TYPE, SharedWindowAudioPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(SharedWindowAudioPayload.TYPE, SharedWindowAudioPayload.CODEC);
		
		// 权限管理命令
		PayloadTypeRegistry.serverboundPlay().register(PermissionCommandPayload.TYPE, PermissionCommandPayload.CODEC);
		PayloadTypeRegistry.clientboundPlay().register(PermissionResponsePayload.TYPE, PermissionResponsePayload.CODEC);
		
		ServerPlayNetworking.registerGlobalReceiver(ServerboundGiveItemsPayload.TYPE, (payload, ctx) -> {
			IMyServerPlayer plr = (IMyServerPlayer) ctx.player();
			if(plr.getItemGiveCooldown() > 0) return;
			plr.setItemGiveCooldown(10);
			
			ArrayList<Long> handles = new ArrayList<Long>();
			for(long handle : payload.handles()) {
				if(handles.contains(handle)) continue;
				handles.add(handle);
			}
			
			if(payload.missingOnly()) WaylandCraftCommon.instance.serverItemManager.giveItemsIfMissing(ctx.player(), handles);
			else WaylandCraftCommon.instance.serverItemManager.giveItems(ctx.player(), handles);
		});
		
		ServerPlayNetworking.registerGlobalReceiver(ServerboundAliveWindowsPayload.TYPE, (payload, ctx) -> {
			IMyServerPlayer plr = (IMyServerPlayer) ctx.player();
			ArrayList<Long> handles = plr.getAliveWindows();
			handles.clear();
			
			for(long handle : payload.handles()) {
				handles.add(handle);
			}
		});
		
		// 处理客户端请求注册共享窗口
		// v0.2.31：注册会触发窗口列表广播（遍历所有玩家 + send），切到服务端主线程执行，
		// 避免占 netty 线程；主线程上读玩家列表也绝对安全（无 CME）。
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowRegisterPayload.TYPE, (payload, ctx) -> {
			ctx.server().execute(() -> SharedWindowServerHandler.handleWindowRegister(payload, ctx.player()));
		});
		
		// 处理客户端请求注销共享窗口
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowUnregisterPayload.TYPE, (payload, ctx) -> {
			ctx.server().execute(() -> SharedWindowServerHandler.handleWindowUnregister(payload.windowHandle(), ctx.player()));
		});
		
		// 处理权限管理命令
		ServerPlayNetworking.registerGlobalReceiver(PermissionCommandPayload.TYPE, (payload, ctx) -> {
			ctx.server().execute(() -> {
				SharedWindowServerHandler.handlePermissionCommand(payload, ctx.player());
			});
		});
		
		// 处理客户端上传的窗口图像 - 只做权限校验并入最新帧缓存；
		// 实际转发由服务端 tick（SharedWindowFrameRelay，每 2 tick 批量转发）完成，
		// netty 线程内绝不调用 ServerPlayNetworking.send，避免堵死发送者连接。
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowImagePayload.TYPE, (payload, ctx) -> {
			ServerPlayer sender = ctx.player();
			UUID senderUUID = sender.getUUID();
			
			// 检查窗口是否由该玩家共享
			SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
			SharedWindowEntry entry = manager.getWindow(payload.windowHandle());
			if(entry == null || !entry.getOwnerUUID().equals(senderUUID)) {
				WaylandCraftCommon.LOGGER.warn("[SERVER] image rejected: entry={}, owner match={}", 
					entry != null, entry != null && entry.getOwnerUUID().equals(senderUUID));
				return;
			}
			
			// 校验通过：只把最新帧存入缓存（按 windowHandle 覆盖旧帧 = 丢中间帧）
			frameRelay.acceptFrame(payload);
		});
		
		// tick 转发注册与 receiver 注册在同一初始化流程
		frameRelay.register();
		
		// 共享窗口音频：netty 线程只入队，tick 批量转发
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowAudioPayload.TYPE, (payload, ctx) -> {
			ServerPlayer sender = ctx.player();
			UUID senderUUID = sender.getUUID();
			
			SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
			SharedWindowEntry entry = manager.getWindow(payload.windowHandle());
			if(entry == null || !entry.getOwnerUUID().equals(senderUUID)) {
				return;
			}
			
			audioRelay.acceptAudio(payload);
		});
		audioRelay.register();
		
		ServerPlayNetworking.registerGlobalReceiver(SharedWindowInteractionPayload.TYPE, (payload, ctx) -> {
			ServerPlayer player = ctx.player();
			UUID playerUUID = player.getUUID();
			
			SharedWindowManager manager = WaylandCraftCommon.instance.sharedWindowManager;
			
			// 检查权限
			if (!manager.canInteract(playerUUID, payload.windowHandle())) {
				return;
			}
			
			// 转发交互给窗口所有者
			// 转发交互给窗口所有者
			InteractionForwarder.forwardInteraction(payload, player);
		});
	}
	
	private static void broadcastWindowList(SharedWindowManager manager, ServerPlayer excludePlayer) {
		// 发送给所有在线玩家（排除发送者）
		for (ServerPlayer player : excludePlayer.level().getServer().getPlayerList().getPlayers()) {
			if (player == excludePlayer) continue;
			
			// 每个接收者用自己的权限构建窗口列表
			List<SharedWindowListPayload.WindowInfo> windowList = new ArrayList<>();
			for (SharedWindowEntry entry : manager.getAllWindows()) {
				windowList.add(new SharedWindowListPayload.WindowInfo(
					entry.getWindowHandle(),
					entry.getOwnerUUID(),
					entry.getWindowTitle(),
					entry.getPermission(player.getUUID())
				));
			}
			
			ServerPlayNetworking.send(player, new SharedWindowListPayload(windowList));
		}
	}
	
}
