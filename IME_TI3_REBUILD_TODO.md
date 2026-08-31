# IME 路径 A 修正版 — 重建 zwp_text_input_v3 server（已完成编译）

## 真实根因（v1.2.4 实测）

- Minecraft 窗口跑在 X11/XWayland fallback（GLFW wayland backend 未启用）
  - → MC 自动接 XIM → 聊天框能输入汉字 ✅
- 嵌入 MC 世界的 firefox 是 **native wayland client**（连 waylandcraft 的 wayland-1）
  - → 需要 `zwp_text_input_v3` global 才能接 IME
  - → **waylandcraft 没暴露该 global**（v0.10 删了 text_input_v3.rs）
  - → firefox 接不上 IME ❌

## 修复路径 A — 重建 zwp_text_input_manager_v3 server

让 waylandcraft 作为 wayland compositor 暴露 text-input 协议：
- firefox enable ti3 → mod 通知 host_bridge FocusIn → ibus
- firefox key → mod 调 host_bridge ProcessKeyEvent → ibus
- ibus CommitText → mod 通过 ti3 obj.commit_string() / preedit_string() 发回 firefox
- firefox disable → mod 通知 host_bridge FocusOut

## 实现计划

- [x] Rust: 重建 `ime/text_input_v3.rs`（从 .deleted_v010/ 拷贝 + 适配）
- [x] Rust: `ime/mod.rs` 加 `ti3: TextInputV3State` 字段 + `create_globals` 实际注册
- [x] Rust: `lib.rs` 自己实现 Dispatch（不用 `delegate_text_input_manager!`，因为它要 SeatHandler）
- [x] Rust: `bridge.rs` `keyboard_focus` 调 `ime.ti3.enter/leave`（拆分借用）
- [x] Rust: `apply_up_events` 真处理 Preedit → ti3 preedit_string + cursor
- [x] Rust: `apply_up_events` 真处理 Commit → ti3 commit_string
- [x] Rust: `set_focus` 同时调 ti3.enter
- [x] Rust: `clear_focus` 同时调 ti3.leave + host_bridge FocusOut
- [x] Rust: `apply_ti3_outcome` 完整化（Enabled 真触发 host_bridge）
- [x] Rust: cargo check ✅ + cargo build --release ✅
- [x] Java: 不需要改（text_input_v3 是 wayland 协议层，不影响 Java API）
- [x] Java gradle build ✅
- [ ] 实机测试：firefox libpinyin 输入汉字（待用户测）

## 编译验证

- Rust cargo build --release: 0 error, 40 warnings (既有 36 + 新增 4)
- Java ./gradlew build: BUILD SUCCESSFUL
- 净改动 ~350 行 Rust, 0 行 Java

## 实机测试要点

1. 启动 waylandcraft 1.2.5（v0.13）
2. /wl launch firefox
3. firefox 焦点 → Java side 应该看到 `[kb-debug] mode=CAPTURE bridge=yes sharedCap=no hoveredLocal=hit hoveredShared=no`
4. firefox 输入框按字母（拼音）→ IME 日志应该看到：
   - `[waylandcraft][ime][ti3] Enable obj=...`
   - `[waylandcraft][ime][ti3] SetSurroundingText obj=... text="n" cursor=1 anchor=1`
   - `[waylandcraft][ime][ti3] SetCursorRectangle obj=... rect=(...)`
   - `[waylandcraft][ime][ti3] commit_instance Enabled`
   - `[waylandcraft][ime][host_bridge] FocusIn（ti3 enable）`
   - `[waylandcraft][host_bridge][dbus-ibus] ProcessKeyEvent keysym=0x... -> consumed=true`
   - `[waylandcraft][host_bridge][dbus-ibus] handle_signal: UpdatePreeditText text="你"`
   - `[waylandcraft][host_bridge][dbus-ibus] handle_signal: CommitText text="你"`
5. firefox 应该看到 preedit 显示拼音 → commit 显示汉字

## 已知问题

- smithay 0.7 的 `TextInputManagerState` 与 `SeatHandler` 强绑定——我们
  WLCSeat 是自定义的，**不能**直接用 `delegate_text_input_manager!`。
  自己实现 Dispatch（见 ime/text_input_v3.rs）。
- `set_preedit_string` 实际 protocol 事件名是 `preedit_string`（无 set_ 前缀）
- `commit_string` 同理
- `preedit_string` 没有 serial 参数（与 ibus / fcitx 不同）
- `delete_surrounding_text` 是 u32 不是 i32