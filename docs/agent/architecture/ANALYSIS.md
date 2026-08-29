# WaylandCraft IME 架构分析

## 当前架构

### 数据流（用户按键 → 文本进 firefox 框）

```
用户按 'n'
  ↓
Minecraft 主线程
  ↓
Java bridge.keyboardInput(scancode, action)
  ↓
Rust bridge::keyboard_input
  ├─→ 1. ime.handle_key (im2 grab 路径，已被 mod 接管)
  └─→ 2. host_bridge.submit(DownEvent::Key)  [v0.9.43+]
        ↓
       host_bridge worker 线程
        ↓
       zbus 同步调用 ibus.ProcessKeyEvent  [v0.9.43+ 接管键盘]
        ↓
       ibus daemon (libpinyin 引擎)
        ↓
       async 发回 CommitText / UpdatePreeditText 信号
        ↓
       host_bridge signal 线程 → 主线程 mpsc
        ↓
       lib.rs::update 每帧 take_up_events_batched
        ↓
       ime::apply_up_events → relay.ime_op → ti3 wire
        ↓
       firefox 文本框显示 commit 汉字
```

### 控制流（每帧 update 循环）

```
WaylandCraft::update() (每帧)
  ├─ 1. self.host_bridge 移到 self.state.host_bridge (共享给 Dispatch 路径)
  ├─ 2. host_bridge drain 上行事件 → apply_up_events → 推 ti3
  ├─ 3. event_loop.dispatch → 触发 Dispatch 路径
  │   ├─ text_input_v3::commit 回调 → apply_ti3_outcome
  │   │   ├─ O::Enabled → relay.set_app_enabled(true) + host_bridge.submit(FocusIn)
  │   │   └─ O::Disabled → relay.set_app_enabled(false) + host_bridge.submit(FocusOut)
  │   └─ input_method_v2::commit 回调 → ime_commit_from_wire
  └─ 4. host_bridge 移回 self.host_bridge
```

## 模块依赖

```
WLCState (lib.rs)
├─ ime: ImeState (ime/mod.rs)
│  ├─ ti3: TextInputV3State (ime/text_input_v3.rs) [自造]
│  ├─ im2: InputMethodV2State (ime/input_method_v2.rs) [自造]
│  └─ relay: Relay (ime/relay.rs) [自造]
├─ host_bridge: HostBridgeHandle (host_bridge/mod.rs)
│  ├─ dbus_ibus: DbusIbusBridge (host_bridge/dbus_ibus.rs)
│  └─ dbus_fcitx5: DbusFcitx5Bridge (host_bridge/dbus_fcitx5.rs)
└─ seat: WLCSeatState (seat.rs) [自造]
```

**关键问题**：waylandcraft 整个 ime/ + seat/ 都**自造**——**不依赖** smithay::wayland::input_method / smithay::wayland::text_input / smithay::Seat

## 核心问题清单

### 问题 1：smithay SeatHandler 未实现（致命）
- **症状**：waylandcraft 用 `WLCSeatState`（自造），不能用 smithay 的 `InputMethodKeyboardGrab`（需要 smithay Seat）
- **根因**：waylandcraft 整个 keyboard pipeline 走 `seat.keyboard_key()`，不经过 smithay Seat
- **修复代价**：~2000 行（重写 seat.rs 用 smithay::input::Seat + SeatHandler）
- **修复依赖**：smithay 完整 im2 框架

### 问题 2：自造 im2 server 框架（~900 行手写代码）
- **症状**：native/src/ime/{input_method_v2,text_input_v3,relay}.rs 都在重复造 smithay 已有功能
- **根因**：v0.9.27 重构时决定"不复用 smithay 框架"（当时讨论见 memory/2026-08-26/waylandcraft-ime-fix.md）
- **修复代价**：~1500 行删除 + 复用 smithay（~500 行新胶水）

### 问题 3：v0.9.45 残留 bug
- 1. **im2 grab 拦截双路径**：press 通过 host_bridge + ime::handle_key 两次进入（已发现，已优化）
- 2. **ti3 enter/leave 抖动**：firefox GTK 正常行为，但导致 app_active toggle（v0.9.44 修"disable 不清缓冲"已部分缓解）
- 3. **commit 0 commit 偶发**：host_bridge dbus-ibus / dbus-fcitx5 信号解析有 edge case

## 根因分析

**waylandcraft 整个 IME 子系统走了一条"自造 smithay 替代"的路径**——8 次版本（v0.9.38-45）都在**这条错的路径上**修表面。

**真正能解决问题的路径只有一条**：
- **用 smithay 完整 im2 + ti3 框架**
- **重写 seat.rs 用 smithay::input::Seat**
- **删除所有自造 ime 状态机**

## 推荐架构（基于 smithay InputMethodManagerState）

### 新模块边界

```
WLCState: smithay::wayland::compositor::CompositorHandler + SmithayState<D>
  ├─ im_manager: smithay::InputMethodManagerState
  │   └─ im2 server (smithay 提供)
  ├─ ti_manager: smithay::TextInputManagerState
  │   └─ ti3 server (smithay 提供)
  ├─ seat: smithay::input::Seat<D>
  │   ├─ keyboard_grab: smithay::InputMethodKeyboardGrab
  │   └─ focus: WlSurface
  └─ host_bridge: HostBridgeHandle (仅做 dbus 客户端)
       └─ zbus 同步调用 ibus.ProcessKeyEvent
```

### 数据流（重写后）

```
用户按 'n'
  ↓
bridge::keyboard_input
  ↓
seat.input_method().with_im() 或 .forward_key_to_host()
  ↓
host_bridge.submit(Key)
  ↓
dbus ProcessKeyEvent
  ↓
ibus 引擎
  ↓
commit/preedit 信号
  ↓
host_bridge 收 UpEvent
  ↓
smithay TextInputManagerState → 推 ti3 wire 到 firefox
  ↓
firefox 文本框显示
```

## 重构路线

### Phase 1: smithay SeatHandler 接入
- 把 WLCSeatState 重构为 smithay::input::Seat<D, WLCState>
- bridge::keyboard_input 改用 seat.input() 路径
- 删除 WLCSeatState 自造代码

### Phase 2: 删自造 ime，引入 smithay im2 + ti3
- 删除 input_method_v2.rs / text_input_v3.rs / relay.rs
- 用 `delegate_input_method_manager!` + `delegate_text_input_manager!`
- 写 InputMethodHandler + TextInputSeat

### Phase 3: host_bridge 与 smithay InputMethodKeyboardGrab 对接
- KeyboardGrab::input 回调 → host_bridge.submit(Key)
- commit/preedit 信号 → TextInputHandle.set_preedit / set_commit_string

### Phase 4: 集成测试
- 实机 firefox 输入中文
- 验证 xterm 路径（XIM server 暂不做——waylandcraft 范围只覆盖 ti3 + im2）

## 工作量估计

| 阶段 | 代码量 | 风险 |
|---|---|---|
| Phase 1: smithay Seat | 2000 行（含 smithay 适配层） | 中（smithay 泛型 D） |
| Phase 2: im2 + ti3 server | 1500 行删除 + 500 行胶水 | 低（smithay 框架稳定） |
| Phase 3: host_bridge 集成 | 800 行 | 中（key 路径测试） |
| Phase 4: 测试 | 200 行测试 | 低 |
| **总计** | **删 2000 + 新 3500 = 净 +1500** | |

## 强制结论

- **现有架构错误**——自造 smithay 替代品是技术债
- **必须重写**——不能继续 patch
- **smithay 0.1+ 是唯一可行 Rust 库**——其他都是 C/C++/完整项目
- **不能取巧**——必须按 Phase 1-4 顺序来

## 与 8 次版本尝试的关系

- v0.9.40 删 host_ime：✓ 删错了自造穿透
- v0.9.41-45 加 host_bridge：✓ dbus 客户端需要，但**仅是 Layer 3**
- v0.9.45 当前状态：❌ Layer 1（XIM/im2 server）依然自造

**所有 8 次尝试都是"在错的架构上修表面"**——这次必须**重写架构**。

## 不做

- 不重写整个 waylandcraft（太大）——只重写 seat + ime
- 不实现 XIM server（waylandcraft 范围只覆盖 ti3 + im2 + host_bridge）
- 不实现 im1 global（v2 已足够，im1 是历史兼容）
- 不做 fcitx5 单独测试（共用 ImeEvent 抽象）
