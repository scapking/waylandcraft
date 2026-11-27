package dev.evvie.waylandcraft.desktop;

import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;

import org.jetbrains.annotations.Nullable;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.WaylandCraftCommon;

public class XDGDesktopManager {
	
	private final WaylandCraft wlc;
	private ArrayList<DesktopEntry> systemEntries = null;
	private Thread systemEntryFetchThread;
	
	public XDGDesktopManager(WaylandCraft wlc) {
		this.wlc = wlc;
		systemEntryFetchThread = new Thread(this::loadSystemEntries);
		systemEntryFetchThread.setDaemon(true);
		systemEntryFetchThread.start();
	}
	
	private void loadSystemEntries() {
		/* Calling this in a separate thread is probably a huge crime but I'm desperate */

		try {
			Instant start = Instant.now();

			RawDesktopEntry[] rawEntries = wlc.bridge.loadSystemDesktopEntries();
			ArrayList<DesktopEntry> systemEntries = new ArrayList<DesktopEntry>();
			for(RawDesktopEntry raw : rawEntries) {
				systemEntries.add(new DesktopEntry(raw.appId, raw.name, raw.genericName, raw.exec, raw.execTerminal, raw.comment, raw.keywords, raw.categories, raw.visible, raw.iconPath));
			}
			this.systemEntries = systemEntries;

			WaylandCraftCommon.LOGGER.info("Completed desktop entry loading in " + Duration.between(start, Instant.now()).toMillis() / 1000.0f + "s");

			Thread iconPreloadThread = new Thread(this::preloadIcons);
			iconPreloadThread.setDaemon(true);
			iconPreloadThread.start();
		} catch(Throwable t) {
			// 后台线程兜底：加载失败（JNI/桥接异常等）不允许线程静默死亡；
			// 失败时 systemEntries 保持 null，entries()/forAppId() 按空列表处理。
			WaylandCraftCommon.LOGGER.warn("Desktop entry loading failed; app launcher entries unavailable", t);
		}
	}

	private void preloadIcons() {
		Instant start = Instant.now();

		try {
			for(DesktopEntry entry : systemEntries) {
				entry.preloadIcon();
			}
		} catch(Throwable t) {
			// 图标预载失败只影响图标显示，不影响条目本身；同样不允许静默死亡。
			WaylandCraftCommon.LOGGER.warn("Desktop icon preloading failed", t);
		}

		WaylandCraftCommon.LOGGER.info("Completed icon preloading in " + Duration.between(start, Instant.now()).toMillis() / 1000.0f + "s");
	}
	
	private boolean completeFetch() {
		boolean done = false;
		try {
			done = systemEntryFetchThread.join(Duration.ZERO);
		} catch(InterruptedException e) {
		}
		
		return done;
	}
	
	public List<DesktopEntry> entries() {
		if(!completeFetch() || systemEntries == null) {
			// systemEntries == null：后台加载线程已失败终止（见 loadSystemEntries 的兜底 catch），
			// 按空列表处理，避免对调用方（渲染线程）抛 NPE。
			return new ArrayList<DesktopEntry>();
		}
		
		ArrayList<DesktopEntry> entries = new ArrayList<DesktopEntry>();
		entries.addAll(systemEntries);
		return entries;
	}
	
	public @Nullable DesktopEntry forAppId(String appId) {
		if(appId == null) return null;
		if(!completeFetch() || systemEntries == null) {
			return null;
		}
		
		for(DesktopEntry entry : systemEntries) {
			if(entry.appId.equals(appId)) return entry;
		}
		return null;
	}
	
}
