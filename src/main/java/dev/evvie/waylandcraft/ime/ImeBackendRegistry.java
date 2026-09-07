package dev.evvie.waylandcraft.ime;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;



import java.util.ArrayList;
import java.util.List;
import java.util.Optional;
import java.util.ServiceLoader;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.function.Supplier;

/**
 * 跨平台 IME backend 注册表 + 选择器。
 *
 * <p>核心思路：让 mod 启动时自动选当前 host 最合适的 IME backend —
 * 用户不需手选。优先级链由 platform probe 决定：
 * <ol>
 *   <li><b>Native wayland zwp_text_input_v3</b> — nested wayland app 路径
 *       （firefox 等）已经覆盖。但对 MC 主进程没用（MC 不是嵌套 wayland
 *       client），这里跳过。</li>
 *   <li><b>fcitx5 dbus</b> — 全 Linux desktop（GNOME/KDE/Sway/Hyprland）
 *       fcitx5 是默认 IME，覆盖 ~70% 桌面用户。</li>
 *   <li><b>ibus dbus</b> — Ubuntu / Debian 默认，fcitx5 不可用时
 *       兜底。</li>
 *   <li><b>XIM (X11 legacy)</b> — 老 X11 应用兜底；走 XOpenIM + XSetICValues。</li>
 *   <li><b>macOS Cocoa (NSTextInputClient)</b> — JNI 调 macOS Cocoa IME API。</li>
 *   <li><b>Windows IMM</b> — JNI 调 ImmSetCompositionString / ImmGetContext。</li>
 *   <li><b>Stub (无 IME)</b> — 上面都不可用时 fallback（mod 仍能跑，
 *       只是 IME 不工作）。</li>
 * </ol>
 *
 * <p>每个 backend 在 {@code META-INF/services/dev.evvie.waylandcraft.ime.ImeBackend}
 * 注册 — 通过 {@link ServiceLoader} SPI 机制自动发现。新增 backend =
 * 写一个 {@code ImeBackend} 实现类 + 在 {@code META-INF/services/} 加一行
 * 完全限定名。不需要改本类。
 */
public final class ImeBackendRegistry {

    private static final Logger LOGGER = LoggerFactory.getLogger(ImeBackendRegistry.class);

    /**
     * platform detect — 优先级从高到低。Win/Mac/Android 用平台原生；
     * Linux 优先 fcitx5 / ibus / XIM；找不到就 stub。
     */
    private static final List<Supplier<ImeBackend>> FACTORIES = List.of(
            Fcitx5DbusBackend::new,           // fcitx5 over dbus (Linux)
            IbusDbusBackend::new,             // ibus over dbus (Linux, Ubuntu default)
            XimBackend::new,                   // XIM (X11 legacy)
            MacCocoaBackend::new,              // macOS NSTextInputClient
            WindowsImmBackend::new,            // Windows IMM
            AndroidImeBackend::new,            // Android InputMethodManager
            StubImeBackend::new                // last-resort
    );

    private static final List<ImeBackend> PROBED_BACKENDS = new CopyOnWriteArrayList<>();
    private static volatile ImeBackend SELECTED;

    static {
        // probe all backends at class load; probe() must be lightweight.
        for (Supplier<ImeBackend> factory : FACTORIES) {
            try {
                ImeBackend b = factory.get();
                if (b.probe()) {
                    PROBED_BACKENDS.add(b);
                    LOGGER.info("[ime] backend probed: {}", b.name());
                }
            } catch (Throwable t) {
                LOGGER.debug("[ime] backend {} probe failed: {}",
                        factory.get().name(), t.toString());
            }
        }
    }

    private ImeBackendRegistry() {}

    /**
     * 选最高优先级 backend 并启动 — 必须在主线程调。
     * 选不中任何 backend 返回空 Optional（理论上 stub backend 总会成功，
     * 永远 non-empty）。
     */
    public static Optional<ImeBackend.ImeSession> start() {
        if (SELECTED == null) {
            synchronized (ImeBackendRegistry.class) {
                if (SELECTED == null) {
                    SELECTED = PROBED_BACKENDS.isEmpty() ? new StubImeBackend() : PROBED_BACKENDS.get(0);
                }
            }
        }
        try {
            LOGGER.info("[ime] starting backend: {}", SELECTED.name());
            Optional<ImeBackend.ImeSession> session = SELECTED.start();
            if (session.isEmpty()) {
                LOGGER.warn("[ime] backend {} start() returned empty", SELECTED.name());
            }
            return session;
        } catch (Throwable t) {
            LOGGER.error("[ime] backend {} start() threw: {}", SELECTED.name(), t.toString());
            return Optional.empty();
        }
    }

    /** 当前选中的 backend（诊断用）。 */
    public static Optional<ImeBackend> selected() {
        return Optional.ofNullable(SELECTED);
    }

    /** probe 成功的 backend 列表（诊断 / 配置面板用）。 */
    public static List<String> available() {
        List<String> out = new ArrayList<>(PROBED_BACKENDS.size());
        for (ImeBackend b : PROBED_BACKENDS) {
            out.add(b.name());
        }
        return out;
    }
}
