# WaylandCraft IME 系统性审查 v0.11

## 第一章：当前真实架构（v0.10.2）

### 1.1 运行环境（v0.10.2 实机 env.txt）

- **OS**: Linux (GNOME)
- **XDG_SESSION_TYPE**: wayland
- **WAYLAND_DISPLAY**: wayland-0（宿主）
- **DISPLAY**: :0
- **XDG_CURRENT_DESKTOP**: GNOME
- **嵌套 firefox 启动时**: WAYLAND_DISPLAY=wayland-1（嵌套合成器）
- **ime 进程**:
  - ibus-daemon `--panel disable` (PID 2739)
  - ibus-memconf (2869)
  - ibus-extension-gtk3 (2870)
  - ibus-portal (2875) ← **关键！portal 入口**
  - ibus-engine-libpinyin (3046) ← **拼音引擎**
  - ibus-x11 (3220)
  - ibus-engine-simple (184691)

### 1.2 真实数据流（用户实机观察）

```
用户按 'n'（中文拼音 "你好" 第一字母）
  ↓
Minecraft 主线程（Java 渲染）
  ↓
Java bridge.keyboardInput(scancode=57, action=Press)  // [Java 端]
  ↓
Rust bridge::keyboard_input                            // [v0.10.2 真实路径]
  ├─ 1. ime.handle_key() → false（v0.10 mod 不拦）
  ├─ 2. seat.keyboard_key() → firefox 自己的 ti3 收到 raw key
  │     ↓ firefox GTK 处理 → GdkIMContext → ibus ProcessKeyEvent
  │     ↓ ibus 引擎处理（拼音状态机）→ preedit/commit
  │     ↓ firefox GTK 收到 preedit/commit → 文本框显示
  └─ 3. host_bridge.submit(DownEvent::Key{keysym: 0x6e, ...})
        ↓ [v0.10.2 新路径：用 seat.xkb_state 解码 keysym]
        ↓ host_bridge 内部 mpsc
        ↓ worker 线程 → ibus.ProcessKeyEvent(keysym, evdev, state)
        ↓ [BUG] 0 ProcessKeyEvent 日志（v0.10.2 实机 49 submit / 0 ProcessKeyEvent）
        ↓ [可能] zbus 同步调用静默失败（let _ = ...）
```

### 1.3 v0.10.2 实机 49 次 submit / 0 ProcessKeyEvent

**关键数据**（v0.10.2 实机日志 line 5-54）：
- `bridge submit_key scancode=10 keysym=0x31`（"1"）
- `bridge submit_key scancode=22 keysym=0xff08`（BackSpace）
- `bridge submit_key scancode=57 keysym=0x6e`（"n"）
- `bridge submit_key scancode=31 keysym=0x69`（"i"）
- `bridge submit_key scancode=43 keysym=0x68`（"h"）
- `bridge submit_key scancode=38 keysym=0x61`（"a"）
- `bridge submit_key scancode=32 keysym=0x6f`（"o"）

**keysym 正确**（v0.10.2 修复生效）——但**0 ProcessKeyEvent 日志**——**`let _ = ic_conns.ic.call(...)` 静默失败**。

### 1.4 真实双客户端问题（架构错误）

```
[双客户端独立工作——mod 走 host_bridge / firefox 走 GdkIMContext]

用户按 'n':
  Path A: bridge → host_bridge → ibus.ProcessKeyEvent → preedit 'n'
           → host_bridge 收信号 → mod ti3 推 firefox
  Path B: seat → firefox ti3 收 raw key → firefox GdkIMContext
           → ibus.ProcessKeyEvent（独立调） → preedit 'n'
           → firefox GTK 文本框显示

两条路径**都**调 ibus.ProcessKeyEvent 同样的 'n'——ibus 引擎去重——
**只有一条路径产生 commit**。但 firefox 文本框**可能**从 Path B 收到
preedit 字符串 'n'（GdkIMContext 显式画在文本框），**也可能**从 Path A
收到 commit 文本 '你'（mod ti3 推）。
```

**用户看到的现象**：
- 拼音+数字进窗口（**Path B 主导**）
- 最终汉字（**Path A 偶尔 work**）
- 延迟不对称（两条路径不同步）
- 闪（firefox focus 抖动 + 两条路径 enter/leave 抖动）

**架构错误**：`bridge::keyboard_input` 调 `host_bridge.submit` + `seat.keyboard_key` 双路——**双客户端独立工作**——**不**是 mod 应该设计的。

### 1.5 v0.10.2 实机 0 ProcessKeyEvent 真因

```rust
ToWorker::ProcessKey { keysym, evdev, state } => {
    // 同步调 ProcessKeyEvent（不等 reply——commit 驱动模式）
    let _ = ic_conns
        .ic
        .call::<_, _, bool>("ProcessKeyEvent", &(keysym, evdev, state));
    // reply 不重要（hybrid async 100% 超时实测）；不发送 FromWorker
    Ok(())
}
```

**`let _ = ...` 丢弃**：
- zbus 错误（如连接丢失、IC 不存在）—**静默**
- zbus Ok(false) — 不知道 consumed — **也不重要**（commit 驱动模式）
- 但 zbus 错误**必须**报告——**否则 49 submit 0 ProcessKey 没线索**

## 第二章：v0.11 计划

### 2.1 v0.11 目标

修复**所有**问题路径——不是单 bug：
1. ✅ zbus 调用错误日志（解决 0 ProcessKeyEvent 静默）
2. ✅ 结构化日志体系（time/component/event/trace_id/payload）
3. ✅ trace_id 端到端（key event 到 commit 到 application text）
4. ✅ 双客户端问题——**必须**改架构：
   - mod 完全**不**调 `seat.keyboard_key`（不再双路）
   - 让 firefox 自己的 GdkIMContext 处理（firefox 自己懂 ti3）
   - mod **只**接 host_bridge 上行 commit/preedit 信号——**推 ti3**
   - 这样**没有**字母键到窗口的问题
5. ✅ Surrounding/CursorRect 主动推（应用 enable 时调 apply_ti3_outcome）
6. ✅ 实测：commit 文本是真实汉字，**无**字母键残留

### 2.2 v0.11 重构

1. **删除** bridge.keyboard_input 的 `seat.keyboard_key()` 双路
2. **保留** host_bridge.submit（只让 mod 转发按键到 ibus）
3. **保留** firefox GdkIMContext 直通（不拦截键盘）
4. **增强** host_bridge submit 错误处理
5. **加** zbus 调用结果日志
6. **加** 结构化日志（JSON 格式 + trace_id）
7. **加** commit 链路完整性验证

### 2.3 不做的事

- 不删除 host_bridge（dbus 客户端是对的方向）
- 不实现 XIM server（firefox 走 GdkIMContext——不通过 mod）
- 不实现 im1 global（同上）
- 不重写 seat.rs
- 不重写整个 IME 子系统

## 第三章：v0.11 双客户端消除方案

### 3.1 新架构（v0.11）

```
[用户按 'n'（嵌套 firefox 输入框）]
  ↓
Minecraft 主线程
  ↓
bridge.keyboard_input
  ├─ 1. host_bridge.submit(DownEvent::Key{keysym, ...})
  │     ↓
  │     host_bridge worker 线程
  │     ↓
  │     ibus.ProcessKeyEvent(keysym, evdev, state)  ← v0.10.2 修复
  │     ↓
  │     ibus 引擎处理（拼音状态机）
  │     ↓
  │     ibus 异步发 commit_text / update_preedit_text
  │     ↓
  │     host_bridge 收信号 → UpEvent 流
  │     ↓
  │     lib.rs::update 每帧 drain → apply_up_events
  │     ↓
  │     relay.ime_op → ti3 wire → firefox 文本框
  │
  └─ 2. **不**调 seat.keyboard_key（v0.10.2 之前双路——删）
```

**结果**：
- firefox **不**通过 GdkIMContext 处理（**因为 mod 已经吞键**——firefox 收不到 key）
  → firefox GTK 不知道按了什么
  → 不会显示 preedit 字符串
  → 不会显示字母
- 只能看到 mod 通过 ti3 推的 commit 文本（汉字）
- **没有**双客户端冲突

**但 firefox 收不到 key**——它的 GdkIMContext 拿不到 key——**它怎么知道要显示 preedit**？

**答案**：**firefox 不需要知道**——preedit 由**宿主 ibus kimpanel 画**（不嵌入 firefox 文本框）——候选窗是独立窗口。

### 3.2 firefox 行为变化

v0.10.2 之前：firefox 自己 GdkIMContext 显示拼音在文本框 + mod 也推
v0.11：firefox 收不到键盘（mod 吞了）——firefox 不显示拼音在文本框
     preedit 由宿主 kimpanel 显示（独立窗口）
     commit 由 mod 通过 ti3 推到 firefox 文本框——只显示汉字

**这是 GNOME 原生输入法行为**——和原生命令行 `fcitx5` 用 firefox 一样。

## 第四章：实施步骤

### Step 1: v0.11.0 修双客户端
- 删 bridge.keyboard_input 的 seat.keyboard_key 双路
- 保留 host_bridge.submit 路径
- 加 zbus 错误日志
- 加结构化日志（可选）

### Step 2: 完整测试
- 实机：firefox 文本框只显示汉字（"你"/"年"/"好"）
- 不显示字母
- 不显示拼音
- 候选窗由宿主 kimpanel 显示
- 延迟降低

### Step 3: 写报告

## 第五章：未做的事（明确边界）

- 不修 XIM server（xterm 路径未实现）
- 不修 im1 global（ibus-wayland 路径未实现）
- 不实现 XWayland native 应用路径
- 不做 smithay 完整 im2/ti3 manager 集成（已证明 E0119 冲突）

## 第六章：命令

让我立即开始 Step 1：v0.11.0 双客户端消除。
