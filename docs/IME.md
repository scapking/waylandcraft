# WaylandCraft 输入法（IME）架构

> 本文档描述 v0.9.27+ 的输入法实现。该版本是一次**协议层重构**：
> 删除了旧的 text-input-v1 / input-method-v1 路径与全部 workaround，
> 以标准现代协议栈（zwp_text_input_v3 + zwp_input_method_v2）为核心重建。

## v0.9.27 重构摘要：为什么旧实现无法工作 & 新实现如何解决

### 为什么旧代码不能实现输入法

旧代码把 IME 当作「字符串搬运管道」，但 Wayland 输入法协议是**分布式
double-buffered 状态机**，正确性由 serial 纪律、原子性、生命周期耦合三者
共同保证——旧代码三者皆缺：

| # | 缺陷 | 协议依据 | 后果 |
|---|---|---|---|
| 1 | 穿透路径发 `commit_string`/`preedit_string` 后**从不发送 `done`** | text-input-v3：客户端缓冲一切事件，仅在 `done(serial)` 到达时应用 | 桌面输入法提交的文本永远停在 App 的缓冲区里，无法上屏 |
| 2 | 宿主事件拆进三条独立缓冲（committed/preedit/delete）分别转发 | 一个 done 周期内必须按 删除→插入→preedit 固定次序应用 | `delete_surrounding + commit` 批次乱序 → 选区重组产出错误文本 |
| 3a | `clear_focus()` 发 `deactivate+done` 却不给计数器 +1 | im-v2：`commit(serial)` 的 serial 必须等于该对象已收到的 done 总数 | 焦点切换一次后，所有后续组合的 serial 全部「过期」→ 整批静默丢弃 |
| 3b | `ti3_serial` 是全局共享计数器 | serial 按「该 text_input 对象收到的 commit 请求数」per-instance 计数 | 多客户端交错提交时 serial 彻底混乱 |
| 3c | im2 事件收到即转发，不等 `commit(serial)` 校验 | 不匹配时合成器必须整批丢弃、不改变状态 | 「丢弃」时污染早已到达 App，丢弃机制形同虚设 |
| 4 | 同时广播 ti_v1/im_v1 与 ti_v3/im_v2；KWin BLOCKED 用 15 秒轮询重建 | — | ibus 被 v1 吸走造成行为分叉；轮询救不了结构性问题 |

### 新代码为什么可以

核心是新增的**纯逻辑中继状态机** `ime/relay.rs`（零 Wayland 类型依赖），
显式建模协议语义本身；wire 层退化为薄适配器：

1. **done 纪律成为不变式**：每次批次放行后必然且只发一次
   `ti.done(<per-instance commit 计数>)` —— 这是客户端应用文本的充要条件。
2. **单 FIFO 原子缓冲**：IME 操作只在两个出口落地——im2 `commit(serial)`
   校验通过（不符则整批清空、App 零感知）；穿透 Done 标记处保序应用。
3. **正确的 serial 模型**：两侧均 per-object 计数；activate/deactivate/
   PushState 每次都推进 IME 侧计数（旧代码漏掉的正是 deactivate 这次）。
4. **生命周期耦合**：焦点 A→B 直接切换时先终结旧会话（Deactivate），
   B 的 enable 再触发 Activate——否则新 surface 的 enable 会被误判为
   会话延续。
5. **双向数据流**：App 的 surrounding/cursor/content_type/cursor_rect
   反向同步给当前端点（桌面 fcitx5 因此拿到真实文本上下文）。
6. **结构问题用结构手段**：X11 后端结构性不支持 enter → 启动即明确
   Unsupported；原生 Wayland 下 KWin 晚创建 text_input 收不到 enter →
   窗口焦点事件驱动的一次性重建（GLFW 回调 → JNI），全程无定时器。

验证：22 个 cargo test —— relay 单元测试 9 个 + **真线缆集成测试** 13 个
（真实 `Display<WLCState>` 上跑两个真实 wayland-client 连接，分别模拟
编辑器与 fcitx5），覆盖拼音逐键组合/退格/候选/选区重组/过期 serial/
A→B 直接切换/grab 分流等完整场景。

## 1. 总体架构

WaylandCraft 同时扮演两个角色：

```
                    ┌────────────────────────────────────────────┐
                    │            Minecraft (Java/GLFW)           │
                    └───────────────┬────────────────────────────┘
                                    │ JNI（scancode/action、焦点事件）
┌──────────┐   wl_surface/wl_keyboard│
│ 游戏内 App │◄───────────────►┌──────▼─────────────────────────┐
│(GTK/Qt…) │  zwp_text_input_v3 │   游戏内合成器 (Rust/smithay)    │
└──────────┘        ▲           │  native/src/ime/               │
                    │           │                                │
                    │           │  Relay 状态机（纯逻辑核心）       │
                    │           │    ├ 端点A：游戏内 im2 客户端     │
                    │           │    └ 端点B：宿主桌面穿透          │
                    │           └──────┬────────────▲────────────┘
                    │                  │ 命令出站     │ 事件入站(保序)
                    │           ┌──────▼────────────┴────────────┐
                    │           │ system_ime.rs（text-input-v3    │
                    │           │   客户端）                       │
                    │           └──────┬────────────▲────────────┘
                    │                  │             │
                    │           ┌──────▼────────────┴────────────┐
                    └──────────►│   宿主合成器 ⇄ 桌面输入法         │
                        复用      │   (KWin / Mutter / wlroots…)   │
                     GLFW连接     │   fcitx5 / IBus                │
                                └────────────────────────────────┘
```

### 模块职责边界（native/src/）

| 模块 | 职责 | 禁止事项 |
|---|---|---|
| `ime/relay.rs` | **纯逻辑协议中继**：serial 记账、enable/disable 生命周期、IME 变更原子缓冲与丢弃判定。零 Wayland 类型依赖 | 不碰任何 wire 对象 |
| `ime/text_input_v3.rs` | ti3 wire 层：manager/object Dispatch、double-buffer pending、per-instance commit 计数 | 不做语义裁决 |
| `ime/input_method_v2.rs` | im2 wire 层：对象管理、键盘 grab 种子化、popup 矩形上报 | 不做语义裁决 |
| `ime/mod.rs` | 门面：全局对象注册、端点选择/切换、命令执行分发 | — |
| `system_ime.rs` | 宿主穿透客户端：连接管理、enable 调和状态机、保序事件缓冲、反向同步执行 | 不含任何定时器/轮询 |
| `seat.rs` | 键盘/指针焦点与原始键转发（键盘输入域，与文本输入严格分离） | — |

## 2. 协议栈与版本

| 协议 | 版本 | 角色 | 用途 |
|---|---|---|---|
| `zwp_text_input_v3` / manager_v3 | 1 | 服务端 + 客户端 | 游戏 App ↔ 合成器；合成器 ↔ 宿主（穿透） |
| `zwp_input_method_v2` / manager_v2 | 1 | 服务端 | 输入法引擎接入游戏内合成器 |
| `wl_seat` / `wl_keyboard` | 8 | 服务端 | 键盘焦点与原始按键 |

**刻意不注册** `zwp_text_input_manager_v1` / `zwp_input_method_v1`：
v1 是废弃路径（功能性子集），同时广播会诱导 ibus 退回 v1 造成行为分叉。
现代栈下 fcitx5（im2）完全支持；IBus 用户走宿主穿透路径。

## 3. 数据流

### 3.1 正向（按键 → 文本）

```
键盘(Java scancode+action)
  → bridge::keyboard_input
      ├ IME grab 存在 → 按键发给 grab 对象（IME 消费）【handle_key 返回 true】
      └ 无 grab → seat.keyboard_key → 游戏 App 的 wl_keyboard（普通路径）
```

游戏内 IME（端点 A，如直接跑在游戏合成器上的 fcitx5）：
```
App enable → Relay Activate → im2 activate+state+done
IME preedit/commit_string/delete + commit(serial)
  → Relay 校验 serial → 整批应用 → ti3 事件 + done(commit计数)
```

宿主穿透（端点 B，默认主路径）：
```
App enable → 出站命令 Activate(state)
SystemIme 调和：ti.enable() + set_surrounding_text/content_type/cursor_rectangle + commit()
宿主合成器路由按键给桌面输入法 → 组合/提交
宿主 done → HostEvent 缓冲（Enter/Leave/CommitString/PreeditString/
            DeleteSurroundingText/Done，严格保序）
→ ImeState::passthrough_events → Relay 原子应用 → ti3 事件 + done
```

### 3.2 反向（App 文本状态 → IME）

App 每次 ti3 commit 携带的 surrounding_text / cursor / anchor /
content_type / text_change_cause / cursor_rectangle 都会进入 Relay，
经当前端点送达输入法：

- 端点 A：im2 `surrounding_text` / `text_change_cause` / `content_type` 事件；
- 端点 B：宿主 ti3 的 `set_surrounding_text` / `set_text_change_cause` /
  `set_content_type` / `set_cursor_rectangle` 请求。

只转发 app 显式设置过的字段；协议默认值不产生噪音批次。

## 4. Focus 生命周期

```
seat 键盘焦点变化（bridge::keyboard_focus / keyboard_unfocus）
  → ImeState::set_focus(surface) / clear_focus()
     ├ ti3.enter(surface)：发给聚焦 client 的全部 text_input 实例（协议强制）
     ├ ti3.leave()：leave 全部实例；此后忽略该 client 的一切请求直至下次 enter
     └ Relay.focus_lost()：清空 pending 缓冲 → 端点 Deactivate
```

规则：

- enter 后 app 才能有效 enable；leave 自动使 enable 失效（两侧一致）。
- 焦点丢失时未应用的 IME 变更整体作废（不会落到新 surface）。
- 端点切换（游戏内 IME 上线/下线 ↔ 穿透就绪）时 Relay 复位 per-endpoint
  计数并按需向新端点补发 Activate。

## 5. State Synchronization（serial 规则）

两条独立但对称的计数链，全部 **per-object**：

| 方向 | 计数器 | 含义 |
|---|---|---|
| 合成器 → App | `done(serial)` | serial = 该 text_input 对象收到的 commit 请求数 |
| IME → 合成器 | `commit(serial)` | serial 必须 = 该 input_method 对象已发的 done 数 |
| 合成器 → IME | `done()`（无参数） | 每次 activate/deactivate/状态推送后必发一次 |
| App → 合成器 | `commit()`（无参数） | double-buffer 应用点 |

- `commit(serial)` 与计数不符 → **整批丢弃**（协议："proceed as normal,
  except it should not change the current state"），app 零感知。
- IME 重连即从计数 0 开始（新对象新基准）。
- 穿透方向的 serial 校验由宿主合成器完成；到达 Relay 的批次视为已校验，
  通过无条件的 `ime_flush()` 应用。

## 6. Preedit / Commit 工作方式

一个组合周期（以拼音 nihao → 你好 为例）：

1. IME 每步发送 `preedit_string("n"…"nihao") + commit(serial)`；
2. Relay 缓冲操作，serial 匹配后整批放行；
3. 编辑器收到 `preedit_string` … `done(N)`，N = 它自己发过的 commit 数；
4. 选定候选：`preedit_string("")` + `commit_string("你好")` 同批到达，
   编辑器按协议固定次序应用：替换旧 preedit → 删除 surrounding → 插入
   commit → 重算 surrounding → 插入新 preedit；
5. 选区重组场景 `delete_surrounding_text(before, after)` 与后续 commit
   的相对顺序原样保持（Relay 是 FIFO 缓冲）。

组合期间合成器不推送新状态 → 不产生新 done → IME 各步回填同一 serial，
这是协议预期行为（真实 fcitx5 即如此）。

## 7. 测试方式

```sh
cd native && cargo test          # 全部 22 个测试
cargo test --lib ime::           # 仅 IME 子系统（20 个）
```

两层测试：

- **relay 单元测试**（9 个）：纯逻辑状态机的 serial 链、丢弃语义、
  端点重连、焦点丢失、组合全流程。
- **线缆级集成测试**（11 个，`ime/tests.rs`）：真实 `Display<WLCState>`
  （无 GPU 模式，dmabuf 关闭）+ 两个真实 wayland-client 连接——
  「编辑器」（ti3 客户端）与「模拟 fcitx5」（im2 客户端），覆盖：
  enable 激活、逐键拼音组合、组合中退格、组合中移动光标、候选选定提交、
  选区删除重组保序、过期 serial 丢弃、焦点 A→none→A、焦点 A→B 直接切换、
  enable/disable/enable 循环、键盘 grab 分流与释放、穿透入站保序应用、
  穿透出站反向同步。另有 seat.rs 的英文原始键路径测试。

Java 侧：`./gradlew build`（需 JDK 25）；mixin 注入 windowFocusChanged
驱动事件式重协商。

## 8. 已验证环境

- **协议一致性**：上述 21 个线缆级测试（真实 wayland-server/client 双端）。
- **实机桌面**：本仓库 CI/开发容器无显示服务器，KWin/Mutter/GNOME +
  fcitx5/IBus 实机回归待社区验证。已知行为参考：
  - KWin ≥6.x：im2 + ti3 完整支持；晚创建 text_input 收不到 enter 的
    问题由窗口焦点事件的**一次性重建**处理（非轮询）。
  - wlroots 系（sway 等）：im2 标准支持。
  - GNOME/Mutter：ti3 经 mutter 内置 IBus 集成支持。

## 9. 已知限制

1. **输入法候选窗（popup surface）不在游戏画面内渲染**：协议对象与矩形
   上报完整维护，但 popup 像素合成进游戏视图需要 ddm 渲染管线支持，
   为独立特性。穿透模式下候选窗显示在**宿主桌面**上（光标矩形已反向
   同步给宿主，定位正确），不受此限制影响。
2. **X11/XWayland 后端的 wayland-ti3 路径不支持宿主穿透**：结构性限制——
   自建连接没有 wl_surface，宿主的 enter 需要 client/surface 关联。
   v0.9.29 起该场景由 **dbus-ibus 后端**覆盖（见 §10），不再依赖游戏窗口系统；
   仅当宿主既非原生 Wayland 又无 ibus 时才落入 Unsupported 诊断。
3. **游戏内 IBus（端点 A）不可用**：ibus 引擎客户端仅实现了 im-v1；
   现代栈下请用 fcitx5 直连游戏合成器，或直接使用穿透路径（推荐，
   无需在游戏内运行任何输入法进程）。

## 10. 宿主后端矩阵（v0.9.29 起）

「把桌面输入法接进嵌套合成器」在不同窗口系统下有不同标准协议栈。
没有任何单一协议能通吃 —— 正确形态是**统一中继内核 + 可插拔宿主后端**：
所有后端产出同一套 `HostEvent`（Enter/Leave/Commit/Preedit/Delete/Done）、
消费同一套 `ImeCommand`，Relay 与游戏内 ti3/im2 wire 层完全不感知差异。

| 后端 | 协议 | 适用环境 | 键盘路由 | 状态 |
|---|---|---|---|---|
| `wayland-ti3` | zwp_text_input_v3 客户端 | 游戏本体跑原生 Wayland | 宿主合成器自行处理（不接管） | ✅ |
| `dbus-ibus` | org.freedesktop.IBus DBus API | 与窗口系统无关，ibus 在跑即可（GNOME 默认） | ProcessKeyEvent 往返，接管+异步裁决 | ✅ |
| `dbus-fcitx5` | fcitx5 DBus 前端 | fcitx5 用户 | 同上（复用机制） | 规划中 |
| `x11-xim` | XIM / GLFW 内置 XIC commit | 传统 X11 会话 | 待定 | 规划中 |

### 探测顺序

```text
wayland-ti3（需要 GLFW wl_display ≠ 0 且宿主暴露 text_input_manager_v3）
  └─ 不可用 → dbus-ibus（session bus 存在且 org.freedesktop.IBus 有主）
       └─ 不可用 → dbus-fcitx5 → x11-xim → Unsupported（附完整探测报告）
```

每一步的失败原因都写入 `waylandcraft-ime.log`；全部失败时汇总成一份
探测报告，「为什么没有输入法」永远有答案。

### dbus-ibus 的按键路由（零阻塞）

需要原始按键的后端在 `bridge.keyboard_input` 里通过
`HostImBackend::submit_key` 接管按键：调用方**立即吞下**，后端内部完成
ProcessKeyEvent 异步往返后在下一帧 `poll` 里裁决 —— 消费则丢弃，
放行则按提交顺序补投递给焦点应用。渲染线程零阻塞、零竞态，
代价是按键最多晚一帧（≤16ms）到达应用。

### dbus-ibus 能力协商与信号

- Capabilities = PREEDIT | AUXILIARY | LOOKUP_TABLE | PROPERTY | FOCUS |
  SURROUNDING_TEXT (0x3F)。
- 信号映射：CommitText→Commit、UpdatePreedit*→Preedit、
  DeleteSurroundingText→Delete、HidePreeditText→空 Preedit、
  ForwardKeyEvent→注入应用。每条信号后立即补 Done，保证 Relay 原子应用。
- 信号体解析为容错式（定位变体/结构体内的文本与游标字段），不硬编码
  IBus 内部序列化细节；无法识别的形态记日志并安全丢弃。
- 候选窗由宿主输入法框架绘制（gnome-shell 的 IBus 面板），光标位置经
  SetCursorLocationRelative 反向同步。

### 已知边界

- dbus-ibus 后端要求宿主运行 IBus（GNOME 默认；KDE 需用户改用 ibus 或
  等 dbus-fcitx5 后端）。
- ibus 进程僵死时其回复不会到达，被接管的按键将不再投递（如实失败，
  不猜测语义）；连接失效会进入 TRANSIENT 重试链路。

