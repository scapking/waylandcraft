package dev.evvie.waylandcraft.capture.fallback;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicReference;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * Thin client for the {@code org.freedesktop.portal.ScreenCast} v6 D-Bus
 * interface, implemented on top of the {@code dbus-send} command-line tool.
 *
 * <p>The choice to shell out rather than embed a D-Bus library keeps
 * WaylandCraft's compile classpath free of {@code org.freedesktop.dbus.*};
 * adding dbus-java would require either a separate source set or a Gradle
 * plugin. {@code dbus-send} ships with any D-Bus-enabled desktop and is
 * therefore available whenever a portal is.
 *
 * <p>What this class does <b>not</b> do: read frames. The portal's
 * {@code OpenPipeWireRemote} returns a file descriptor to a PipeWire graph
 * that needs libpipewire to drive. The primary {@link
 * dev.evvie.waylandcraft.capture.PipeWireCaptureManager} handles that case
 * via the native {@code libwaylandcraft} bridge; this class is the
 * fallback path used when the native bridge is unavailable or when the
 * user is on a compositor where PipeWire is not reachable. In that case
 * {@link #captureFrame()} uses {@link WaylandFrameGrabber} (grim) as a
 * best-effort stand-in.
 *
 * <p>The class is best-effort about the portal being there. If no portal
 * is running, every call throws a {@link PortalException} with a
 * descriptive message; the caller should translate that into a
 * "Wayland capture unavailable on this host" status rather than
 * crashing the game.
 *
 * <p>Migrated from RenderCraft (Sept 2026) — the original
 * {@code dev.scapking.rendcraft.protocol.wayland.WaylandPortalClient} hid
 * itself behind reflection to keep dbus off the Minecraft main source
 * set; that is no longer needed in WaylandCraft (loom version and source
 * set layout differ), so this port exposes the class directly.
 */
public class WaylandPortalClient {
    private static final Logger LOGGER = LoggerFactory.getLogger(WaylandPortalClient.class);
    private static final String SCREENCAST_DEST = "org.freedesktop.portal.Desktop";
    private static final String SCREENCAST_PATH = "/org/freedesktop/portal/desktop";
    private static final String SCREENCAST_IFACE = "org.freedesktop.portal.ScreenCast";

    public static class PortalException extends Exception {
        public PortalException(String message) { super(message); }
        public PortalException(String message, Throwable cause) { super(message, cause); }
    }

    private final AtomicReference<PortalSessionHandle> currentSession = new AtomicReference<>();

    public WaylandPortalClient() throws PortalException {
        // Probe dbus-send; if it is not on PATH, fail fast so the
        // caller can fall back to the no-portal stub path.
        try {
            Process p = new ProcessBuilder("dbus-send", "--version").start();
            p.getOutputStream().close();
            p.getErrorStream().close();
            int code = p.waitFor();
            if (code != 0) {
                throw new PortalException("dbus-send exited with code " + code);
            }
            LOGGER.info("dbus-send probe OK; portal client ready");
        } catch (IOException e) {
            throw new PortalException("dbus-send not available on PATH: " + e.getMessage(), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new PortalException("Interrupted while probing dbus-send", e);
        }
    }

    /**
     * {@code CreateSession(a{sv}) -> o}. Returns the new session path.
     */
    public PortalSessionHandle createSession() throws PortalException {
        closeCurrentSession();
        String reply = runDbusSend(
                SCREENCAST_DEST,
                SCREENCAST_PATH,
                SCREENCAST_IFACE + ".CreateSession",
                "a{sv}",
                "handle_token:string:rendcraft-session");
        String path = extractObjectPath(reply);
        if (path == null) {
            throw new PortalException("CreateSession reply did not contain an object path: " + reply);
        }
        PortalSessionHandle handle = new PortalSessionHandle(path);
        currentSession.set(handle);
        LOGGER.info("ScreenCast session created: {}", path);
        return handle;
    }

    /**
     * {@code SelectSources(o, a{sv}) -> o}. Window source, hidden cursor.
     */
    public String selectSources(PortalSessionHandle session) throws PortalException {
        if (session == null) {
            throw new PortalException("selectSources: session is null");
        }
        return runDbusSend(
                SCREENCAST_DEST,
                session.path(),
                "org.freedesktop.portal.ScreenCast.SelectSources",
                "o,a{sv}",
                session.path(),
                "types:uint32:2",
                "multiple:boolean:false",
                "cursor_mode:uint32:1");
    }

    /**
     * {@code Start(o, s, a{sv}) -> o}. Empty parent window; the portal is
     * allowed to pick its own chooser dialog position.
     */
    public String start(PortalSessionHandle session) throws PortalException {
        if (session == null) {
            throw new PortalException("start: session is null");
        }
        return runDbusSend(
                SCREENCAST_DEST,
                session.path(),
                "org.freedesktop.portal.ScreenCast.Start",
                "o,s,a{sv}",
                session.path(),
                "",
                "");
    }

    /**
     * Releases the held session if any. The portal tears down the
     * underlying screencast once its refcount hits zero.
     */
    public void closeCurrentSession() {
        PortalSessionHandle held = currentSession.getAndSet(null);
        if (held == null) {
            return;
        }
        try {
            runDbusSend(
                    SCREENCAST_DEST,
                    held.path(),
                    "org.freedesktop.portal.Session.Close",
                    "");
            LOGGER.info("Closed ScreenCast session {}", held.path());
        } catch (PortalException e) {
            LOGGER.warn("Failed to close ScreenCast session {}: {}", held.path(), e.getMessage());
        }
    }

    /**
     * Grab the most recent compositor output as a stand-in for the
     * PipeWire-fed frame. The portal session has already been opened by
     * {@link #start(PortalSessionHandle)}, so we know we are on a Wayland
     * desktop with a portal; this just does the cheap path via
     * {@code grim -t ppm -}.
     */
    public WaylandFrameGrabber.Frame captureFrame() throws PortalException {
        WaylandFrameGrabber grabber = new WaylandFrameGrabber();
        WaylandFrameGrabber.Frame frame;
        try {
            frame = grabber.tryGrabWithGrim();
        } catch (WaylandFrameGrabber.GrabFailed e) {
            throw new PortalException("Frame grab failed: " + e.getMessage(), e);
        }
        if (frame == null) {
            throw new PortalException(
                    "Neither grim nor another screencopy tool is on PATH; "
                            + "install grim (or run on a wlroots-based desktop) "
                            + "to enable Wayland capture.");
        }
        return frame;
    }

    public void close() {
        closeCurrentSession();
    }

    /**
     * Invoke {@code dbus-send --session --print-reply ...} and capture its
     * standard output. dbus-send exits non-zero on protocol errors (e.g.
     * portal rejected the call), which we surface as a
     * {@link PortalException}.
     */
    private String runDbusSend(String dest, String path, String method, String signature, String... args)
            throws PortalException {
        ProcessBuilder pb = new ProcessBuilder(buildDbusSendArgs(dest, path, method, signature, args));
        pb.redirectErrorStream(true);
        Process process = null;
        try {
            process = pb.start();
            String output = drain(process.getInputStream());
            int code = process.waitFor();
            if (code != 0) {
                throw new PortalException("dbus-send " + method + " failed (code " + code + "): " + output);
            }
            return output;
        } catch (IOException e) {
            throw new PortalException("dbus-send " + method + " I/O failed: " + e.getMessage(), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new PortalException("Interrupted while running dbus-send " + method, e);
        } finally {
            if (process != null && process.isAlive()) {
                process.destroyForcibly();
            }
        }
    }

    private String[] buildDbusSendArgs(String dest, String path, String method,
                                       String signature, String[] args) {
        String[] base = new String[]{
                "dbus-send",
                "--session",
                "--print-reply",
                "--dest=" + dest,
                path,
                method
        };
        if (signature == null || signature.isEmpty()) {
            String[] result = new String[base.length];
            System.arraycopy(base, 0, result, 0, base.length);
            return result;
        }
        String[] result = new String[base.length + 1 + args.length];
        System.arraycopy(base, 0, result, 0, base.length);
        result[base.length] = signature;
        System.arraycopy(args, 0, result, base.length + 1, args.length);
        return result;
    }

    private static String drain(InputStream in) throws IOException {
        byte[] buf = new byte[4096];
        StringBuilder out = new StringBuilder();
        int n;
        while ((n = in.read(buf)) > 0) {
            out.append(new String(buf, 0, n, StandardCharsets.UTF_8));
        }
        return out.toString();
    }

    /**
     * dbus-send prints the reply on stdout in the form
     * {@code method return time=... sender=... -> reply_serial=...\n   object path "/foo/bar"}.
     * Pick out the quoted object path; null if not present.
     */
    private static String extractObjectPath(String reply) {
        if (reply == null) return null;
        int open = reply.indexOf("object path \"");
        if (open < 0) return null;
        int start = open + "object path \"".length();
        int close = reply.indexOf('"', start);
        if (close < 0) return null;
        return reply.substring(start, close);
    }
}