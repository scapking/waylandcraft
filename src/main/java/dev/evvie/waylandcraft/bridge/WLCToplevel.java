package dev.evvie.waylandcraft.bridge;

import org.jetbrains.annotations.Nullable;

public class WLCToplevel extends WLCAbstractWindow {
	
	@Nullable
	public String title;
	
	@Nullable
	public String appID;
	
	/** 窗口所属客户端进程 PID（wayland SO_PEERCRED；0 = 未知/X11 窗口） */
	public int pid = 0;
	
	public ToplevelRequests requests = new ToplevelRequests();
	public boolean fullscreen = false;
	
	@Nullable
	public SurfaceGeometry restoreGeometry = null;
	
	public WLCToplevel(long handle) {
		super(handle);
	}
	
	public static class ToplevelRequests {
		
		public boolean minimize = false;
		public boolean maximize = false;
		public boolean unmaximize = false;
		public boolean fullscreen = false;
		public boolean unfullscreen = false;
		
	}
	
}
