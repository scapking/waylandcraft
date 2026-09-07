package dev.evvie.waylandcraft.capture.fallback;

/**
 * Opaque handle returned by a successful {@code CreateSession} call against
 * {@code org.freedesktop.portal.ScreenCast}.
 *
 * <p>The handle is the session object path handed back by the portal, e.g.
 * {@code /org/freedesktop/portal/desktop/session/1_109/wcs1}. It must be
 * passed to subsequent {@code SelectSources} / {@code Start} calls and is
 * released by {@link WaylandPortalClient#closeCurrentSession()}.
 *
 * <p>This class lives in the {@code capture.fallback} package because it is
 * only used by the dbus-send based fallback path that does not depend on
 * the native PipeWire bridge. The primary {@link
 * dev.evvie.waylandcraft.capture.PipeWireCaptureManager} talks to
 * {@code libwaylandcraft} via JNI and does not need this type.
 */
public record PortalSessionHandle(String path) {
    public PortalSessionHandle {
        if (path == null || path.isEmpty()) {
            throw new IllegalArgumentException("portal session path is empty");
        }
    }
}