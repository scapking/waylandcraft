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
 * ibus dbus backend — Ubuntu / Debian 默认 IME 框架（ibus-daemon）。
 *
 * <p>与 {@link Fcitx5DbusBackend} 同构：dbus-send 调方法、dbus-monitor 收
 * signals。ibus-daemon 在 session bus 注册 {@code org.freedesktop.IBus}，
 * 通过 {@code org.freedesktop.IBus.InputContext} 暴露输入会话。
 *
 * <p><b>wire 说明（对照 native host_bridge/dbus_ibus.rs）</b>：
 * <ul>
 *   <li>CreateInputContext(s:client_name) — native 用 portal
 *       ({@code org.freedesktop.portal.IBus}) 单参；老 ibus-daemon 直连
 *       接口同名单参，fcitx5 的 ibus 兼容面也是单参 — 统一试单参。</li>
 *   <li>CommitText 信号 body 是 IBusText variant — dbus-monitor 文本模式
 *       显示为 {@code variant string "..."}，{@link #parseStringArg}
 *       的子串匹配能抓到。</li>
 *   <li>UpdatePreeditText(s, cursor, visible) / HidePreeditText /
 *       UpdateLookupTable — 与 Fcitx5DbusBackend 处理的信号集一致。</li>
 * </ul>
 *
 * <p><b>状态机</b>：{@link #focusIn()} / {@link #focusOut()} 通知 ibus-daemon
 * 激活/停用本 IC（ibus 只有 FocusIn 后才把按键交给 IME 引擎）。
 *
 * <p>本类覆盖：ibus-pinyin / ibus-rime / ibus-hangul / ibus-mozc /
 * ibus-anthy 等全部跑在 ibus-daemon 上的引擎。
 */
public final class IbusDbusBackend implements ImeBackend {

    private static final Logger LOGGER = LoggerFactory.getLogger(IbusDbusBackend.class);

    /** ibus-daemon 的 session bus 名字（老直连接口；非 portal）。 */
    private static final String IBUS_DEST = "org.freedesktop.IBus";
    private static final String IBUS_PATH = "/org/freedesktop/IBus";
    private static final String IC_IFACE = "org.freedesktop.IBus.InputContext";

    @Override
    public String name() {
        return "ibus-dbus";
    }

    @Override
    public boolean probe() {
        if (!hasDbusSend()) {
            return false;
        }
        // 只认真正的 ibus-daemon：fcitx5 也实现 ibus 兼容接口但它注册的是
        // org.fcitx.Fcitx5，不会占用 org.freedesktop.IBus。若两个都在，
        // fcitx5 backend 优先级更高（Registry 顺序），这里不会先被选中。
        return dbusHasName(IBUS_DEST);
    }

    @Override
    public Optional<ImeSession> start() {
        String icPath = createInputContext(IBUS_DEST, "waylandcraft");
        if (icPath == null) {
            LOGGER.warn("[ime] ibus CreateInputContext returned no path");
            return Optional.empty();
        }
        IbusSession s = new IbusSession(IBUS_DEST, icPath, IC_IFACE);
        s.startMonitor();
        return Optional.of(s);
    }

    // ----- dbus helpers（与 Fcitx5DbusBackend 相同模式）-----

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

    private static String createInputContext(String dest, String appName) {
        try {
            // ibus-daemon 直连：CreateInputContext(s:client_name) -> o:path
            Process p = new ProcessBuilder(
                    "dbus-send", "--session", "--print-reply",
                    "--dest=" + dest,
                    IBUS_PATH,
                    "org.freedesktop.IBus.CreateInputContext",
                    "string:" + appName).start();
            String out = drain(p.getInputStream());
            int code = p.waitFor();
            if (code != 0) {
                LOGGER.debug("[ime] ibus CreateInputContext rc={} out={}", code, out.trim());
                return null;
            }
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

    // ----- Session: dbus-monitor + 解析（修复版，与 Fcitx5 同款）-----

    private static final class IbusSession implements ImeSession {
        private final String dest;
        private final String icPath;
        private final String icIface;
        private final AtomicReference<Listener> listener = new AtomicReference<>();
        private Process monitor;
        private Thread readerThread;
        private volatile boolean closed;

        IbusSession(String dest, String icPath, String icIface) {
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

        /** ibus IC 激活：FocusIn 后引擎才处理按键。 */
        @Override
        public void focusIn() {
            try {
                dbusCall(icIface + ".FocusIn").waitFor();
            } catch (Exception e) {
                LOGGER.debug("[ime] ibus FocusIn failed: {}", e.toString());
            }
        }

        /** ibus IC 停用。 */
        @Override
        public void focusOut() {
            try {
                dbusCall(icIface + ".FocusOut").waitFor();
            } catch (Exception e) {
                LOGGER.debug("[ime] ibus FocusOut failed: {}", e.toString());
            }
        }

        /** ibus 引擎在用户确认时自发 CommitText 信号上屏；这里只清残留 preedit。 */
        @Override
        public void commit(String text) {
            if (text == null || text.isEmpty()) return;
            try {
                dbusCall(icIface + ".Reset").waitFor();
            } catch (Exception e) {
                LOGGER.debug("[ime] ibus reset error: {}", e.toString());
            }
        }

        @Override
        public void deleteSurrounding(int before, int after) {
            if (before == 0 && after == 0) return;
            try {
                if (before > 0) {
                    dbusCall(icIface + ".DeleteSurroundingText",
                            "int32:" + (-before), "uint32:" + before).waitFor();
                }
                if (after > 0) {
                    dbusCall(icIface + ".DeleteSurroundingText",
                            "int32:0", "uint32:" + after).waitFor();
                }
            } catch (Exception e) {
                LOGGER.debug("[ime] ibus deleteSurrounding error: {}", e.toString());
            }
        }

        @Override
        public void setCursorRectangle(int x, int y, int width, int height) {
            // ibus 用 SetCursorLocationRelative(x, y, w, h)（相对 surface 左上）
            // 而不是 fcitx5 的 SetCursorRect。native dbus_ibus.rs 同此。
            try {
                Process p = new ProcessBuilder(
                        "dbus-send", "--session", "--print-reply",
                        "--dest=" + dest,
                        icPath,
                        icIface + ".SetCursorLocationRelative",
                        "int32:" + x, "int32:" + y,
                        "int32:" + width, "int32:" + height).start();
                p.getOutputStream().close();
                p.getErrorStream().close();
                p.waitFor();
            } catch (Exception e) {
                LOGGER.debug("[ime] ibus SetCursorLocationRelative failed: {}", e.toString());
            }
        }

        void startMonitor() {
            try {
                String rule = "type='signal',interface='" + icIface + "',path='" + icPath + "'";
                ProcessBuilder pb = new ProcessBuilder(
                        "timeout", "300", "dbus-monitor",
                        "--session", rule);
                pb.redirectErrorStream(true);
                this.monitor = pb.start();

                this.readerThread = new Thread(() -> {
                    try (BufferedReader r = new BufferedReader(
                            new InputStreamReader(monitor.getInputStream(), StandardCharsets.UTF_8))) {
                        // dbus-monitor 每 signal：header 单行（signal time=... path=...;
                        // interface=...; member=...）+ 参数行（缩进）+ 空行分隔。
                        Pattern header = Pattern.compile(
                                "signal .*?path=([^;]*); interface=([^;]*); member=(\\S+)");
                        String line;
                        String pendingIface = null, pendingMember = null;
                        boolean inSignal = false;
                        List<String> argLines = new ArrayList<>();
                        while ((line = r.readLine()) != null) {
                            if (closed) break;
                            String trimmed = line.trim();
                            if (trimmed.startsWith("signal ") || trimmed.startsWith("error ")) {
                                if (inSignal && pendingIface != null && pendingMember != null) {
                                    dispatchSignal(pendingIface, pendingMember, argLines);
                                }
                                Matcher m = header.matcher(trimmed);
                                if (m.matches()) {
                                    pendingIface = m.group(2);
                                    pendingMember = m.group(3);
                                    argLines.clear();
                                    inSignal = true;
                                } else {
                                    inSignal = false;
                                    pendingIface = pendingMember = null;
                                }
                            } else if (trimmed.isEmpty()) {
                                if (inSignal && pendingIface != null && pendingMember != null) {
                                    dispatchSignal(pendingIface, pendingMember, argLines);
                                    pendingIface = pendingMember = null;
                                    argLines.clear();
                                }
                                inSignal = false;
                            } else if (inSignal) {
                                argLines.add(trimmed);
                            }
                        }
                        if (inSignal && pendingIface != null && pendingMember != null) {
                            dispatchSignal(pendingIface, pendingMember, argLines);
                        }
                    } catch (Exception e) {
                        if (!closed) {
                            LOGGER.warn("[ime] ibus dbus-monitor reader died: {}", e.toString());
                        }
                    }
                }, "ibus-ime-monitor");
                readerThread.setDaemon(true);
                readerThread.start();
            } catch (Exception e) {
                LOGGER.warn("[ime] ibus startMonitor failed: {}", e.toString());
            }
        }

        private void dispatchSignal(String iface, String member, List<String> args) {
            Listener l = listener.get();
            if (l == null) return;
            try {
                switch (member) {
                    case "CommitText" -> {
                        String s = parseStringArg(args);
                        if (s != null) l.onCommit(s);
                    }
                    case "UpdatePreeditText", "UpdatePreeditTextWithMode", "PreeditText" -> {
                        // ibus: (text_variant, cursor_pos, visible) — cursor 是第 2 个 uint32
                        String s = parseStringArg(args);
                        int cursor = parseIntArg(args, 1);
                        if (s != null) {
                            int end = cursor > 0 ? cursor : s.length();
                            l.onPreeditChanged(s, 0, Math.max(0, end));
                        }
                    }
                    case "HidePreeditText" -> l.onPreeditChanged("", 0, 0);
                    case "ShowLookupTable", "UpdateLookupTable" -> {
                        // 候选表 wire 是 IBusLookupTable variant，文本模式解析不完整；
                        // 候选渲染本就不依赖候选表（MC 原生 IMEPreeditOverlay），跳过。
                    }
                    default -> {
                        // ignore FocusIn/FocusOut/PropertyActivate/其他
                    }
                }
            } catch (Throwable t) {
                LOGGER.warn("[ime] ibus dispatchSignal({}) failed: {}", member, t.toString());
            }
        }

        private static final Pattern STRING_ARG =
                Pattern.compile("(?:variant\\s+)?string\\s+\"([^\"]*)\"");
        private static final Pattern INT_ARG = Pattern.compile("uint32\\s+(\\d+)");

        private static String parseStringArg(List<String> argLines) {
            for (String line : argLines) {
                Matcher m = STRING_ARG.matcher(line);
                if (m.find()) return m.group(1);
            }
            return null;
        }

        private static int parseIntArg(List<String> argLines, int idx) {
            int seen = 0;
            for (String line : argLines) {
                Matcher m = INT_ARG.matcher(line);
                if (m.find()) {
                    if (seen == idx) {
                        try {
                            return Integer.parseInt(m.group(1));
                        } catch (NumberFormatException e) {
                            return 0;
                        }
                    }
                    seen++;
                }
            }
            return 0;
        }

        private Process dbusCall(String method, String... args) throws java.io.IOException {
            List<String> cmd = new ArrayList<>();
            cmd.add("dbus-send");
            cmd.add("--session");
            cmd.add("--print-reply");
            cmd.add("--dest=" + dest);
            cmd.add(icPath);
            cmd.add(method);
            for (String a : args) cmd.add(a);
            return new ProcessBuilder(cmd).start();
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
