# WaylandCraft IME 架构（C 方案 — 嵌套合成器作为完整 mini 桌面）

## 目标

mod 在嵌套合成器里**对外是完整的桌面 IME 服务**——支持所有桌面 IME 协议，**对内**只是宿主 IME daemon 的**透明转发器**。

**绝不模拟、绝不嵌套、绝不实现 IME 引擎**。**所有 IME 输入来自宿主**（ibus / fcitx5 / 其他 dbus 后端）。

## 三层架构

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 1: 协议适配（XIM server / im2 global / im1 global）  │
│  - 接收应用 IME 协议 → 转内部 ImeEvent 流                  │
│  - 接收内部 ImeEvent → 翻译回协议（commit/preedit 等）     │
└────────────────────┬────────────────────────────────────────┘
                     │
┌────────────────────▼────────────────────────────────────────┐
│  Layer 2: 内部 IME 事件流（协议无关）                       │
│  enum ImeEvent {                                            │
│      Activate,                                              │
│      Deactivate,                                            │
│      PreeditUpdate(String, i32, i32),                       │
│      Commit(String),                                        │
│      Done(u32),                                             │
│  }                                                          │
└────────────────────┬────────────────────────────────────────┘
                     │
┌────────────────────▼────────────────────────────────────────┐
│  Layer 3: 宿主桥接（dbus-ibus / dbus-fcitx5）              │
│  - 内部事件 → 宿主 daemon（ProcessKeyEvent + 信号）         │
│  - 宿主信号（CommitText/UpdatePreedit）→ 内部事件            │
│  - 用 hybrid async mode 2 通信（事实：mode 2 是 ibus 推荐）│
└─────────────────────────────────────────────────────────────┘
```

## 协议覆盖

| 协议 | 用途 | 应用类型 |
|---|---|---|
| XIM server | X11 输入法协议 | xterm、emacs -nw、纯 X11 应用 |
| im2 (zwp_input_method_v2) | Wayland 现代 IME 协议 | gnome-text-editor、wayland native 应用 |
| im1 (zwp_input_method_v1) | Wayland 老 IME 协议 | ibus-wayland 客户端（ibus 内部用） |

**覆盖全桌面 IME 协议**——所有 Linux 桌面应用都能用嵌套合成器的 IME。

## 数据流（用户按 'n'）

```
用户按 n
  ↓
嵌套 firefox 收到键盘事件
  ↓
firefox 内部 GdkIMContext 处理（GTK 自己的 IME client）
  ↓
firefox 通过 im2 协议向嵌套合成器发 ProcessKeyEvent
  ↓
[Layer 1: im2 global] 收到，翻译为 ImeEvent::KeyPress('n', keysym=0x6e)
  ↓
[Layer 2: 内部事件流] ImeEvent 通过 mpsc 流向 Layer 3
  ↓
[Layer 3: dbus-ibus] 调 org.freedesktop.IBus.InputContext.ProcessKeyEvent
  ↓
宿主 ibus daemon 引擎 libpinyin 处理拼音
  ↓
宿主 ibus 发 CommitText / UpdatePreeditText 信号
  ↓
[Layer 3: dbus-ibus] 信号翻译为 ImeEvent::Commit / PreeditUpdate
  ↓
[Layer 2: 内部事件流] 流回 Layer 1
  ↓
[Layer 1: im2 global] 翻译为 wire 协议 commit_string / preedit_string
  ↓
firefox 文本框显示 "n" + 候选窗显示汉字
```

**mod 永远不模拟 IME——所有处理都委托给宿主 ibus daemon**。

## 关键设计原则

1. **不模拟**——mod 不实现 XIM server 时假装是 XIM server（是的，需要实现 XIM server，但不假装——它**真**是 XIM server，应用连它就以为是真 XIM server，但所有按键转给宿主 daemon）
2. **不嵌套**——不启动第二个 ibus 实例；只连用户现有的
3. **协议无关内层**——XIM/im2/im1 都翻译成同一套 ImeEvent，业务逻辑只跟 ImeEvent 打交道
4. **唯一 IME 源**——所有应用通过 mod，mod 通过 dbus 接宿主 daemon
5. **保留 ti3 协议**——嵌套应用内部仍然用 ti3 协议（text_input_v3）+ im2 server（input_method_v2 server）

## 待删 / 待改

### 删除
- `native/src/host_ime/` 整个模块（dbus_ibus.rs, dbus_fcitx5.rs, mod.rs）
- `native/src/system_ime.rs` 的 dbus 后端连接代码
- `native/src/bridge.rs::keyboard_input` 的 `passthrough_wants_keys` 拦截分支
- Java 端 `CursorRectReporter.java`（嵌套应用自管光标）

### 重构
- `native/src/ime/` 重写为协议无关的 ImeEvent 流
- `native/src/ime/text_input_v3.rs` 保留（应用端 ti3 client 仍工作）
- `native/src/ime/input_method_v2.rs` 改为真正的 input method server（不是中继）

### 新增
- `native/src/ime/xim_server.rs` — XIM 协议实现
- `native/src/ime/input_method_v1.rs` — im1 global
- `native/src/ime/ime_event.rs` — 内部 IME 事件枚举
- `native/src/ime/host_bridge.rs` — 宿主 dbus 桥接（替代 host_ime）
- 测试：mock XIM client、mock im2 client、mock im1 client

## 工期

~5-7 周（7000-10000 行新代码 + 测试 + 文档）

## 不做兼容层

- **不保留旧的 host_ime 路径**——它在 v0.9.39 时已经证明架构错误
- **不保留旧的 system_ime ti3 中继**——它让 firefox 双客户端冲突
- **不做"环境变量切换"**——只有一套正确路径
- **不做"如果用户不用 X 就跳过"**——所有协议都实现
