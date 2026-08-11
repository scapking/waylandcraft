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
	
	/* ---- 控制距离 / 游戏优化 / 布局 ---- */
	/** 控制窗口时隐藏虚拟鼠标光标（默认不自动隐藏，按键切换） */
	boolean hideCursor = false;
	/** 布局默认开启（回到 v0.2.37 行为；未初始化坐标时自动用玩家位置初始化） */
	boolean layoutEnabled = true;
	/** 新窗口自动加入布局（false 时只排 /wl layout add 手动指定的窗口） */
	boolean layoutAutoJoin = true;
	/** 布局模板：cube（方块）或 sphere（圆球） */
	String layoutTemplate = "cube";
	/** 是否已通过 /wl layout init 初始化坐标（未初始化布局不可用） */
	boolean layoutInitialized = false;
	/** 初始化坐标（布局中心） */
	double layoutInitX = 0.0;
	double layoutInitY = 0.0;
	double layoutInitZ = 0.0;
	/** 初始化朝向（度，0=朝+Z，顺时针） */
	double layoutInitYaw = 0.0;
	/** 布局半径（格，窗口中心距初始化中心的水平距离） */
	double layoutRadius = 6.0;
	/** 同层窗口最小间距（格，左右） */
	double layoutSpacing = 0.4;
	/** 层间垂直间距（格，上下） */
	double layoutStackSpacing = 0.4;
	/** 方块模板每面窗口数（默认一面 2 个，四面 8 个） */
	int layoutCubePerFace = 2;
	/** 新加入布局的窗口自动调整到的分辨率 */
	int layoutDefaultWidth = 1080;
	int layoutDefaultHeight = 540;
	/** 窗口底部距地面最小净空（格） */
	double groundClearance = 0.4;
	/** Ctrl+方向键在布局中切换核心窗口（0上 1下 2左 3右 由按键决定，见代码） */
	double moveStep = 0.5;
	
	/* This is where the field names go to avoid typos */
	public static final String PIXELS_PER_BLOCK = "pixelsPerBlock";
	public static final String WINDOW_ANTIALIASING = "windowAntialiasing";
	public static final String FOCUS_ON_HOVER = "focusOnHover";
	public static final String HIDE_CURSOR = "hideCursor";
	public static final String LAYOUT_ENABLED = "layoutEnabled";
	public static final String LAYOUT_AUTO_JOIN = "layoutAutoJoin";
	public static final String LAYOUT_TEMPLATE = "layoutTemplate";
	public static final String LAYOUT_INITIALIZED = "layoutInitialized";
	public static final String LAYOUT_INIT_X = "layoutInitX";
	public static final String LAYOUT_INIT_Y = "layoutInitY";
	public static final String LAYOUT_INIT_Z = "layoutInitZ";
	public static final String LAYOUT_INIT_YAW = "layoutInitYaw";
	public static final String LAYOUT_RADIUS = "layoutRadius";
	public static final String LAYOUT_SPACING = "layoutSpacing";
	public static final String LAYOUT_STACK_SPACING = "layoutStackSpacing";
	public static final String LAYOUT_CUBE_PER_FACE = "layoutCubePerFace";
	public static final String LAYOUT_DEFAULT_WIDTH = "layoutDefaultWidth";
	public static final String LAYOUT_DEFAULT_HEIGHT = "layoutDefaultHeight";
	public static final String GROUND_CLEARANCE = "groundClearance";
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
	
	public String getLayoutTemplate() {
		return layoutTemplate;
	}
	
	public boolean getLayoutInitialized() {
		return layoutInitialized;
	}
	
	public double getLayoutInitX() {
		return layoutInitX;
	}
	
	public double getLayoutInitY() {
		return layoutInitY;
	}
	
	public double getLayoutInitZ() {
		return layoutInitZ;
	}
	
	public double getLayoutInitYaw() {
		return layoutInitYaw;
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
	
	public int getLayoutCubePerFace() {
		return layoutCubePerFace;
	}
	
	public int getLayoutDefaultWidth() {
		return layoutDefaultWidth;
	}
	
	public int getLayoutDefaultHeight() {
		return layoutDefaultHeight;
	}
	
	public double getGroundClearance() {
		return groundClearance;
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
	
	protected void setStringSetting(String name, String value) {
		try {
			Field field = WaylandCraftSettings.class.getDeclaredField(name);
			field.set(this, value);
		} catch (NoSuchFieldException | IllegalArgumentException | IllegalAccessException e) {
			WaylandCraftCommon.LOGGER.error("Invalid setting accessed: '" + name + "' as string!");
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
	
	// Get string setting. Returns null only when setting was not found.
	protected @Nullable String getStringSetting(String name) {
		try {
			Field field = WaylandCraftSettings.class.getDeclaredField(name);
			return (String) field.get(this);
		} catch (NoSuchFieldException | IllegalArgumentException | IllegalAccessException e) {
			WaylandCraftCommon.LOGGER.error("Invalid setting accessed: '" + name + "' as string!");
			e.printStackTrace();
		}
		return null;
	}
	
}
