# MC 26.1.2 (Fabric/Yarn) 文本输入组件 API 调研报告 — WaylandCraft IME 前端集成

> 调研日期：2026-08-28 ｜ 版本：minecraft 26.1.2 + yarn（loom 1.16-SNAPSHOT）
> 证据来源：本地 loom 反混淆 jar
> `/root/.gradle/caches/fabric-loom/minecraftMaven/net/minecraft/minecraft-merged-deobf/26.1.2/minecraft-merged-deobf-26.1.2.jar`
> （`javap -c -p` 反汇编验证，非猜测）
>
> 重要前置结论：**MC 26.x 已内建完整 IME 管线**（GLFW preedit 回调 → `PreeditEvent` →
> `Screen.preeditUpdated` → `EditBox`/`IMEPreeditOverlay`），WaylandCraft 的前端不必从零造轮子，
> 详见 §6 集成建议。

---

## 1. 文本输入组件类名（旧名 TextFieldWidget 已不存在）

| 场景 | 类（yarn 名） | 说明 |
|---|---|---|
| 聊天框 / 命令框 / 铁砧重命名 / 搜索框 / 大部分单行输入 | **`net.minecraft.client.gui.components.EditBox`** | 单行文本组件，继承 `AbstractWidget`。1.21 起由 `TextFieldWidget` 改名而来，26.1.2 中已**没有** `TextFieldWidget` 类 |
| 告示牌编辑 | **`AbstractSignEditScreen`**（屏幕级）+ **`net.minecraft.client.gui.font.TextFieldHelper`** | 告示牌**不用 EditBox**！`AbstractSignEditScreen.signField` 是 `TextFieldHelper`（底层编辑助手：光标/选区/剪贴板），按键在 Screen 自己处理 |
| 成书编辑 | **`net.minecraft.client.gui.components.MultiLineEditBox`**（继承 `AbstractTextAreaWidget`）| `BookEditScreen.page` |
| 多行文本框数据模型 | `net.minecraft.client.gui.components.MultilineTextField` | MultiLineEditBox 内部持有 |
| 命令补全候选列表 | `net.minecraft.client.gui.components.CommandSuggestions` + 内部类 `CommandSuggestions$SuggestionsList` | 游戏内候选窗的官方参考实现（§4.2 锚点算法直接抄它）|
| IME preedit 浮层 | **`net.minecraft.client.gui.components.IMEPreeditOverlay`** | 26.x 内建的 preedit 覆盖层（输入框下方的小文本框）|

关键实例字段：
- `ChatScreen.input` — `protected EditBox`（聊天框）
- `AbstractCommandBlockEditScreen.commandEdit` / `previousEdit` — `protected EditBox`
- `BookEditScreen.page` — `MultiLineEditBox`
- `AbstractSignEditScreen.signField` — `TextFieldHelper`

### 1.1 怎么拿到当前聚焦的文本组件

```java
// Minecraft.screen 在 26.1.2 是 public 字段
Screen screen = Minecraft.getInstance().screen;
GuiEventListener focused = screen.getFocused();          // 来自 AbstractContainerEventHandler，public
if (focused instanceof EditBox eb) { ... }
```

- `Screen extends AbstractContainerEventHandler implements Renderable`。
- `AbstractContainerEventHandler`：
  - `public GuiEventListener getFocused()`
  - `public void setFocused(GuiEventListener)`
- 焦点路径：`ContainerEventHandler.getCurrentFocusPath() : ComponentPath`（默认方法）。
  `net.minecraft.client.gui.ComponentPath` 接口：
  - `static ComponentPath leaf(GuiEventListener)`
  - `static ComponentPath path(ContainerEventHandler, ComponentPath...)`
  - `GuiEventListener component()`（叶子组件）
  - `void applyFocus(boolean)`
- 健壮写法（焦点可能套在容器里，如 `FocusableTextWidget`、筛选列表内嵌框）：

```java
ComponentPath path = screen.getCurrentFocusPath();
GuiEventListener leaf = path != null ? path.component() : screen.getFocused();
if (leaf instanceof EditBox eb) { /* 游戏内聊天/命令框 */ }
else if (leaf instanceof MultiLineEditBox mleb) { /* 成书 */ }
// 告示牌：focused 是 null（screen 自己处理输入），需单独识别 screen instanceof AbstractSignEditScreen
```

---

## 2. 设置文本 / 光标处插入 / 取光标位置（EditBox，方法签名实测）

```java
// —— 整框文本 ——
public void setValue(String value)              // 设整个文本（内部 onValueChange → responder）
public String getValue()                        // 取文本
public String getHighlighted()                  // 取选中的文本

// —— 光标处插入（IME commit 直写入口）——
public void insertText(String text)
// 内部逻辑（字节码确认）：替换 [min(cursorPos,highlightPos), max(...)] 选区，
// 先 StringUtil.filterText 过滤非法字符，再受 maxLength 约束，最后 onValueChange。

// —— 光标 / 选区 ——
public void setCursorPosition(int pos)
public int  getCursorPosition()
public void setHighlightPos(int pos)
public void moveCursor(int amount, boolean shift)
public void moveCursorTo(int pos, boolean shift)
public void moveCursorToStart(boolean shift)
public void moveCursorToEnd(boolean shift)
public int  getWordPosition(int dir)
public void deleteWords(int dir) / deleteChars(int dir) / deleteCharsToPos(int pos)
public void setSuggestion(String suggestion)     // 灰色补全后缀

// —— 可编辑状态 ——
public boolean canConsumeInput()   // = isActive() && isFocused() && isEditable()（字节码确认）
public void setEditable(boolean) / private boolean isEditable()
public void setMaxLength(int) / private int getMaxLength()
public void setResponder(Consumer<String> responder)   // 文本变化回调（聊天框用它刷 commandSuggestions）
public void addFormatter(EditBox.TextFormatter formatter) // 渲染格式化钩子
```

`MultiLineEditBox` / `MultilineTextField` 版：

```java
// MultiLineEditBox
public void setValue(String) / public String getValue()
public boolean preeditUpdated(PreeditEvent)      // 自带 IMEPreeditOverlay
// MultilineTextField（内部数据模型）
public void insertText(String) / public void setValue(String)
public int  cursor() / public int getLineAtCursor() / public int getLineCount()
public StringView getLineView(int) / public void setValueListener(Consumer<String>) / setCursorListener(Runnable)
```

`TextFieldHelper`（告示牌用）：

```java
public void insertText(String)
public int  getCursorPos() / public void setCursorPos(int) / setSelectionRange(int,int)
public void cut() / copy() / paste() / selectAll()
public boolean charTyped(CharacterEvent) / public boolean keyPressed(KeyEvent)
```

---

## 3. 键盘字符输入从哪里进 + mixin 注入点

### 3.1 事件分发链（26.x 全改为事件对象，不再是裸 int）

GLFW 回调统一注册在 `com.mojang.blaze3d.platform.InputConstants.setupKeyboardCallbacks(Window, GLFWKeyCallbackI, GLFWCharCallbackI, GLFWPreeditCallbackI, GLFWIMEStatusCallbackI)`（KeyboardHandler.setup 内，字节码确认）。

| 事件 | 入口（net.minecraft.client.KeyboardHandler） | 到组件 |
|---|---|---|
| 按键 | `keyPress(JILnet/minecraft/client/input/KeyEvent;)V`（private） | … → `Screen.keyPressed(KeyEvent)` → **`AbstractContainerEventHandler` 默认**：`getFocused().keyPressed(KeyEvent)` → `EditBox.keyPressed` |
| 字符 | `charTyped(JLnet/minecraft/client/input/CharacterEvent;)V`（private） | … → `Screen.charTyped(CharacterEvent)`（继承默认）→ focused child → `EditBox.charTyped(CharacterEvent)` → `insertText(event.codepointAsString())` |
| **IME preedit（26.x 新增）** | `preeditCallback(JLnet/minecraft/client/input/PreeditEvent;)V`（private，GLFW preedit 回调）→ `public static submitPreeditEvent(GuiEventListener, PreeditEvent)` | → `GuiEventListener.preeditUpdated(PreeditEvent)`（ContainerEventHandler 默认转发 focused child）→ `EditBox.preeditUpdated` / `MultiLineEditBox.preeditUpdated` / `AbstractSignEditScreen.preeditUpdated` |
| IME 开关状态 | `GLFWIMEStatusCallbackI` | → `TextInputManager.notifyIMEChanged()` / `setIMEInputMode` |

事件记录（record 类，`net.minecraft.client.input`）：

```java
public record KeyEvent(int key, int scancode, int modifiers)
    // 另有 hasShiftDown() / hasControlDownWithQuirk() 等（InputWithModifiers 默认方法）
public record CharacterEvent(int codepoint) { String codepointAsString(); boolean isAllowedChatCharacter(); }
public record PreeditEvent(String fullText, int caretPosition, List<String> blocks, int focusedBlock) {
    static PreeditEvent createFromCallback(int, long, int, long, int, int); // 读 GLFW 原生缓冲区，WaylandCraft 用不到
    MutableComponent toFormattedText(Style);
}
```

注意点：
- `EditBox.keyPressed(KeyEvent)` 用 `tableswitch 259..269` 处理方向键/删除键/Home/End，Esc/Enter 等由 Screen 处理；普通字符**不进 keyPressed**，走 `charTyped`。
- 因此"键盘字符输入"的注入点有两个语义不同的位置：
  - 想拦/改**按键级**事件 → `Screen.keyPressed(KeyEvent)` HEAD 或 `KeyboardHandler.keyPress` HEAD（项目已有 `KeyboardHandlerMixin` 注入先例）。
  - 想拦/改**字符落框** → `EditBox.charTyped(CharacterEvent)` HEAD（cancellable）。
  - 想喂 **preedit** → 直接调 `KeyboardHandler.submitPreeditEvent(screen, new PreeditEvent(...))`（public static，**不需要 mixin**），或 `editBox.preeditUpdated(event)`。

### 3.2 推荐 mixin 注入点（mixin target 字符串，供 fabric.mod / mixins.json 使用）

| 目的 | Target（class + method + 描述符） |
|---|---|
| 获取焦点组件变化 | `AbstractContainerEventHandler.setFocused(Lnet/minecraft/client/gui/components/events/GuiEventListener;)V` @Inject HEAD |
| IME 活跃时吞键（数字/翻页/选择） | `Screen.keyPressed(Lnet/minecraft/client/input/KeyEvent;)Z` @Inject HEAD cancellable（回调 `CallbackInfoReturnable<Boolean>`）|
| 游戏内候选窗绘制（屏幕级，最稳） | `Screen.extractRenderState(Lnet/minecraft/client/gui/GuiGraphicsExtractor;IIF)V` @Inject TAIL |
| 游戏内候选窗绘制（贴文本框） | `EditBox.extractWidgetRenderState(Lnet/minecraft/client/gui/GuiGraphicsExtractor;IIF)V` @Inject TAIL |
| preedit 注入拦截/替换 vanilla 浮层 | `EditBox.preeditUpdated(Lnet/minecraft/client/input/PreeditEvent;)Z` @Inject HEAD cancellable |
| commit 直写入口（也可直接方法调用，无需 mixin） | `EditBox.insertText(Ljava/lang/String;)V`（public，直接调用）|
| 读私有锚点字段 | `EditBox` 字段 `textX:I` / `textY:I` / `displayPos:I` / `cursorPos:I` / `value:Ljava/lang/String;` — `@Accessor` |

> 告示牌：输入在 `AbstractSignEditScreen` 自己处理（`keyPressed` / `charTyped` / `preeditUpdated` 都重写了，preedit 渲染在 `extractRenderState` 里调 `extractSignText`），没有聚焦子组件，前端要单独判 `screen instanceof AbstractSignEditScreen` 并把 preedit/commit 交给 `signField`（TextFieldHelper）。

---

## 4. 屏幕坐标 / 尺寸（候选窗锚点）

### 4.1 组件矩形（AbstractWidget）

```java
public int getX() / setX(int) / public int getY() / setY(int)   // x,y 是 private，只有 getter
protected int width / height（有 public getWidth()/getHeight()/setWidth/setHeight）
public int getRight() / getBottom()
public ScreenRectangle getRectangle()   // record: left()/top()/right()/bottom()/containsPoint(int,int)
```

### 4.2 光标屏幕位置（候选窗锚点，关键）

- 官方 API：`EditBox.getScreenX(int position)` → `getX() + font.width(value.substring(0, position))`。
  注意：**不扣 `displayPos` 滚动偏移**，长文本滚动后不精确；但 vanilla 的 `CommandSuggestions.showSuggestions`
  就是用 `input.getScreenX(suggestions.getRange().getStart())` + `Mth.clamp(x, getScreenX(0), getScreenX(0)+getInnerWidth()-maxWidth)`
  来锚定补全窗的 —— 游戏内候选窗**直接抄这个算法**即可，和原版行为一致。
- 精确做法：`textX`/`textY` 是 private（`updateTextPosition()` 计算：
  `textX = getX() + (centered ? (width - font.width(文本))/2 : (bordered ? 4 : 0))`；
  `textY = getY() + (bordered ? (height-8)/2 : 0)`）。mixin `@Accessor("textX")`/`@Accessor("textY")` 读出来，
  再加 `font.width(可见文本.substring(0, cursorPos - displayPos))` 得光标精确 x。
- `EditBox.getInnerWidth()` = `width - (bordered ? 8 : 0)`。
- 多行：`AbstractTextAreaWidget.getInnerLeft()/getInnerTop()/getInnerHeight()`；行矩形用
  `MultilineTextField.getLineView(int)`（StringView 有 start/end + 行首 x 偏移）。

---

## 5. 渲染管线（26.x 大改：renderWidget(GuiGraphics) 已不存在）

### 5.1 渲染入口

- 26.x 渲染模型改为 **render-state 提取**：`AbstractWidget.extractRenderState(GuiGraphicsExtractor, int, int, float)`（final）
  → `protected abstract extractWidgetRenderState(GuiGraphicsExtractor, int, int, float)`。
  **没有** `renderWidget(GuiGraphics, ...)`；`GuiGraphics` 文本 API 也被
  `GuiGraphicsExtractor.text(...)` 取代（不再是 `GuiGraphics.drawString`）。
- `Screen.extractRenderStateWithTooltipAndSubtitles(GuiGraphicsExtractor, int, int, float)`（final）→ `Screen.extractRenderState(...)`（子类重写）。

### 5.2 EditBox.extractWidgetRenderState 内部（字节码确认的顺序）

1. 背景：`GuiGraphicsExtractor.blitSprite(RenderPipelines.GUI_TEXTURED, SPRITES.get(isActive, isFocused), getX(), getY(), getWidth(), getHeight())`
2. 文本：`GuiGraphicsExtractor.text(Font, FormattedCharSequence, x, y, color, shadow)`（`applyFormat` 跑 formatters）
3. 补全灰色：`text(Font, String, x-1, textY, -8355712, shadow)`
4. 选区高亮：`GuiGraphicsExtractor.textHighlight(int x1, int y1, int x2, int y2, boolean invert)`
5. **光标（闪烁在这里）**：
   - 可见性：`TextCursorUtils.isCursorVisible(Util.getMillis() - focusedTime)` → `(elapsed/300)%2==0`（300ms 半周期，`CURSOR_BLINK_INTERVAL_MS`）
   - 绘制：`TextCursorUtils.extractInsertCursor(GuiGraphicsExtractor, x, y, color, height)`（插入态光标条）
     或 `TextCursorUtils.extractAppendCursor(GuiGraphicsExtractor, Font, x, y, color, shadow)`（框末附加光标）
6. 光标类型：`GuiGraphicsExtractor.requestCursor(CursorTypes.IBEAM / NOT_ALLOWED)`
7. **preedit 浮层**：`IMEPreeditOverlay.updateInputPosition(x, y)` → `GuiGraphicsExtractor.setPreeditOverlay(Renderable)`
   （该 Renderable 由渲染器在提取阶段绘制；`IMEPreeditOverlay.extractRenderState` 内部还调
   `Minecraft.textInputManager().setTextInputArea(l,t,r,b)` 通知 OS 级输入法候选框位置）

`GuiGraphicsExtractor` 相关签名：

```java
public void text(Font, String, int x, int y, int color)
public void text(Font, String, int x, int y, int color, boolean shadow)
public void text(Font, FormattedCharSequence, int x, int y, int color, boolean shadow)
public void text(Font, Component, int x, int y, int color, boolean shadow)
public void centeredText(Font, ..., int y, int color)
public void textHighlight(int x1, int y1, int x2, int y2, boolean invert)
public void fill(int x1, int y1, int x2, int y2, int color)
public void blitSprite(RenderPipeline, Identifier, int x, int y, int w, int h)
public void enableScissor(int,int,int,int) / disableScissor()
public void setPreeditOverlay(Renderable)
```

---

## 6. 对 WaylandCraft 的集成建议（基于以上事实）

1. **commit 直写**：不需要 mixin。拿到 `screen.getFocused()`（或 `getCurrentFocusPath().component()`）后
   `if (focused instanceof EditBox eb) eb.insertText(commitText);` 即可（`insertText` 自动处理选区替换 + filterText + maxLength）。
2. **preedit 内嵌**（设计文档 D7）：两条路
   - 复用 vanilla：`KeyboardHandler.submitPreeditEvent(screen, new PreeditEvent(fullText, caret, blocks, focusedBlock))`
     （public static，零 mixin）；preedit 结束传 `null` 清空浮层。
   - 或自绘：mixin `EditBox.preeditUpdated` HEAD cancellable，把 `preeditOverlay` 字段换成自己的 Renderable。
3. **候选窗自绘**（D1/D5）：推荐注入 `Screen.extractRenderState` TAIL（屏幕级绘制，对所有文本组件生效），
   或 `EditBox.extractWidgetRenderState` TAIL（贴框绘制）。锚点算法抄 `CommandSuggestions.showSuggestions`：
   `x = Mth.clamp(editBox.getScreenX(cursorPos), editBox.getScreenX(0), editBox.getScreenX(0) + editBox.getInnerWidth() - popupWidth)`，
   `y = editBox.getY() + editBox.getHeight()`（或光标行下沿）。背景/文字用 `GuiGraphicsExtractor.fill / text`。
4. **键盘门控**（D3）：`Screen.keyPressed(KeyEvent)` HEAD cancellable —— IME 会话活跃时吞掉字符/数字/翻页/选择键，
   未激活全放行（注意别吞 Esc/Enter；vanilla `EditBox.keyPressed` 的 tableswitch 259-269 是方向/删除键）。
5. **虚拟窗口路径不动**（ti3/im2 已有），Rust→Java 新回调（Candidates/CursorRect/CommitString/PreeditString）落到 §3.2 的注入点。

---

## 7. 最小 mixin 代码草图（示意，未编译验证）

```java
// dev/evvie/waylandcraft/mixin/EditBoxMixin.java —— 候选窗锚点 + 光标后绘制
@Mixin(EditBox.class)
public abstract class EditBoxMixin {
    @Accessor("textX") public abstract int waylandcraft$getTextX();
    @Accessor("textY") public abstract int waylandcraft$getTextY();
    @Accessor("displayPos") public abstract int waylandcraft$getDisplayPos();

    // 目标字符串：extractWidgetRenderState(Lnet/minecraft/client/gui/GuiGraphicsExtractor;IIF)V
    @Inject(method = "extractWidgetRenderState",
            at = @At("TAIL"))
    private void waylandcraft$drawCandidatePopup(GuiGraphicsExtractor graphics,
                                                 int mouseX, int mouseY, float tickDelta,
                                                 CallbackInfo ci) {
        EditBox self = (EditBox) (Object) this;
        if (IMContext.INSTANCE.target() == self && IMContext.INSTANCE.hasCandidates()) {
            IMContext.INSTANCE.popup().draw(self, graphics);   // 自绘候选窗，锚点见 §4.2
        }
    }
}
```

```java
// dev/evvie/waylandcraft/mixin/ScreenMixin.java —— IME 活跃时吞键 + 屏幕级候选窗
@Mixin(Screen.class)
public class ScreenMixin {
    // 目标字符串：keyPressed(Lnet/minecraft/client/input/KeyEvent;)Z
    @Inject(method = "keyPressed", at = @At("HEAD"), cancellable = true)
    private void waylandcraft$gateImeKeys(KeyEvent event, CallbackInfoReturnable<Boolean> cir) {
        if (IMContext.INSTANCE.isComposing() && IMContext.INSTANCE.shouldConsume(event)) {
            cir.setReturnValue(true);   // 吞键：不进 Screen/EditBox
        }
    }

    // 目标字符串：extractRenderState(Lnet/minecraft/client/gui/GuiGraphicsExtractor;IIF)V
    @Inject(method = "extractRenderState", at = @At("TAIL"))
    private void waylandcraft$drawScreenCandidatePopup(GuiGraphicsExtractor graphics,
                                                       int mouseX, int mouseY, float tickDelta,
                                                       CallbackInfo ci) {
        IMContext.INSTANCE.popup().drawScreen((Screen) (Object) this, graphics);
    }
}
```

```java
// dev/evvie/waylandcraft/mixin/KeyboardHandlerMixin.java —— 追加：接收 Rust→Java preedit/commit 时注入
// （项目已有此 mixin，这里展示怎么喂 preedit 事件，无需新注入点）
public class KeyboardHandlerMixin {   // 示意：Rust 回调里这样用
    // IMContext.commitText()  → ((EditBox)focused).insertText(text)
    // IMContext.setComposingText(fullText, caret, blocks, focusedBlock)
    //   → KeyboardHandler.submitPreeditEvent(Minecraft.getInstance().screen,
    //        new PreeditEvent(fullText, caret, blocks, focusedBlock))
    // IMContext.finishComposing() → KeyboardHandler.submitPreeditEvent(screen, null)
}
```

新 mixin 需登记进 `src/main/resources/waylandcraft.client.mixins.json`（package `dev.evvie.waylandcraft.mixin`）。

---

## 附录：关键类速查（全部 javap 实测）

- `net.minecraft.client.gui.components.EditBox`（单行）→ `AbstractWidget`
- `net.minecraft.client.gui.components.MultiLineEditBox`（多行）→ `AbstractTextAreaWidget` → `AbstractScrollArea` → `AbstractWidget`
- `net.minecraft.client.gui.font.TextFieldHelper`（告示牌）
- `net.minecraft.client.gui.components.IMEPreeditOverlay`（内建 preedit 浮层，`implements Renderable`）
- `net.minecraft.client.gui.components.CommandSuggestions$SuggestionsList`（候选窗参考）
- `net.minecraft.client.gui.components.TextCursorUtils`（光标闪烁：`isCursorVisible(J)Z`、`extractInsertCursor`、`extractAppendCursor`）
- `net.minecraft.client.input.{KeyEvent, CharacterEvent, PreeditEvent}`
- `net.minecraft.client.KeyboardHandler`（`submitPreeditEvent` public static；`preeditCallback`/`keyPress`/`charTyped` private）
- `net.minecraft.client.gui.screens.Screen`（`getFocused()` 继承自 `AbstractContainerEventHandler`；`getCurrentFocusPath()` 默认方法）
- `com.mojang.blaze3d.platform.TextInputManager`（`setTextInputArea(int,int,int,int)` — OS 输入法候选框位置）
