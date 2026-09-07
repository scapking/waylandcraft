package dev.evvie.waylandcraft.ime;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.List;
import java.util.Optional;

/**
 * Android IME backend — 通过 {@code android.view.inputmethod.InputMethodManager}
 * 通信。
 *
 * <p>设计参考：developer.android.com/games/agdk/add-support-for-text-input
 * — Android GameTextInput API + {@code BaseInputConnection}。
 *
 * <p>覆盖关系：所有 Android IME（GBoard / SwiftKey / 搜狗输入法 / QQ输入法
 * / Samsung Keyboard / 华为小艺 / 微信键盘 / 科大讯飞等）— 都走
 * Android IME framework 统一接口。
 */
public final class AndroidImeBackend implements ImeBackend {

    private static final Logger LOGGER = LoggerFactory.getLogger(AndroidImeBackend.class);

    @Override
    public String name() {
        return "android-ime";
    }

    @Override
    public boolean probe() {
        // TODO: 通过 jni / 反射判 Android
        // Minecraft.getInstance() instanceof MinecraftClient on Android —
        //   waylandcraft 的 Platform.isAndroid() (bridge.java:164) 已经在用
        // 这个 backend 只在 Android 上激活
        return false;
    }

    @Override
    public Optional<ImeSession> start() {
        // TODO: 通过 GameTextInput (AGDK) 或 fallback 到 InputMethodManager
        //   InputMethodManager imm = context.getSystemService(...)
        //   imm.showSoftInput(view, 0)
        //   InputConnection ic = view.onCreateInputConnection(EditorInfo)
        //   ic.commitText(text, cursorPos)
        //   ic.setComposingText(text, cursor)  -- preedit
        //
        // 设计参考：developer.android.com/games/agdk/add-support-for-text-input
        return Optional.empty();
    }
}
