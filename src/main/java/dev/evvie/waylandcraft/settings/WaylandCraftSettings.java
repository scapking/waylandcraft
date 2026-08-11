package dev.evvie.waylandcraft.settings;

import java.lang.reflect.Field;

import org.jetbrains.annotations.Nullable;

import dev.evvie.waylandcraft.WaylandCraftCommon;

/* Settings for waylandcraft
 * 
 * All of the fields here that do not have the "transient" modifier are written to the settings.json!
 * Because of that and because these fields are also accessed using reflection, just don't change their names,
 * unless you also update all the references and are okay with user settings breaking across updates.
 */
public class WaylandCraftSettings {
	
	/* The settings fields shouldn't have a modifier so that they're package-private.
	 * This avoids code directly setting the values.
	 */
	
	int pixelsPerBlock = 500;
	boolean windowAntialiasing = false;
	boolean focusOnHover = false;
	
	/* ---- 控制距离 / 游戏优化 / 圆形布局 ---- */
	/** 控制窗口时隐藏虚拟鼠标光标（沉浸游玩） */
	boolean hideCursor = true;
	/** 圆形自动布局默认开启 */
	boolean layoutEnabled = true;
	/** 新窗口自动加入圆形布局（false 时只排 /wl layout add 手动指定的窗口） */
	boolean layoutAutoJoin = true;
	/** 环形半径（格） */
	double layoutRadius = 6.0;
	/** 同层窗口最小间距（格） */
	double layoutSpacing = 0.5;
	/** 层间垂直间距（格，一圈排满后向上堆） */
	double layoutStackSpacing = 1.0;
	/** Ctrl+方向键平移步长（格/次） */
	double moveStep = 0.5;
	
	/* This is where the field names go to avoid typos */
	public static final String PIXELS_PER_BLOCK = "pixelsPerBlock";
	public static final String WINDOW_ANTIALIASING = "windowAntialiasing";
	public static final String FOCUS_ON_HOVER = "focusOnHover";
	public static final String HIDE_CURSOR = "hideCursor";
	public static final String LAYOUT_ENABLED = "layoutEnabled";
	public static final String LAYOUT_AUTO_JOIN = "layoutAutoJoin";
	public static final String LAYOUT_RADIUS = "layoutRadius";
	public static final String LAYOUT_SPACING = "layoutSpacing";
	public static final String LAYOUT_STACK_SPACING = "layoutStackSpacing";
	public static final String MOVE_STEP = "moveStep";
	
	/* This is where the getters go */
	
	public int getPixelsPerBlock() {
		return pixelsPerBlock;
	}
	
	public boolean getAntialiasing() {
		return windowAntialiasing;
	}
	
	public boolean getFocusOnHover() {
		return focusOnHover;
	}
	
	public boolean getHideCursor() {
		return hideCursor;
	}
	
	public boolean getLayoutEnabled() {
		return layoutEnabled;
	}
	
	public boolean getLayoutAutoJoin() {
		return layoutAutoJoin;
	}
	
	public double getLayoutRadius() {
		return layoutRadius;
	}
	
	public double getLayoutSpacing() {
		return layoutSpacing;
	}
	
	public double getLayoutStackSpacing() {
		return layoutStackSpacing;
	}
	
	public double getMoveStep() {
		return moveStep;
	}
	
	/* Methods to modifiy settings by name */
	
	protected void setIntSetting(String name, int value) {
		try {
			Field field = WaylandCraftSettings.class.getDeclaredField(name);
			field.setInt(this, value);
		} catch (NoSuchFieldException | IllegalArgumentException | IllegalAccessException e) {
			WaylandCraftCommon.LOGGER.error("Invalid setting accessed: '" + name + "' as int!");
			e.printStackTrace();
		}
	}
	
	protected void setBooleanSetting(String name, boolean value) {
		try {
			Field field = WaylandCraftSettings.class.getDeclaredField(name);
			field.setBoolean(this, value);
		} catch (NoSuchFieldException | IllegalArgumentException | IllegalAccessException e) {
			WaylandCraftCommon.LOGGER.error("Invalid setting accessed: '" + name + "' as boolean!");
			e.printStackTrace();
		}
	}
	
	protected void setDoubleSetting(String name, double value) {
		try {
			Field field = WaylandCraftSettings.class.getDeclaredField(name);
			field.setDouble(this, value);
		} catch (NoSuchFieldException | IllegalArgumentException | IllegalAccessException e) {
			WaylandCraftCommon.LOGGER.error("Invalid setting accessed: '" + name + "' as double!");
			e.printStackTrace();
		}
	}
	
	// Get int setting. Returns null only when setting was not found.
	protected @Nullable Integer getIntSetting(String name) {
		try {
			Field field = WaylandCraftSettings.class.getDeclaredField(name);
			return field.getInt(this);
		} catch (NoSuchFieldException | IllegalArgumentException | IllegalAccessException e) {
			WaylandCraftCommon.LOGGER.error("Invalid setting accessed: '" + name + "' as int!");
			e.printStackTrace();
		}
		return null;
	}
	
	// Get boolean setting. Returns null only when setting was not found.
	protected @Nullable Boolean getBooleanSetting(String name) {
		try {
			Field field = WaylandCraftSettings.class.getDeclaredField(name);
			return field.getBoolean(this);
		} catch (NoSuchFieldException | IllegalArgumentException | IllegalAccessException e) {
			WaylandCraftCommon.LOGGER.error("Invalid setting accessed: '" + name + "' as boolean!");
			e.printStackTrace();
		}
		return null;
	}
	
	// Get double setting. Returns null only when setting was not found.
	protected @Nullable Double getDoubleSetting(String name) {
		try {
			Field field = WaylandCraftSettings.class.getDeclaredField(name);
			return field.getDouble(this);
		} catch (NoSuchFieldException | IllegalArgumentException | IllegalAccessException e) {
			WaylandCraftCommon.LOGGER.error("Invalid setting accessed: '" + name + "' as double!");
			e.printStackTrace();
		}
		return null;
	}
	
}
