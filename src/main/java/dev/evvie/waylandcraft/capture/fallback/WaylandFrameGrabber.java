package dev.evvie.waylandcraft.capture.fallback;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.TimeUnit;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * Pure-Java PPM (P6) parser and a {@code grim} shell-out helper.
 *
 * <p>This is the "smallest possible" Wayland capture path: {@code grim} speaks
 * the {@code wlr-screencopy} protocol directly and dumps the captured frame
 * as PPM, so a single external binary plus this parser replaces the entire
 * libpipewire consumer side of {@code PipeWireCaptureManager} / native
 * {@code portal_capture.rs}.
 *
 * <p>Trade-off vs. the primary native path:
 * <ul>
 *   <li>Pros — no libpipewire runtime dependency, no native bridge, runs
 *       anywhere {@code grim} is on {@code PATH} (Sway, Hyprland, wlroots-based
 *       compositors).</li>
 *   <li>Cons — can only grab the focused output (or a region), not a
 *       specific window that the portal session selected. For window-level
 *       capture the primary native path is still required.</li>
 * </ul>
 *
 * <p>The PPM/P6 format is parsed here in pure Java:
 * <pre>
 *   P6
 *   &lt;W&gt; &lt;H&gt;
 *   255
 *   &lt;RGB binary data...&gt;
 * </pre>
 * The output is the top-down RGBA8 array Minecraft textures want, with
 * alpha forced to {@code 0xFF}.
 *
 * <p>Migrated from RenderCraft (Sept 2026) — the original
 * {@code dev.scapking.rendcraft.protocol.wayland.WaylandFrameGrabber} used
 * {@code org.slf4j.LoggerFactory} directly with no class-specific logger;
 * this port uses {@link WaylandCraftCommon#LOGGER} so it integrates with the
 * mod-wide logging configuration.
 */
public class WaylandFrameGrabber {
    private static final Logger LOGGER = LoggerFactory.getLogger(WaylandFrameGrabber.class);

    public static class GrabFailed extends Exception {
        public GrabFailed(String message) { super(message); }
        public GrabFailed(String message, Throwable cause) { super(message, cause); }
    }

    /** Result of a successful grab. */
    public record Frame(int width, int height, byte[] rgba) {}

    /**
     * Try to grab the focused output with {@code grim}. Returns {@code null}
     * when {@code grim} is not on {@code PATH} (the caller can then fall
     * back to a no-frame stub or surface a "Wayland capture unavailable"
     * status to the user).
     */
    public Frame tryGrabWithGrim() throws GrabFailed {
        if (!toolAvailable("grim")) {
            return null;
        }
        ProcessBuilder pb = new ProcessBuilder("grim", "-t", "ppm", "-");
        pb.redirectErrorStream(true);
        Process process = null;
        try {
            process = pb.start();
            byte[] stdout = drain(process.getInputStream());
            if (!process.waitFor(5, TimeUnit.SECONDS)) {
                process.destroyForcibly();
                throw new GrabFailed("grim timed out after 5s");
            }
            int code = process.exitValue();
            if (code != 0) {
                throw new GrabFailed("grim exited with code " + code + ": "
                        + new String(stdout, StandardCharsets.UTF_8));
            }
            return parsePpm(stdout);
        } catch (IOException e) {
            throw new GrabFailed("Failed to run grim: " + e.getMessage(), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new GrabFailed("Interrupted while running grim", e);
        } finally {
            if (process != null && process.isAlive()) {
                process.destroyForcibly();
            }
        }
    }

    /**
     * Probe {@code PATH} for a binary without throwing. A failed probe is
     * the normal "fall back" signal — log at debug and let the caller
     * decide whether to surface the absence to the user.
     */
    private static boolean toolAvailable(String name) {
        try {
            Process p = new ProcessBuilder("which", name).start();
            p.getOutputStream().close();
            p.getErrorStream().close();
            return p.waitFor() == 0;
        } catch (Exception e) {
            LOGGER.debug("which {} failed: {}", name, e.getMessage());
            return false;
        }
    }

    /**
     * Parse a binary PPM (P6) image into a top-down RGBA8 array.
     *
     * <p>The header is three lines
     * <pre>
     *   P6
     *   &lt;W&gt; &lt;H&gt;
     *   255
     * </pre>
     * followed immediately by {@code W*H*3} bytes of interleaved RGB.
     * Comments ({@code #} to end of line) are honoured before the raster
     * but not on data lines; this is enough for grim's output.
     */
    public static Frame parsePpm(byte[] data) throws GrabFailed {
        int idx = 0;
        // Read magic "P6"
        if (idx + 2 > data.length || data[idx] != 'P' || data[idx + 1] != '6') {
            throw new GrabFailed("Not a P6 PPM (no magic)");
        }
        idx = skipWhitespaceAndComments(data, idx + 2);
        int width = readInt(data, idx);
        idx = skipWhitespace(data, widthEnd(data, idx));
        int height = readInt(data, idx);
        idx = skipWhitespace(data, heightEnd(data, idx));
        int maxval = readInt(data, idx);
        idx = skipWhitespace(data, maxvalEnd(data, idx));
        if (maxval != 255) {
            throw new GrabFailed("PPM maxval=" + maxval + " not supported (expected 255)");
        }
        // Single whitespace separator before raster
        if (idx < data.length && (data[idx] == ' ' || data[idx] == '\n' || data[idx] == '\r' || data[idx] == '\t')) {
            idx++;
        }
        int rasterStart = idx;
        int rasterLen = data.length - rasterStart;
        int expected = width * height * 3;
        if (rasterLen < expected) {
            throw new GrabFailed("PPM truncated: have " + rasterLen + " bytes, expected " + expected);
        }
        byte[] rgba = new byte[width * height * 4];
        int src = rasterStart;
        int dst = 0;
        for (int i = 0; i < width * height; i++) {
            rgba[dst++] = data[src];
            rgba[dst++] = data[src + 1];
            rgba[dst++] = data[src + 2];
            rgba[dst++] = (byte) 0xFF;
            src += 3;
        }
        return new Frame(width, height, rgba);
    }

    private static int skipWhitespaceAndComments(byte[] data, int idx) {
        while (idx < data.length) {
            byte c = data[idx];
            if (c == ' ' || c == '\n' || c == '\r' || c == '\t') {
                idx++;
            } else if (c == '#') {
                // skip to end of line
                while (idx < data.length && data[idx] != '\n') {
                    idx++;
                }
            } else {
                return idx;
            }
        }
        return idx;
    }

    private static int skipWhitespace(byte[] data, int idx) {
        while (idx < data.length) {
            byte c = data[idx];
            if (c == ' ' || c == '\n' || c == '\r' || c == '\t') {
                idx++;
            } else {
                return idx;
            }
        }
        return idx;
    }

    private static int readInt(byte[] data, int idx) {
        int start = idx;
        while (idx < data.length && data[idx] >= '0' && data[idx] <= '9') {
            idx++;
        }
        int n = 0;
        for (int i = start; i < idx; i++) {
            n = n * 10 + (data[i] - '0');
        }
        return n;
    }

    private static int widthEnd(byte[] data, int idx) {
        while (idx < data.length && data[idx] >= '0' && data[idx] <= '9') {
            idx++;
        }
        return idx;
    }

    private static int heightEnd(byte[] data, int idx) {
        while (idx < data.length && data[idx] >= '0' && data[idx] <= '9') {
            idx++;
        }
        return idx;
    }

    private static int maxvalEnd(byte[] data, int idx) {
        while (idx < data.length && data[idx] >= '0' && data[idx] <= '9') {
            idx++;
        }
        return idx;
    }

    private static byte[] drain(InputStream in) throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream(1 << 20);
        byte[] buf = new byte[8192];
        int n;
        while ((n = in.read(buf)) > 0) {
            out.write(buf, 0, n);
        }
        return out.toByteArray();
    }
}