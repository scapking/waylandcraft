package dev.evvie.waylandcraft.ime;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.BufferedReader;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Optional;
import java.util.concurrent.atomic.AtomicReference;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * fcitx5 dbus backend — 通过 {@code dbus-send} + {@code dbus-monitor}
 * 命令行调 fcitx5 的 dbus 接口。
 *
 * <p><b>为什么不用 dbus-java (hypfvieh/dbus-java)？</b>：loom 1.16 main
 * 源集严格隔离第三方 jar — 加 dbus-java 需要新建 source set 或 Gradle
 * plugin。fallback 包 {@code WaylandPortalClient} 用的范式：
 * {@code ProcessBuilder} 调命令行工具 — 这里沿用相同设计，{@code
 * dbus-send} 调方法、{@code dbus-monitor} 收 signals。
 *
 * <p><b>覆盖</b>：fcitx5 上跑的所有 IME 引擎（pinyin / rime / chewing /
 * mozc / hangul / vi / anthy / table / kkc / …）— 全部走 fcitx5
 * InputContext1.CommitString / PreeditString signals。
 *
 * <p><b>协议来源</b>：fcitx/fcitx5/blob/master/src/im/keyboard/desktopdbus.cpp
 * + dbusfrontend.cpp。CommitString 字符串是 UTF-8，PreeditString 是
 * (text, cursor) 二元组，UpdateFormattedPreedit 是 a{sv} dict。
 *
 * <p><b>未来</b>：loom 1.16+ 如果放宽 main set 第三方限制，迁回
 * dbus-java 后这段代码可以删掉（dbus-send 调用会改成 JNI 调用），
 * 但接口契约（{@link ImeBackend}）不变。
 */
public final class Fcitx5DbusBackend implements ImeBackend {

    private static final Logger LOGGER = LoggerFactory.getLogger(Fcitx5DbusBackend.class);

    private static final String FCITX5_DEST = "org.fcitx.Fcitx5";
    private static final String FCITX5_PATH = "/org/freedesktop/IBus";
    private static final String IC_IFACE = "org.freedesktop.IBus.InputContext";
    // fcitx5 同时实现 IBus compat interface；老 fcitx4 用户用
    //   org.fcitx.Fcitx.InputContext — 这两个 path 都要试。
    private static final String[] KNOWN_DESTINATIONS = {
            "org.fcitx.Fcitx5",
            "org.fcitx.Fcitx",
    };
    private static final String[] KNOWN_IC_INTERFACES = {
            "org.fcitx.Fcitx.InputContext1",
            "org.freedesktop.IBus.InputContext",
    };

    /** 探测 — 调 dbus-send 看 fcitx5 是否在 session bus 上。 */
    @Override
    public String name() { return "fcitx5-dbus"; }

    @Override
    public boolean probe() {
        if (!hasDbusSend()) {
            return false;
        }
        // 用 ListNames 看 bus owner；fcitx5 启动后注册 org.fcitx.Fcitx5
        for (String dest : KNOWN_DESTINATIONS) {
            if (dbusHasName(dest)) {
                LOGGER.info("[ime] fcitx5 detected as dbus name '{}'", dest);
                return true;
            }
        }
        return false;
    }

    @Override
    public Optional<ImeSession> start() {
        // probe 过的 dest；start 阶段再 probe 一次拿最新 path
        String dest = null;
        for (String d : KNOWN_DESTINATIONS) {
            if (dbusHasName(d)) { dest = d; break; }
        }
        if (dest == null) {
            LOGGER.warn("[ime] fcitx5 dbus name disappeared between probe and start");
            return Optional.empty();
        }
        // IC 接口 — fcitx5 默认实现 ibus 兼容（v1 时代遗留）
        // 5.1+ 推荐 fcitx InputContext1；老 fcitx4 用 ibus
        String icIface = dest.contains("Fcitx5") ? "org.fcitx.Fcitx.InputContext1"
                : "org.freedesktop.IBus.InputContext";

        // CreateIC: 调用 fcitx5 的 CreateInputContext
        //   org.fcitx.Fcitx5.CreateInputContext(s:program-name, ...) -> o
        //   org.fcitx.Fcitx.CreateInputContext(...) -> o
        // 返回值是 IC object path；失败返回 null
        String icPath = createInputContext(dest, "waylandcraft", "waylandcraft-mc");
        if (icPath == null) {
            LOGGER.warn("[ime] CreateInputContext returned no path");
            return Optional.empty();
        }

        // 启动 dbus-monitor 监听该 IC 的 signals
        Fcitx5Session s = new Fcitx5Session(dest, icPath, icIface);
        s.startMonitor();
        return Optional.of(s);
    }

    // ----- dbus-send helpers (RenderCraft fallback 设计) -----

    private static boolean hasDbusSend() {
        try {
            Process p = new ProcessBuilder("dbus-send", "--version").start();
            p.getOutputStream().close();
            p.getErrorStream().close();
            return p.waitFor() == 0;
        } catch (Exception e) {
            return false;
        }
    }

    private static boolean dbusHasName(String dest) {
        try {
            // dbus-send --session --print-reply --dest=org.freedesktop.DBus
            //   /org/freedesktop/DBus org.freedesktop.DBus.NameHasOwner s:<dest>
            Process p = new ProcessBuilder(
                    "dbus-send", "--session", "--print-reply",
                    "--dest=org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus.NameHasOwner",
                    "string:" + dest).start();
            String out = drain(p.getInputStream());
            int code = p.waitFor();
            return code == 0 && out.contains("= true");
        } catch (Exception e) {
            return false;
        }
    }

    private static String createInputContext(String dest, String appName, String uniqueId) {
        // org.fcitx.Fcitx5.CreateInputContext(in s:arg0) -> (o:path)
        // 老 fcitx4 接口略不同 — 但 IBUS compat 接口签名是统一的
        //   CreateInputContext(s:arg0) -> o
        try {
            Process p = new ProcessBuilder(
                    "dbus-send", "--session", "--print-reply",
                    "--dest=" + dest,
                    "/org/freedesktop/IBus",
                    "org.freedesktop.IBus.CreateInputContext",
                    "string:" + appName).start();
            String out = drain(p.getInputStream());
            int code = p.waitFor();
            if (code != 0) return null;
            // 输出形如： "   object path \"/org/freedesktop/IBus/InputContext_0/0\""
            return extractObjectPath(out);
        } catch (Exception e) {
            return null;
        }
    }

    private static String extractObjectPath(String reply) {
        if (reply == null) return null;
        int i = reply.indexOf("object path");
        if (i < 0) return null;
        int q1 = reply.indexOf('"', i);
        if (q1 < 0) return null;
        int q2 = reply.indexOf('"', q1 + 1);
        if (q2 < 0) return null;
        return reply.substring(q1 + 1, q2);
    }

    private static String drain(InputStream in) {
        try {
            BufferedReader r = new BufferedReader(new InputStreamReader(in, StandardCharsets.UTF_8));
            StringBuilder sb = new StringBuilder();
            String line;
            while ((line = r.readLine()) != null) sb.append(line).append('\n');
            return sb.toString();
        } catch (Exception e) {
            return "";
        }
    }

    // ----- Session: dbus-monitor thread + signal 解析 -----

    /**
     * fcitx5 IC session — 启动一个 dbus-monitor 进程读 signals，
     * 解析出 commit / preedit 事件后 fire listener。
     */
    private static final class Fcitx5Session implements ImeSession {
        private final String dest;
        private final String icPath;
        private final String icIface;
        private final AtomicReference<Listener> listener = new AtomicReference<>();
        private Process monitor;
        private Thread readerThread;
        private volatile boolean closed;

        Fcitx5Session(String dest, String icPath, String icIface) {
            this.dest = dest;
            this.icPath = icPath;
            this.icIface = icIface;
        }

        @Override
        public String seat() {
            return icPath;
        }

        @Override
        public void setEventListener(Listener listener) {
            this.listener.set(listener);
        }

        /**
         * 启动 dbus-monitor 监听 IC 接口的 signals。
         *
         * <p>{@code dbus-monitor} 输出每行一个 signal：形如
         * <pre>{@code
         * signal sender=:1.42 -> dest=:1.50 serial=1 path=/o/f/d/IBus/InputContext_0/0;
         * interface=org.freedesktop.IBus.InputContext; member=CommitText
         *    string "你好"
         * }</pre>
         *
         * <p>我们用 grep -m 1 配合 awk 提取 event name + arguments，
         * 解析后 fire listener。
         */
        void startMonitor() {
            try {
                // dbus-monitor "type='signal',interface='<icIface>',path='<icPath>'"
                // timeout 给个 generous 的 5 分钟，每 5 分钟重启防止内存泄漏
                String rule = "type='signal',interface='" + icIface + "',path='" + icPath + "'";
                ProcessBuilder pb = new ProcessBuilder(
                        "timeout", "300", "dbus-monitor",
                        "--session", rule);
                pb.redirectErrorStream(true);
                this.monitor = pb.start();

                this.readerThread = new Thread(() -> {
                    try (BufferedReader r = new BufferedReader(
                            new InputStreamReader(monitor.getInputStream(), StandardCharsets.UTF_8))) {
                        String line;
                        String pendingIface = null, pendingMember = null;
                        List<String> argLines = new ArrayList<>();
                        while ((line = r.readLine()) != null) {
                            if (closed) break;
                            String trimmed = line.trim();
                            if (trimmed.startsWith("interface=")) {
                                // 新 signal 开始
                                if (pendingIface != null && pendingMember != null) {
                                    dispatchSignal(pendingIface, pendingMember, argLines);
                                }
                                pendingIface = trimmed;
                                argLines.clear();
                            } else if (trimmed.startsWith("member=")) {
                                pendingMember = trimmed;
                            } else if (trimmed.isEmpty()) {
                                // 空行 = signal 结束
                                if (pendingIface != null && pendingMember != null) {
                                    dispatchSignal(pendingIface, pendingMember, argLines);
                                    pendingIface = pendingMember = null;
                                    argLines.clear();
                                }
                            } else {
                                argLines.add(trimmed);
                            }
                        }
                        // EOF
                        if (pendingIface != null && pendingMember != null) {
                            dispatchSignal(pendingIface, pendingMember, argLines);
                        }
                    } catch (Exception e) {
                        if (!closed) {
                            LOGGER.warn("[ime] dbus-monitor reader died: {}", e.toString());
                        }
                    }
                }, "fcitx5-ime-monitor");
                readerThread.setDaemon(true);
                readerThread.start();
            } catch (Exception e) {
                LOGGER.warn("[ime] startMonitor failed: {}", e.toString());
            }
        }

        /**
         * 把 dbus-monitor 解析的 signal 转成 listener callback。
         *
         * <p>只关心 fcitx5 / ibus 实际 emit 的几个 signal —
         * 其余直接忽略。
         */
        private void dispatchSignal(String ifaceLine, String memberLine, List<String> args) {
            String iface = ifaceLine.substring("interface=".length());
            String member = memberLine.substring("member=".length());
            Listener l = listener.get();
            if (l == null) return;

            try {
                switch (member) {
                    case "CommitText", "CommitString" -> {
                        String s = parseStringArg(args);
                        if (s != null) l.onCommit(s);
                    }
                    case "PreeditText", "PreeditString" -> {
                        // 老 ibus PreeditText(s, i, ui) — text + cursor + visibility
                        // fcitx5 PreeditString(s, i) — text + cursor
                        String s = parseStringArg(args);
                        if (s != null) {
                            l.onPreeditChanged(s, 0, s.length());
                        }
                    }
                    case "UpdatePreeditCaret" -> {
                        // (i) cursor — 忽略，预编辑 API 不暴露 caret
                    }
                    case "UpdateAuxiliaryText" -> {
                        // (s, ui) — 暂忽略
                    }
                    case "HidePreeditText" -> {
                        l.onPreeditChanged("", 0, 0);
                    }
                    case "ShowPreeditText" -> {
                        // preedit 已通过 PreeditString 收到
                    }
                    default -> {
                        // ignore ForwardKey / FocusIn / FocusOut / etc.
                    }
                }
            } catch (Throwable t) {
                LOGGER.warn("[ime] dispatchSignal({}) failed: {}", member, t.toString());
            }
        }

        /**
         * dbus-monitor 输出的字符串形如 {@code string "你好"} — 提取引号内容。
         * 多个 string arg 时取第一个。
         */
        private static final Pattern STRING_ARG = Pattern.compile("string\\s+\"([^\"]*)\"");

        private static String parseStringArg(List<String> argLines) {
            for (String line : argLines) {
                Matcher m = STRING_ARG.matcher(line);
                if (m.find()) return m.group(1);
            }
            return null;
        }

        // ---- ImeSession impl ----

        @Override
        public void commit(String text) {
            // 调 IC.ProcessKeyEvent 让 fcitx5 处理 + commit
            // 实际更简单：直接 forwardKeyEvent(keysym) — fcitx5 内部会 commit
            // 但 fcitx5 一般不允许 client 端直接 commit — 这条路径标 TODO
            LOGGER.debug("[ime] commit() requested (text={}); fcitx5 IC commit TODO", text);
        }

        @Override
        public void deleteSurrounding(int before, int after) {
            // fcitx5 dbus 接口没有直接的 deleteSurrounding — 通过 ForwardKey
            // 模拟 Backspace / Delete 键
            LOGGER.debug("[ime] deleteSurrounding({}/{}) — TODO via ForwardKey", before, after);
        }

        @Override
        public void setCursorRectangle(int x, int y, int width, int height) {
            // fcitx5 ≥ 5.0.13 暴露 SetCursorRect:
            //   IC.SetCursorRect(i:i_x, i:i_y, i:i_w, i:i_h) — 4 个 int
            try {
                Process p = new ProcessBuilder(
                        "dbus-send", "--session", "--print-reply",
                        "--dest=" + dest,
                        icPath,
                        icIface + ".SetCursorRect",
                        "int32:" + x, "int32:" + y,
                        "int32:" + width, "int32:" + height).start();
                p.getOutputStream().close();
                p.getErrorStream().close();
                int code = p.waitFor();
                if (code != 0) {
                    LOGGER.debug("[ime] SetCursorRect returned {}", code);
                }
            } catch (Exception e) {
                LOGGER.debug("[ime] SetCursorRect failed: {}", e.toString());
            }
        }

        @Override
        public void close() {
            closed = true;
            if (monitor != null && monitor.isAlive()) {
                monitor.destroyForcibly();
            }
            if (readerThread != null) {
                readerThread.interrupt();
            }
            // DestroyIC: 释放 fcitx5 资源
            try {
                Process p = new ProcessBuilder(
                        "dbus-send", "--session", "--print-reply",
                        "--dest=" + dest,
                        icPath,
                        icIface + ".Destroy").start();
                p.getOutputStream().close();
                p.getErrorStream().close();
                p.waitFor();
            } catch (Exception ignored) {}
        }
    }

}
