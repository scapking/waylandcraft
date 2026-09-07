package dev.evvie.waylandcraft.ime;

import java.util.List;
import java.util.Optional;

/**
 * 通用 IME 后端接口 — 抽象所有输入法引擎（fcitx5 / ibus / kime / scim /
 * 任何 XIM / macOS Cocoa / Windows IMM）共有的"输入会话"概念。
 *
 * <p>设计目标：
 * <ul>
 *   <li>对上层（{@link ImeDispatcher}、MC {@code PreeditEvent} listener）
 *       暴露**协议无关**的输入会话 API — 上层不需要知道是 fcitx5 dbus 还是
 *       XIM 还是 native {@code zwp_text_input_v3}。</li>
 *   <li>每个 backend 只负责一件事：把当前 host 的 IME 事件
 *       (commit string / preedit text / candidates / cursor rect) 转成本接口
 *       的事件流。</li>
 *   <li>新增 IME 引擎支持 = 加一个 {@code ImeBackend} 实现 + 在
 *       {@link ImeBackendRegistry} 注册一行 — 不动 dispatcher / 渲染层。</li>
 * </ul>
 *
 * <p>事件模型（与 Wayland {@code zwp_text_input_v3} 协议对齐，但与具体
 * backend 解耦）：
 * <pre>{@code
 *   onFocusGained(seat)
 *     -> onPreeditChanged(text, cursorBegin, cursorEnd)
 *     -> onCommit(text)                    // 用户从候选中选了一个 / 直接 commit
 *     -> onCandidatesChanged(List<String>, cursor, pageSize)
 *     -> onModifersChanged(modifiers)
 *   onFocusLost(seat)
 * }</pre>
 *
 * <p>每个 backend 必须实现以下能力检测：{@link #probe()} 在 mod 启动时调一次，
 * 用于决定是否启用该 backend（fcitx5 dbus 接口存在 = probe 成功）。
 *
 * <p>命名约定：所有公开方法命名是动词或动词短语；状态变化走 callback
 * 注册（{@link #setEventListener}）。Backend 内部对 IME 引擎是 polling
 * 还是 event-driven 由实现决定 — 对外不暴露。
 *
 * <p>线程模型：所有 callback 在 mod 主线程（render thread）触发。
 * backend 内部用 worker thread 调 dbus / IPC，收到事件后用
 * {@code Minecraft.getInstance().execute(Runnable)} 切回主线程再 fire
 * listener callback。
 */
public interface ImeBackend {

    /** 后端身份 — 用于日志、诊断、配置面板。 */
    String name();

    /**
     * 启动探测 — mod 启动时调一次。返回 {@code true} 表示当前 host
     * 可用此 backend (fcitx5 dbus 接口存在 / XIM 服务可达 / 平台有 IMM
     * API)。
     *
     * <p>probe 阶段 <b>不能</b>启动后台线程或打开 dbus connection —
     * 只能做轻量级 introspection（检查 dbus name owner、读环境变量、
     * 调 {@code ImmGetContext} 占位等等）。
     */
    boolean probe();

    /**
     * 启动 backend — 建立 dbus connection / XIM 服务注册 / 平台 IME 句柄。
     * 失败时返回空 Optional。
     *
     * <p>必须在主线程调用。Backend 内部可启动 worker thread。
     */
    Optional<ImeSession> start();

    /**
     * ImeSession 是 per-seat 活动会话 — 在 {@code onFocusGained} 时 backend
     * 创建并返回给 dispatcher，dispatcher 用它订阅 commit / preedit 事件
     * 直到 {@code onFocusLost}。
     */
    interface ImeSession {

        /** 当前 focus 的 seat（screen / window）。 */
        String seat();

        /**
         * 注销 listener — dispatcher 在 {@code onFocusLost} 后调。
         * session 关闭后不再发任何 callback。
         */
        void close();

        /**
         * 提交一段预编辑文本（替代预编辑）— backend 把字符串送到 IME
         * commit 通道（XIM commit / dbus CommitString / NSTextInput insertText）。
         * 对 nested wayland 应用：通过现有 {@code host_bridge} → dbus-ibus 路径。
         */
        void commit(String text);

        /**
         * 删除预编辑光标周围若干字符 — 双向（begin > 0 删前面，end < 0
         * 删后面）— 等价于 Wayland {@code zwp_text_input_v3.delete_surrounding_text}。
         */
        void deleteSurrounding(int beforeLength, int afterLength);

        /**
         * 同步预编辑光标周围的文本（用于智能纠错 / 输入法上下文）。
         * 选填 — 不是所有 backend 都支持（XIM 没有；fcitx5 dbus 也没有；
         * macOS Cocoa 有；Wayland ti3 有）。
         */
        default void setSurroundingText(String text, int cursor, int anchor) {
            // no-op
        }

        /**
         * 设置预编辑光标屏幕坐标（X / Y / W / H）— 桌面候选窗的锚点。
         * 这是解决 v0.9.33 "候选窗漂移" 问题的核心 API：所有 backend
         * 都必须有意义地实现，不能 no-op。
         */
        void setCursorRectangle(int x, int y, int width, int height);

        /**
         * 注册事件 listener — 必须在 close() 之前调一次。
         */
        void setEventListener(Listener listener);

        /**
         * 事件 listener — 在主线程触发（backend 内部负责线程切换）。
         * 任一回调抛异常会被捕获并 log，不会污染 IME 状态机。
         */
        interface Listener {
            /** 预编辑变化 — 整段重发，dispatcher 负责 diff 后送到 MC PreeditEvent。 */
            void onPreeditChanged(String text, int cursorBegin, int cursorEnd);

            /**
             * 提交 — 用户从候选中选了一个词 / 直接敲了非 IME 字符。
             * 这是 <b>最终文本</b>，dispatcher 必须把它写入当前
             * focused EditBox / MultiLineEditBox。
             */
            void onCommit(String text);

            /**
             * 候选窗变化 — 当 IME 弹出候选（fcitx5 LookupTable / ibus
             * Engine.LookupTable / kime preedit page）。backend 把分页后
             * 当前页的候选列表传上来。
             *
             * <p>列表顺序 = 候选顺序；{@code cursor} = 当前高亮索引；
             * {@code pageSize} = 这一页的候选数（通常 5/9/10）。
             */
            void onCandidatesChanged(List<String> candidates, int cursor, int pageSize);

            /**
             * 修饰键状态 — Shift / Ctrl / Caps Lock 等（可选实现）。
             * dispatcher 用它过滤 IME 候选键 vs 游戏快捷键的冲突。
             */
            default void onModifiersChanged(int modifiers) {}

            /**
             * IME 引擎主动重置（fcitx5 重启、用户切到英文等）—
             * dispatcher 应该清空所有 preedit + candidates。
             */
            default void onReset() {}
        }
    }
}
