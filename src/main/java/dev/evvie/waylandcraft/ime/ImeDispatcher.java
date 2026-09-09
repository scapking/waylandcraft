package dev.evvie.waylandcraft.ime;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;



import java.util.List;
import java.util.Objects;
import java.util.Optional;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.atomic.AtomicReference;

/**
 * IME 事件分发器 — mod 主进程唯一的 IME 协调器。
 *
 * <p>职责：
 * <ol>
 *   <li>在 mod 启动时通过 {@link ImeBackendRegistry} 选 backend 并 start。</li>
 *   <li>把 backend 回调（preedit / commit / candidates）翻译成
 *       Minecraft 26.1.2 的 {@code PreeditEvent} 输入 + 注入到 focused
 *       {@code EditBox} / {@code MultiLineEditBox}。</li>
 *   <li>把 MC focused 文本框的光标矩形回写给 backend
 *       （{@link #setCursorRectangle} 内部委托 {@code CursorRectReporter}
 *       已经做的 tick loop）。</li>
 *   <li>IMBlocker 协调（见 v0.13+ IMBlocker 思路）：当 candidate
 *       popup 显示时，截获冲突键（Tab/Enter/Esc）由 IME 消费而不是游戏。</li>
 * </ol>
 *
 * <p>关键设计：dispatcher 不知道 backend 是什么 — 它只跟
 * {@link ImeBackend.ImeSession} 接口对话。fcitx5 / ibus / XIM / 平台
 * native 全部走同一条路径。
 *
 * <p>线程模型：
 * <ul>
 *   <li>backend 回调可能在 worker thread（dbus callback / XIM event）—
 *       listener 内部用 {@code Minecraft.getInstance().execute(Runnable)}
 *       切回主线程。</li>
 *   <li>mod 调 {@link #setCursorRectangle} / {@link #onFocusedTextFieldChanged} 必须在
 *       render thread。</li>
 * </ul>
 */
public final class ImeDispatcher {

    private static final Logger LOGGER = LoggerFactory.getLogger(ImeDispatcher.class);

    private static final ImeDispatcher INSTANCE = new ImeDispatcher();

    public static ImeDispatcher get() {
        return INSTANCE;
    }

    private final AtomicReference<ImeBackend.ImeSession> currentSession =
            new AtomicReference<>();
    private final CopyOnWriteArrayList<FocusedTextField> textFields =
            new CopyOnWriteArrayList<>();

    private volatile PreeditState preedit = PreeditState.EMPTY;
    /** 当前 MC 是否有文本框持焦点（驱动 backend focusIn/focusOut）。 */
    private volatile boolean inputFocused = false;
    private volatile List<String> candidates = List.of();
    private volatile int candidateCursor = 0;
    private volatile int candidatePageSize = 0;

    private ImeDispatcher() {}

    /**
     * 启动 IME — 在 mod init 时调一次。如果当前 host 没有可用 IME
     * backend，悄默忽略（mod 仍可工作，只是 IME 不可用）。
     */
    public void start() {
        Optional<ImeBackend.ImeSession> session = ImeBackendRegistry.start();
        if (session.isEmpty()) {
            LOGGER.info("[ime] no backend available, IME disabled");
            return;
        }
        ImeBackend.ImeSession s = session.get();
        currentSession.set(s);
        s.setEventListener(new RelayListener(s));
        LOGGER.info("[ime] dispatcher started with session seat={}", s.seat());
    }

    /**
     * 关闭 IME — mod shutdown / world 切换时调。
     */
    public void stop() {
        ImeBackend.ImeSession s = currentSession.getAndSet(null);
        if (s != null) {
            try {
                s.close();
            } catch (Throwable t) {
                LOGGER.warn("[ime] session close threw: {}", t.toString());
            }
        }
    }

    /**
     * 注册一个 focused text field — Minecraft 26.1.2 的
     * {@code EditBox} / {@code MultiLineEditBox} 启动时调。当 preedit / commit
     * 来时，dispatcher 把文本注入这个 field。
     *
     * <p>field 用 {@code (componentId, textField)} 二元组标识 — 同一时刻
     * 只能有一个 active field（嵌套 wayland 协议决定 — 焦点只能在一个
     * surface）。多 surface 时只有最顶层 field 接收 IME 输入。
     */
    public void registerTextField(String componentId, FocusedTextField field) {
        Objects.requireNonNull(field, "field");
        textFields.removeIf(f -> f.componentId().equals(componentId));
        textFields.add(field);
    }

    public void unregisterTextField(String componentId) {
        textFields.removeIf(f -> f.componentId().equals(componentId));
    }

    /**
     * 当前 focused text field 变化时调 — Wayland {@code enter} / {@code leave}
     * 事件在 mod 端的对应。MC 26.1.2 的 {@code Screen.setFocused} 触发这条。
     *
     * <p>{@code focused == null} = focus lost (= 之前的 field 失去焦点)
     * — dispatcher 清空 preedit + 通知 backend end session。
     */
    public void onFocusedTextFieldChanged(String componentId) {
        ImeBackend.ImeSession s = currentSession.get();
        if (s == null) return;

        FocusedTextField focused = textFields.stream()
                .filter(f -> f.componentId().equals(componentId))
                .findFirst().orElse(null);

        if (focused == null) {
            // leave: clear state
            preedit = PreeditState.EMPTY;
            candidates = List.of();
            candidateCursor = 0;
            candidatePageSize = 0;
            // notify MC: clear any preedit
            McImeIntegration.clearPreedit();
            return;
        }

        // enter: notify MC + backend
        McImeIntegration.beginComposition(componentId);
    }

    /**
     * MC 文本框输入焦点状态（每 tick 由 MinecraftMixin 检测 EditBox
     * focused 后调用）。变化时通知 backend focusIn/focusOut —— fcitx5/ibus
     * 只有 FocusIn 后才把按键交给 IME 引擎（v1.2.26 验证：缺这一步导致
     * IC 永不激活，preedit/commit 收不到）。
     */
    public void onScreenInputFocusChanged(boolean active) {
        if (active == inputFocused) return;
        inputFocused = active;
        ImeBackend.ImeSession s = currentSession.get();
        if (s == null) return;
        try {
            if (active) {
                s.focusIn();
            } else {
                s.focusOut();
                preedit = PreeditState.EMPTY;
                candidates = List.of();
                McImeIntegration.clearPreedit();
            }
        } catch (Throwable t) {
            LOGGER.warn("[ime] focus change threw: {}", t.toString());
        }
    }

    /**
     * 把光标矩形推给 IME backend — 桌面候选窗锚点。每 tick 由
     * {@code CursorRectReporter} 触发（位置变化时）。
     *
     * <p>v0.9.33 "候选窗漂移" 根因之一：锚点不乘 guiScale / 不在
     * 文本框实际屏幕坐标。这里强制要求 backend 实现此方法 —
     * 接口里已经 javadoc 注明 "不能 no-op"。
     */
    public void setCursorRectangle(int x, int y, int width, int height) {
        ImeBackend.ImeSession s = currentSession.get();
        if (s == null) return;
        try {
            s.setCursorRectangle(x, y, width, height);
        } catch (Throwable t) {
            LOGGER.warn("[ime] setCursorRectangle threw: {}", t.toString());
        }
    }

    /**
     * 通知 backend 用户在 focused field 删除了周围字符 — 让 IME
     * 同步预编辑上下文。
     */
    public void notifyDeleteSurrounding(int before, int after) {
        ImeBackend.ImeSession s = currentSession.get();
        if (s == null) return;
        try {
            s.deleteSurrounding(before, after);
        } catch (Throwable t) {
            LOGGER.debug("[ime] deleteSurrounding threw: {}", t.toString());
        }
    }

    public void notifySurroundingText(String text, int cursor, int anchor) {
        ImeBackend.ImeSession s = currentSession.get();
        if (s == null) return;
        try {
            s.setSurroundingText(text, cursor, anchor);
        } catch (Throwable t) {
            LOGGER.debug("[ime] setSurroundingText threw: {}", t.toString());
        }
    }

    // --- 状态访问（renderer 用） ---

    public PreeditState preedit() { return preedit; }
    public List<String> candidates() { return candidates; }
    public int candidateCursor() { return candidateCursor; }
    public int candidatePageSize() { return candidatePageSize; }

    /**
     * 预编辑状态 — text + cursor 范围。
     * 整段重发：dispatcher 不做 diff（MC {@code PreeditEvent} 接收整段）。
     */
    public record PreeditState(String text, int cursorBegin, int cursorEnd) {
        public static final PreeditState EMPTY = new PreeditState("", 0, 0);
        public boolean isEmpty() { return text == null || text.isEmpty(); }
    }

    /**
     * Focused text field 抽象 — MC {@code EditBox} / {@code MultiLineEditBox}
     * / nested wayland app 各自实现。
     */
    public interface FocusedTextField {
        String componentId();

        /** 把 IME 提交的字符注入 field 当前位置 — mod 主线程。 */
        void insertText(String text);

        /** 删除 preedit（用户敲 ESC / 选词后 backend 触发）。 */
        void clearPreedit();
    }

    /**
     * 把 backend 回调（preedit / commit / candidates）转发给
     * {@link McImeIntegration} — 真正的 MC PreeditEvent 注入在
     * {@code McImeIntegration} 实现。
     */
    private final class RelayListener implements ImeBackend.ImeSession.Listener {
        private final ImeBackend.ImeSession session;

        RelayListener(ImeBackend.ImeSession session) {
            this.session = session;
        }

        @Override
        public void onPreeditChanged(String text, int cursorBegin, int cursorEnd) {
            McImeIntegration.runOnRenderThread(() -> {
                preedit = text == null || text.isEmpty()
                        ? PreeditState.EMPTY
                        : new PreeditState(text, cursorBegin, cursorEnd);
                McImeIntegration.updatePreedit(preedit);
            });
        }

        @Override
        public void onCommit(String text) {
            if (text == null || text.isEmpty()) return;
            McImeIntegration.runOnRenderThread(() -> {
                McImeIntegration.injectCommit(text);
                preedit = PreeditState.EMPTY;
            });
        }

        @Override
        public void onCandidatesChanged(List<String> cands, int cursor, int pageSize) {
            McImeIntegration.runOnRenderThread(() -> {
                candidates = cands == null ? List.of() : List.copyOf(cands);
                candidateCursor = cursor;
                candidatePageSize = pageSize;
                McImeIntegration.updateCandidates(candidates, cursor, pageSize);
            });
        }

        @Override
        public void onModifiersChanged(int modifiers) {
            McImeIntegration.runOnRenderThread(() ->
                    McImeIntegration.onModifiersChanged(modifiers));
        }

        @Override
        public void onReset() {
            McImeIntegration.runOnRenderThread(() -> {
                preedit = PreeditState.EMPTY;
                candidates = List.of();
                McImeIntegration.clearPreedit();
            });
        }
    }
}
