package dev.evvie.waylandcraft.ime;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.List;
import java.util.Optional;

/**
 * macOS Cocoa IME backend — 通过 JNI 调 {@code NSTextInputClient} 协议。
 *
 * <p>设计参考：developer.apple.com/documentation/appkit/nstextinputclient
 * — 任何 NSView 实现这个 protocol 就能接系统 IME。
 *
 * <p>覆盖关系：所有 macOS 自带 IME（日语 / 韩语 / 简体中文 / 繁体 /
 * 越南语 / 手写识别等）+ 第三方 IME （Squirrel / 鼠须管 / 仙鹤、
 * ATOK、Kotoeri）— 都走 Cocoa input system。
 */
public final class MacCocoaBackend implements ImeBackend {

    private static final Logger LOGGER = LoggerFactory.getLogger(MacCocoaBackend.class);

    @Override
    public String name() {
        return "macos-cocoa";
    }

    @Override
    public boolean probe() {
        // TODO: 通过 jni 调 osascript 或 sysctl 判断 macOS + IME 是否启用
        // 设计参考：developer.apple.com/library/archive/documentation/Cocoa/
        //   Conceptual/TextEditing/Tasks/TextViewTask.html
        // 真正的 macOS 实现是 JNI 调
        //   +[NSTextInputContext currentInputContext]
        //   -[NSTextInputClient insertText:replacementRange:]
        //   -[NSTextInputClient setMarkedText:selectedRange:replacementRange:]
        //
        // 这一 backend 应该是 waylandcraft 在 macOS viewer-only 平台下的
        // 焦点 — 但 macOS 平台没 native capture，IME 是 viewer-only 的
        // 子功能。优先级低于 fcitx5/ibus (只在 macOS 上激活)。
        return false;
    }

    @Override
    public Optional<ImeSession> start() {
        return Optional.empty();
    }
}
