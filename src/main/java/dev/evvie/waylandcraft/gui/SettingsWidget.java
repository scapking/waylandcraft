package dev.evvie.waylandcraft.gui;

import org.jetbrains.annotations.Nullable;
import org.lwjgl.glfw.GLFW;

import dev.evvie.waylandcraft.WaylandCraft;
import dev.evvie.waylandcraft.gui.theme.PanelRenderer;
import dev.evvie.waylandcraft.gui.theme.WcColors;
import dev.evvie.waylandcraft.gui.theme.WcTheme;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.Font;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.gui.components.AbstractWidget;
import net.minecraft.client.gui.components.EditBox;
import net.minecraft.client.gui.narration.NarrationElementOutput;
import net.minecraft.client.input.CharacterEvent;
import net.minecraft.client.input.KeyEvent;
import net.minecraft.client.input.MouseButtonEvent;
import net.minecraft.client.input.MouseButtonInfo;
import net.minecraft.client.resources.sounds.SimpleSoundInstance;
import net.minecraft.network.chat.Component;
import net.minecraft.sounds.SoundEvents;
import net.minecraft.util.ARGB;

/**
 * 设置行组件 —— 统一科幻风格。
 * 行：圆角内嵌底 + 左标题右控件；控件：霓虹开关 / 霓虹数值框。
 */
public class SettingsWidget extends AbstractWidget {

	// Standard widget width, height
	public static final int WIDTH = 300;
	public static final int HEIGHT = 30;

	// Width of the interactable element in the widget
	private static final int ELEMENT_WIDTH = 100;

	public final ControlElement control;
	protected WaylandCraft wlc;

	private SettingsWidget(WaylandCraft instance, ControlElement control, Component message) {
		super(0, 0, WIDTH, HEIGHT, message);
		this.control = control;
		this.wlc = instance;
	}

	public static SettingsWidget createBooleanWidget(WaylandCraft instance, String settingName, Component message) {
		BooleanControlElement control = new BooleanControlElement(instance, settingName);
		return new SettingsWidget(instance, control, message);
	}

	public static SettingsWidget createIntWidget(WaylandCraft instance, String settingName, Component message) {
		IntControlElement control = new IntControlElement(instance, settingName);
		return new SettingsWidget(instance, control, message);
	}

	// upstream 26350c4: 文本设置行（EditBox 控件）
	public static SettingsWidget createTextWidget(WaylandCraft instance, String settingName, Component message) {
		TextControlElement control = new TextControlElement(instance, settingName);
		return new SettingsWidget(instance, control, message);
	}

	@Override
	protected void extractWidgetRenderState(GuiGraphicsExtractor graphics, int mouseX, int mouseY, float a) {
		Font font = Minecraft.getInstance().font;

		int x = getX();
		int y = getY();
		int width = getWidth();
		int height = getHeight();
		int totalElementWidth = ELEMENT_WIDTH;
		int textPad = (height - font.lineHeight) / 2;

		// 行底（内嵌圆角面板）
		PanelRenderer.field(graphics, x, y, width, height);

		if(isFocused() || isHoveredOrFocused()) {
			PanelRenderer.neonBorder(graphics, x, y, width, height);
		}

		graphics.enableScissor(x, y, x + width - totalElementWidth - textPad, y + height);
		graphics.text(font, message, x + textPad, y + textPad,
				active ? WcColors.TEXT : WcColors.TEXT_DISABLED, active);
		graphics.disableScissor();

		int elemPad = 5;

		int elemX = x + width - totalElementWidth + elemPad;
		int elemY = y + elemPad;
		int elemWidth = totalElementWidth - elemPad * 2;
		int elemHeight = height - elemPad * 2;

		control.setPosSize(elemX, elemY, elemWidth, elemHeight);
		control.setFocused(this.isFocused());
		control.extractControlElement(graphics, mouseX, mouseY, a);
	}

	// upstream 26350c4: 焦点丢失 / 屏幕关闭时提交控件的挂起值
	public void saveValue() {
		control.saveValue();
	}

	@Override
	public boolean mouseClicked(MouseButtonEvent event, boolean doubleClick) {
		if(!this.isActive()) return false;
		if(!isMouseOver(event.x(), event.y())) return false;

		if(!doubleClick) control.onClick((int) event.x(), (int) event.y(), event.buttonInfo());
		else control.onDoubleClick();
		return true;
	}

	@Override
	public boolean keyPressed(KeyEvent event) {
		if(!this.isActive()) return false;
		return control.onKeyPressed(event);
	}

	@Override
	public boolean keyReleased(KeyEvent event) {
		if(!this.isActive()) return false;
		return control.onKeyReleased(event);
	}

	@Override
	public boolean charTyped(CharacterEvent event) {
		if(!this.isActive()) return false;
		return control.onCharTyped(event);
	}

	@Override
	protected void updateWidgetNarration(NarrationElementOutput output) {
	}

	public abstract static class ControlElement {

		public final String settingName;
		protected WaylandCraft wlc;

		private int x;
		private int y;
		private int width;
		private int height;
		private boolean focused;

		public ControlElement(WaylandCraft wlc, String settingName) {
			this.settingName = settingName;
			this.wlc = wlc;
		}

		public void setPosSize(int x, int y, int width, int height) {
			this.x = x;
			this.y = y;
			this.width = width;
			this.height = height;
		}

		public void setFocused(boolean focused) {
			this.focused = focused;
		}

		public int getX() {
			return x;
		}

		public int getY() {
			return y;
		}

		public int getWidth() {
			return width;
		}

		public int getHeight() {
			return height;
		}

		public boolean isFocused() {
			return focused;
		}

		public boolean isInside(int testX, int testY) {
			return x <= testX && testX <= x + width && y <= testY && testY <= y + height;
		}

		public abstract void extractControlElement(GuiGraphicsExtractor graphics, int mouseX, int mouseY, float a);

		// upstream 26350c4: 提交挂起的编辑值。
		// Boolean 控件点击即写、Int 控件失焦即写，默认空实现保持 fork 现有行为不变；
		// TextControlElement 覆写此方法。
		public void saveValue() {}

		public void onClick(int mouseX, int mouseY) {}
		// upstream 26350c4: 带按键信息的点击。默认转发到旧 2 参签名，
		// 现有 Boolean/Int 控件行为不变；TextControlElement 覆写本方法。
		public void onClick(int mouseX, int mouseY, MouseButtonInfo buttonInfo) {
			onClick(mouseX, mouseY);
		}
		public void onDoubleClick() {}
		public void onDrag() {}

		public void doClickSound() {
			Minecraft.getInstance().getSoundManager().play(SimpleSoundInstance.forUI(SoundEvents.UI_BUTTON_CLICK, 1.0F));
		}

		public boolean onKeyPressed(KeyEvent event) {
			return false;
		}

		public boolean onKeyReleased(KeyEvent event) {
			return false;
		}

		// upstream 26350c4: 文本输入事件
		public boolean onCharTyped(CharacterEvent event) {
			return false;
		}

	}

	/** 霓虹开关控件（ON=青底滑块右 / OFF=暗底滑块左） */
	public static class BooleanControlElement extends ControlElement {

		public BooleanControlElement(WaylandCraft wlc, String settingName) {
			super(wlc, settingName);
		}

		private boolean getValue() {
			return wlc.settingsManager.getBooleanSetting(settingName);
		}

		private void setValue(boolean value) {
			wlc.settingsManager.setBooleanSetting(settingName, value);
		}

		private void toggle() {
			setValue(!getValue());
			doClickSound();
		}

		@Override
		public void extractControlElement(GuiGraphicsExtractor graphics, int mouseX, int mouseY, float a) {
			int x = getX();
			int y = getY();
			int width = getWidth();
			int height = getHeight();
			boolean on = getValue();

			Font font = Minecraft.getInstance().font;

			if(on) {
				PanelRenderer.neonFilled(graphics, x, y, width, height);
			}
			else {
				PanelRenderer.field(graphics, x, y, width, height);
			}

			// 滑块
			int pad = 3;
			int knob = height - pad * 2;
			int knobX = on ? x + width - knob - pad : x + pad;
			graphics.fill(knobX, y + pad, knobX + knob, y + pad + knob, on ? WcColors.TEXT_ON_NEON : WcColors.TEXT_DIM);

			Component text = on ? Component.literal("ON") : Component.literal("OFF");
			int tx = on ? x + pad + 1 : x + width - font.width(text) - pad - 1;
			graphics.text(font, text, tx, y + height / 2 - font.lineHeight / 2,
					on ? WcColors.TEXT_ON_NEON : WcColors.TEXT_DIM);

			if(isFocused()) PanelRenderer.neonBorder(graphics, x, y, width, height);
		}

		@Override
		public void onClick(int mouseX, int mouseY) {
			if(isInside(mouseX, mouseY)) toggle();
		}

		@Override
		public boolean onKeyPressed(KeyEvent event) {
			if(event.isSelection()) {
				toggle();
				return true;
			}
			return false;
		}

	}

	/** 霓虹数值框控件 */
	public static class IntControlElement extends ControlElement {

		private @Nullable String entry = null;

		public IntControlElement(WaylandCraft wlc, String settingName) {
			super(wlc, settingName);
		}

		private int getValue() {
			return wlc.settingsManager.getIntSetting(settingName);
		}

		private void setValue(int value) {
			wlc.settingsManager.setIntSetting(settingName, value);
		}

		@Override
		public void extractControlElement(GuiGraphicsExtractor graphics, int mouseX, int mouseY, float a) {
			int x = getX();
			int y = getY();
			int width = getWidth();
			int height = getHeight();

			Font font = Minecraft.getInstance().font;

			String str;
			if(entry == null) str = "" + getValue();
			else str = entry + "_";

			Component text = Component.literal(str);

			PanelRenderer.field(graphics, x, y, width, height);

			int textWidth = font.width(text);
			int textHeight = font.lineHeight;
			int textX = x + width / 2 - textWidth / 2;
			int textY = y + height / 2 - textHeight / 2;

			graphics.text(font, text, textX, textY, isFocused() ? WcColors.CYAN : WcColors.TEXT);
			if(isFocused()) PanelRenderer.neonBorder(graphics, x, y, width, height);
		}

		private void stopEntry() {
			if(entry == null) return;
			if(entry.length() == 0) return;
			setValue(Integer.parseInt(entry));
			entry = null;
		}

		// upstream 26350c4: 外部提交通道复用既有的失焦保存逻辑，行为不变
		@Override
		public void saveValue() {
			stopEntry();
		}

		@Override
		public void setFocused(boolean focused) {
			if(!focused && isFocused()) {
				// Focus lost
				stopEntry();
			}

			super.setFocused(focused);
		}

		@Override
		public void onDoubleClick() {
			if(entry == null) {
				entry = "" + getValue();
			}
		}

		@Override
		public boolean onKeyPressed(KeyEvent event) {
			boolean isEnter = event.key() == GLFW.GLFW_KEY_ENTER;
			boolean isBackspace = event.key() == GLFW.GLFW_KEY_BACKSPACE;

			if(isEnter && entry != null) {
				stopEntry();
				return true;
			}
			if((isBackspace || isEnter) && entry == null) {
				entry = "";
				return true;
			}
			if(isBackspace && entry != null) {
				if(entry.length() > 0) entry = entry.substring(0, entry.length() - 1);
				return true;
			}

			int digit = event.getDigit();
			if(digit != -1) {
				if(entry != null) entry += digit;
				else entry = "" + digit;
				return true;
			}
			return false;
		}

	}

	/** upstream 26350c4: EditBox 文本控件（如默认终端选择）。
	 *  持久化复用 fork 已有的 string 设置接口（setStringSetting/getStringSetting）。 */
	public static class TextControlElement extends ControlElement {

		private EditBox editBox;

		public TextControlElement(WaylandCraft wlc, String settingName) {
			super(wlc, settingName);

			editBox = new EditBox(Minecraft.getInstance().font, getWidth(), getHeight(), Component.literal(settingName));
			String saved = getSavedValue();
			if(saved != null) {
				editBox.insertText(saved);
			}
			editBox.moveCursorTo(0, false);
		}

		private @Nullable String getSavedValue() {
			return wlc.settingsManager.getStringSetting(settingName);
		}

		@Override
		public void setPosSize(int x, int y, int width, int height) {
			super.setPosSize(x, y, width, height);
			editBox.setRectangle(width, height, x, y);
		}

		@Override
		public void extractControlElement(GuiGraphicsExtractor graphics, int mouseX, int mouseY, float a) {
			editBox.extractRenderState(graphics, mouseX, mouseY, a);
		}

		@Override
		public void saveValue() {
			wlc.settingsManager.setStringSetting(settingName, editBox.getValue());
		}

		@Override
		public void onClick(int mouseX, int mouseY, MouseButtonInfo buttonInfo) {
			editBox.onClick(new MouseButtonEvent(mouseX, mouseY, buttonInfo), false);
		}

		@Override
		public void setFocused(boolean focused) {
			if(!focused && isFocused()) {
				// Focus lost
				saveValue();
			}

			editBox.setFocused(focused);
			super.setFocused(focused);
		}

		@Override
		public boolean onKeyPressed(KeyEvent event) {
			if(event.key() == GLFW.GLFW_KEY_ENTER) {
				saveValue();
				return true;
			}

			return editBox.keyPressed(event);
		}

		@Override
		public boolean onKeyReleased(KeyEvent event) {
			return editBox.keyReleased(event);
		}

		@Override
		public boolean onCharTyped(CharacterEvent event) {
			return editBox.charTyped(event);
		}

	}

}
