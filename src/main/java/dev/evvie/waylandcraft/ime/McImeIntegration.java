package dev.evvie.waylandcraft.ime;

import net.minecraft.client.Minecraft;
import net.minecraft.client.KeyboardHandler;
import net.minecraft.client.gui.components.EditBox;
import net.minecraft.client.gui.components.MultiLineEditBox;
import net.minecraft.client.gui.components.events.GuiEventListener;
import net.minecraft.client.gui.screens.ChatScreen;
import net.minecraft.client.input.CharacterEvent;
import net.minecraft.client.input.PreeditEvent;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.List;

/**
 * Minecraft 26.1.2 原生 IME 集成层 — 把 dispatcher 的事件翻译成 MC 的
 * {@code PreeditEvent} + 注入到 focused {@code EditBox} / {@code MultiLineEditBox}。
 *
 * <p>本类在 MC 26.1.2 jar 反编译后实操验证的关键 API（class file inspection）：
 * <ul>
 *   <li>{@code KeyboardHandler.submitPreeditEvent(GuiEventListener, PreeditEvent)} —
 *       <b>static</b> 方法，把 IME 候选/预编辑事件直接 push 给当前 focused
 *       widget。MC 自己的 {@code IMEPreeditOverlay} 自动渲染。</li>
 *   <li>{@code KeyboardHandler.resubmitLastPreeditEvent(GuiEventListener)} —
 *       重发 lastPreeditEvent（focus 切换时 mod 用）。</li>
 *   <li>{@code EditBox.charTyped(CharacterEvent)} — 注入已 commit 的字符
 *       到当前光标位置。</li>
 *   <li>{@code EditBox.setValue(String)} / {@code EditBox.insertText(String)} —
 *       强注入（提交整段 / 替换）。</li>
 * </ul>
 *
 * <p>本方法在 render thread 调用 — dispatcher 用 {@link #runOnRenderThread} 保证。
 */
public final class McImeIntegration {

    private static final Logger LOGGER = LoggerFactory.getLogger(McImeIntegration.class);

    private McImeIntegration() {}

    /**
     * 调 callback 当且仅当当前在 render thread — dispatcher 的 worker
     * thread 回调都通过这条。
     */
    public static void runOnRenderThread(Runnable r) {
        Minecraft mc = Minecraft.getInstance();
        if (mc == null) return;
        if (mc.isSameThread()) {
            r.run();
        } else {
            mc.execute(r);
        }
    }

    /**
     * Preedit 变化 — 构造 MC 26.1.2 的 {@code PreeditEvent} 调
     * {@code KeyboardHandler.submitPreeditEvent(focused, event)}。
     * MC 自己的 {@code IMEPreeditOverlay} 负责渲染 — mod 不重画。
     */
    public static void updatePreedit(ImeDispatcher.PreeditState s) {
        Minecraft mc = Minecraft.getInstance();
        if (mc == null || mc.screen == null) return;
        GuiEventListener focused = mc.screen.getFocused();
        if (focused == null) return;

        // PreeditEvent 是 record (String, int, List<String>, int)
        // 第三个参数是"格式化块"（text + 格式）— 我们用单 block 简化
        // 第四个参数是当前 block（高亮候选的）— 没候选时填 0
        PreeditEvent event = new PreeditEvent(
                s.text(),
                s.cursorBegin(),
                List.of(s.text()),
                0
        );
        try {
            KeyboardHandler.submitPreeditEvent(focused, event);
        } catch (Throwable t) {
            LOGGER.warn("[ime] submitPreeditEvent failed: {}", t.toString());
        }
    }

    /**
     * 注入 commit 字符到 focused field — MC 自己的 EditBox.charTyped()
     * 路径（包含 clipboard / undo / cursor 移动等）。
     */
    public static void injectCommit(String text) {
        Minecraft mc = Minecraft.getInstance();
        if (mc == null || mc.screen == null || text == null || text.isEmpty()) return;

        GuiEventListener focused = mc.screen.getFocused();
        if (focused == null) return;

        if (focused instanceof EditBox eb) {
            // 强制整体注入 — 跳过 char-by-char（避免触发 IME 重新进入）
            eb.insertText(text);
        } else if (focused instanceof MultiLineEditBox mleb) {
            mleb.insertText(text);
        } else {
            // 通用 fallback — 用 charTyped 逐字符
            for (int i = 0; i < text.length(); ) {
                int cp = text.codePointAt(i);
                try {
                    focused.charTyped(new CharacterEvent(mc, cp, 0));
                } catch (Throwable ignored) {}
                i += Character.charCount(cp);
            }
        }
    }

    /**
     * candidates 变化 — MC 没有 candidates 的独立 API，但
     * {@link ImeDispatcher.PreeditState} 的 candidates field 会被
     * ImeRenderOverlay（v0.15+）独立渲染。这里保留接口以便将来扩展。
     */
    public static void updateCandidates(List<String> cands, int cursor, int pageSize) {
        // placeholder — v0.15+ 加 ImeRenderOverlay 渲染候选
    }

    /** ESC / commit 完成 — 清空 preedit。 */
    public static void clearPreedit() {
        Minecraft mc = Minecraft.getInstance();
        if (mc == null || mc.screen == null) return;
        GuiEventListener focused = mc.screen.getFocused();
        if (focused == null) return;
        try {
            KeyboardHandler.submitPreeditEvent(focused, new PreeditEvent("", 0, List.of(), 0));
        } catch (Throwable t) {
            LOGGER.warn("[ime] clearPreedit failed: {}", t.toString());
        }
    }

    /**
     * 当前 EditBox 获得 focus（Screen.setFocused 触发）— 让 MC 重新
     * 提交 lastPreeditEvent 把当前 IME 候选窗位置更新。
     */
    public static void beginComposition(String componentId) {
        Minecraft mc = Minecraft.getInstance();
        if (mc == null || mc.screen == null) return;
        GuiEventListener focused = mc.screen.getFocused();
        if (focused == null) return;
        try {
            KeyboardHandler kbh = mc.keyboardHandler;
            if (kbh != null) kbh.resubmitLastPreeditEvent(focused);
        } catch (Throwable t) {
            LOGGER.debug("[ime] beginComposition resubmit: {}", t.toString());
        }
    }

    public static void onModifiersChanged(int modifiers) {
        // 触发 IMBlocker 状态（Tab/Enter/Esc 冲突）— v0.15+ 实现
    }
}
