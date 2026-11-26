package dev.evvie.waylandcraft.bridge;

import org.jetbrains.annotations.Nullable;

import dev.evvie.waylandcraft.render.WindowFramebuffer;

public abstract class WLCAbstractWindow {
	
	// Set to zero when this window no longer exists
	private long handle;
	
	@Nullable
	protected WLCSurface surface;
	
	@Nullable
	protected WLCSurface lastChild;
	
	protected boolean wasMapped = false;

	/** v1.4.4：updateSurfaceTree 异常的一次性告警位（恢复健康后重置）。 */
	boolean treeErrorWarned = false;
	
	public SurfaceGeometry geometry;
	
	@Nullable
	public WindowFramebuffer framebuffer = null;
	
	public WLCAbstractWindow(long handle) {
		this.handle = handle;
	}
	
	public long getHandle() {
		return this.handle;
	}
	
	protected long takeHandle() {
		long old = this.handle;
		this.handle = 0;
		return old;
	}
	
	public boolean isAlive() {
		return handle != 0;
	}
	
	public WLCSurface getSurfaceTree() {
		return this.surface;
	}
	
	public WLCSurface getSurfaceTreeLast() {
		return this.lastChild;
	}
	
	public boolean isMapped() {
		return isAlive() && getSurfaceTree().getBuffer() != null;
	}
	
	public static record SurfaceGeometry(int x, int y, int width, int height) {
	}
	
}
