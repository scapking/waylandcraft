package dev.evvie.waylandcraft.ime;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.List;
import java.util.Optional;

/**
 * Stub backend — 所有平台原生 / dbus / XIM backend 都不可用时启用。
 *
 * <p>行为：什么都不做 — mod 仍能跑，IME 不可用。Listener 永远不被调。
 *
 * <p>覆盖关系：纯 fallback，不覆盖任何真实 IME 引擎。
 */
public final class StubImeBackend implements ImeBackend {

    private static final Logger LOGGER = LoggerFactory.getLogger(StubImeBackend.class);

    @Override
    public String name() {
        return "stub";
    }

    @Override
    public boolean probe() {
        return true;  // always available
    }

    @Override
    public Optional<ImeSession> start() {
        return Optional.of(new ImeSession() {
            @Override
            public String seat() { return "stub"; }
            @Override
            public void close() {}
            @Override
            public void commit(String text) {}
            @Override
            public void deleteSurrounding(int before, int after) {}
            @Override
            public void setCursorRectangle(int x, int y, int width, int height) {}
            @Override
            public void setEventListener(Listener listener) {}
        });
    }
}
