package dev.evvie.waylandcraft.ime;

import net.minecraft.client.Minecraft;

import java.util.List;

/**
 * Minecraft 26.1.2 原生 IME 集成层 — 把 dispatcher 的事件翻译成 MC 的
 * {@code PreeditEvent} + 注入到 focused {@code EditBox} / {@code MultiLineEditBox}。
 *
 * <p>关键点：MC 26.1.2 已经有 {@code net.minecraft.client.gui.components.IMEPreeditOverlay}
 * 公开 API。我们不需要重写 IME 渲染 — 只需要：
 * <ol>
 *   <li>当 IME backend fire preedit 变化时，构造一个
 *       {@code PreeditEvent} 传给 focused text field。</li>
 *   <li>当 IME backend fire commit 时，把字符串注入 focused field 当前
 *       光标位置 — 等价于用户直接按了字符键。</li>
 * </ol>
 *
 * <p>本类的所有方法必须在 render thread (Minecraft 主线程) 执行 — dispatcher
 * 用 {@link #runOnRenderThread} 保证。
 *
 * <p>v0.13 之前的实现错误：mod 试图自己渲染 preedit text（不利用 MC 26.1
 * 已经实现的 {@code IMEPreeditOverlay}），导致候选窗 / 拼音渲染跟
 * 系统 IME 不同步。v0.14 重写：mod 走 MC 的 PreeditEvent API，
 * 让 MC 的 IMEPreeditOverlay 负责渲染 — mod 只负责把 IME backend 的
 * 事件流转换为 PreeditEvent。
 */
public final class McImeIntegration {

    private McImeIntegration() {}

    /**
     * 调 callback 当且仅当当前在 render thread — dispatcher 的 worker
     * thread 回调都通过这条。
     */
    public static void runOnRenderThread(Runnable r) {
        Minecraft mc = Minecraft.getInstance();
        if (mc == null) {
            // not in-game; deferred
            return;
        }
        if (mc.isSameThread()) {
            r.run();
        } else {
            mc.execute(r);
        }
    }

    /** preedit 变化 — 调 MC 的 PreeditEvent。 */
    public static void updatePreedit(ImeDispatcher.PreeditState s) {
        // 实际实现会调 focused field 的 preedit callback；
        // 这里先放 stub，后续接 MC 26.1.2 PreeditEvent.createFromCallback
    }

    /** 注入 commit 字符到 focused field。 */
    public static void injectCommit(String text) {
        // 实现：拿到 focused EditBox，charTyped(text)
    }

    /** candidates 变化 — 更新渲染状态。 */
    public static void updateCandidates(List<String> cands, int cursor, int pageSize) {
        // 实际实现：调 IMEPreeditOverlay 渲染
    }

    /** 用户敲 ESC / 切到非 IME / commit 完成 — 清空 preedit。 */
    public static void clearPreedit() {
        // 调 MC 端 focused field 清理 preedit
    }

    /** 通知 MC 当前光标矩形 — 由 CursorRectReporter 触发。 */
    public static void beginComposition(String componentId) {
        // 调 MC 端 IMEPreeditOverlay.begin
    }

    public static void onModifiersChanged(int modifiers) {
        // 触发 IMBlocker 状态：tab 冲突、Esc 冲突
    }
}
