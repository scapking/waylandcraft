# IME 重构架构设计 v0.1 — 输入法输出直送窗口 + 游戏内候选窗

## 1. 现状问题（用户实机证据）
1. 数字键泄漏进窗口（IME 消费数字键选字，同时键被转发给窗口）
2. 汉字 commit 进不了窗口（v0.9.31 两个解析 bug，1.1.51 已修，未实机验证）
3. 候选窗随机漂移（宿主面板坐标映射不可靠；ibus-daemon --panel disable 下面板不可用）
4. 极端快速输入时出现垃圾文本（GObject 类型名泄漏，1.1.51 已修）
5. 底层方向问题：键盘拦截 + 模拟注入，而非"输入法输出直送窗口"

## 2. 目标架构

```
                 ┌─────────────── 游戏内（Java）───────────────┐
  真实键盘 GLFW ─→ KeyboardHandlerMixin → bridge
                 │                                            │
                 │  IMContext（焦点 TextField / 虚拟窗口焦点）  │
                 │   ├ commitText()  ← 直写文本字段            │
                 │   ├ setComposingText() ← 字段内联拼音       │
                 │   └ setCandidates() ← 候选列表 → 自绘候选窗 │
                 └──────────────┬─────────────────────────────┘
                                │ JNI (Rust→Java 回调，新)
                 ┌──────────────▼──────────────┐
                 │ Rust 统一事件模型（新扩展）    │
                 │ HostEvent +=                 │
                 │   Candidates(visible,list,   │
                 │     cursor,page,total)       │
                 │   CursorRect(x,y,w,h)        │
                 └──────────────┬──────────────┘
                    ┌───────────┴───────────┐
                    ▼                       ▼
        ┌─ dbus-ibus ────────┐   ┌─ dbus-fcitx5 ───────┐
        │ +LookupTable 解析   │   │ +ClientSideUI 解析   │
        │ +PageUp/Down       │   │ +候选导航            │
        │ +数字键消费门控     │   │ +数字键消费门控       │
        └────────────────────┘   └────────────────────┘
```

## 3. 关键设计决策
- D1: 候选窗 = 游戏内自绘（Java 渲染层）。位置 = 焦点文本位置（TextField 光标 或 虚拟窗口 im2 cursor rect）。彻底脱离宿主面板坐标映射 → 不漂移。
- D2: commit/preedit 对**虚拟窗口**走 ti3/im2 协议直送（已有，保持）；对**游戏内聊天框**走 Java IMContext 直写 TextField（新）。删掉"模拟按键注入"路径。
- D3: 键盘门控：IME 有活跃 preedit/候选会话时，字符/数字/翻页/选择键**只进 IME，不放行窗口**；IME 未激活全放行。ibackend 的 submit_key 已有，需加"会话活跃"判定。
- D4: Rust→Java 回调：init 时保存 JavaVM，worker 线程 attach，调用 IMContext 静态方法。事件保序（沿用 ev_tx 通道 + 每帧 poll）。
- D5: 候选数据流（ibus）：
  - UpdateLookupTable(IBusLookupTable) → 解析 → Candidates 事件
  - ShowLookupTable/HideLookupTable → visible 切换
  - PageUp/PageDown 由引擎方法触发（候选翻页）；选字走 ProcessKeyEvent 数字键（引擎内部消费）
- D6: 兼容性矩阵：Wayland 原生 = ti3/im2（已有）；XWayland = dbus-ibus/fcitx5（已有+补候选）；XIM 暂不实现（GLFW 不支持，记录在案）。
- D7: preedit 游戏内聊天框内嵌显示（先把 preedit 拼进 TextField 文本，朴素版），候选窗自绘（必须版）。

## 4. 事件模型扩展（Rust）
```rust
pub enum HostEvent {
    Enter,
    Leave,
    CommitString(String),
    PreeditString(String, i32, i32),
    DeleteSurroundingText(u32, u32),
    Done(u32),
    // —— 新增 ——
    Candidates {
        visible: bool,
        entries: Vec<CandidateEntry>,   // { label: String, text: String, comment: String }
        cursor_index: usize,            // 高亮候选
        page_size: usize,
        page_is_first: bool,
        page_is_last: bool,
        round: bool,
    },
    CursorRect { x: i32, y: i32, w: i32, h: i32 },  // 焦点光标位置（候选窗锚点）
}

pub struct CandidateEntry {
    pub label: String,   // 序号（"1." / "①" 或空）
    pub text: String,    // 候选文本
    pub comment: String, // 注释（读音等）
}
```

## 4b. 调研发现（P0 已完成，Agent 报告要点）
- **MC 26.x 渲染是两段式**：无 GuiGraphics，用 `GuiGraphicsExtractor`（extract/render）。画文本 = `extractor.text(font,s,x,y,color,shadow)`；矩形 = `fill`；1px 边框 = 新方法 `outline(x,y,w,h,color)`。全程 GUI 单位，`guiWidth()=Window.getGuiScaledWidth()`。
- **MC 26.1.2 自带原生 IME 管线**（重大利好）：
  - `net.minecraft.client.input.PreeditEvent`（record: fullText/caretPosition/blocks/focusedBlock）
  - `KeyboardHandler.submitPreeditEvent(GuiEventListener, PreeditEvent)` → `EditBox.preeditUpdated` → 自动创建 `IMEPreeditOverlay` 画内联拼音+光标（输入框下方弹层、超界翻转、blitSprite 背景）——**preedit 不用自绘，直接喂原生管线**
  - `TextInputManager.setTextInputArea(x1,y1,x2,y2)` 上报候选区域给宿主 IME（GUI 单位，内部×scale）——可用于对齐宿主面板/告知候选位置
  - MC 原生只画 preedit，**不画候选列表** → 候选窗自绘
- **候选窗渲染钩子**：`@Mixin(Screen.class)` 注入 `extractRenderStateWithTooltipAndSubtitles` @RETURN（final 方法，覆盖所有 Screen；Fabric API 等价物 = ScreenEvents.afterExtract）
- **EditBox 定位**：`getX/getY/getBottom/getWidth`、`getCursorPosition()`、`getScreenX(charIndex)`（= x + font.width(substring)）→ 光标屏幕 x；定位算法照抄 IMEPreeditOverlay：锚点=光标处/输入框下方，超界翻转夹紧
- **候选窗渲染器最小草图**：半透明底 fill + outline 边框 + 高亮行 fill + text 行；ROW_HEIGHT=12、PAD=2；实现 Renderable 挂 Screen.renderables 或直接 extract 注入
- ibus/fcitx5 候选协议调研结果待 Agent 返回（task-5b58ef335fb3 / task-1602df038e1d）

## 5. Java 侧新增组件
- `ime/IMContext.java` — 单例：commitText/setComposingText/setCandidates/焦点管理；持有当前目标（游戏 TextField 或虚拟窗口）
- `ime/CandidatePopupRenderer.java` — 自绘候选窗（背景、边框、高亮、文字；锚点=光标矩形；屏幕边界翻转）
- `ime/TextFieldIMBridge.java` — mixin 集成：注入点=Screen 焦点切换、TextField 渲染/光标、键盘
- Mixins：`TextFieldWidgetMixin`（光标/插入/渲染坐标）、`ScreenMixin`（focus 变化、渲染钩子）
- preedit 走 MC 原生管线（submitPreeditEvent），候选窗自绘

## 6. 实施顺序（P0 调研 → P1 核心 → P2 完善）
- P0 ✅ 调研（4 Agent 并行：MC TextField API / ibus LookupTable / fcitx5 ClientSideUI / MC GUI 渲染）
- P1.1 Rust: HostEvent 扩展 + dbus-ibus LookupTable 解析 + dbus-fcitx5 ClientSideUI 解析
- P1.2 Rust: Rust→Java 回调通道（JavaVM attach + IMContext 调用）
- P1.3 Java: IMContext + TextField 集成（commit 直写 / preedit 内嵌 / 焦点绑定）
- P1.4 Java: CandidatePopupRenderer 自绘候选窗
- P1.5 键盘门控精确化（IME 会话活跃时吞键）
- P2: 集成测试（Rust 侧解析单测 + Java 侧 mock）、CI、实机验证
