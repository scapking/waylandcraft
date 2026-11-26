# WaylandCraft 输入法（IME）架构

> 本文档描述 v0.9.27+ 的输入法实现。该版本是一次**协议层重构**：
> 删除了旧的 text-input-v1 / input-method-v1 路径与全部 workaround，
> 以标准现代协议栈（zwp_text_input_v3 + zwp_input_method_v2）为核心重建。
>
> **v0.14 更新（本文以「v0.9.27 重构摘要」一节为准的协议内核已恢复并沿用）**：
> v0.10 曾删除 im2/relay 路线改走 host dbus 穿透，但实测天花板已到——宿主
> 引擎的 focus 状态归宿主会话管理（GNOME/mutter 隔离），每个
> ProcessKeyEvent 恒返回 consumed=false，中文永远打不出（详见
> IME_RESEARCH_CONCLUSIONS.md）。v0.14 恢复 v0.9.27 的 im2+relay 架构，并
> 升级为**嵌套 IM 守护进程路线**：在我们的嵌套 WAYLAND_DISPLAY 上拉起一个
> 独立的 fcitx5/ibus 实例作为 IME 客户端，引擎状态完全归嵌套会话所有，
> 宿主 focus 隔离问题从根上消失。host_bridge（dbus 穿透）保留为**可选后备
> 路径**。完整设计与路由决策表见文末「v0.14 嵌套 IM 守护进程路线」。

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
| 3c | im2 事件收到即转发，不等 `commit(serial)` 校验 | 不匹配时合成器不得改变对象状态（**并非丢弃文本**——sync8 勘误：协议原文 "proceed as normal, except it should not change the current state"） | 「丢弃」时污染早已到达 App，丢弃机制形同虚设 |
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
| `ime/relay.rs` | **纯逻辑协议中继**：enable/disable 生命周期、IME 变更原子缓冲与 serial 校验裁决（对照显式传入的 wire 计数；不符不丢文本）。零 Wayland 类型依赖 | 不碰任何 wire 对象；不维护权威 done 计数（只读镜像除外） |
| `ime/text_input_v3.rs` | ti3 wire 层：manager/object Dispatch、double-buffer pending、per-instance commit 计数 | 不做语义裁决 |
| `ime/input_method_v2.rs` | im2 wire 层：对象管理、键盘 grab 种子化、popup 矩形上报、**per-代际 done 计数（commit serial 校验的唯一事实源）** | 不做语义裁决 |
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
App enable → Relay Activate → im2 activate+state+done（wire 计数 +1）
IME preedit/commit_string/delete + commit(serial)
  → Relay 对照 wire done 计数校验（单一事实源）→ 批次一律交付
    （sync8：serial 不符不再丢文本）→ ti3 事件 + done(commit计数)
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

- `commit(serial)` 与计数不符 → **照常交付文本，仅不改变该 im2 对象的
  current state**（协议原文 "proceed as normal, except it should not
  change the current state"）。sync7 及以前把它错读成「整批丢弃」，
  叠加双计数漂移后造成实机 51/51 汉字全部丢失——sync8 已修正。
- **计数单一事实源**：IME 侧 done 计数只由 wire 层
  （`input_method_v2.rs::Im2Instance::done_count`）维护——每次实际发出
  done 才 +1，对象换代/销毁即归零；`Relay::ime_done_count` 只是经
  `mirror_done_count` 同步的只读诊断镜像，不参与任何判定。IME 重连即从
  计数 0 开始（新对象新基准）。
- 穿透方向的 serial 校验由宿主合成器完成；到达 Relay 的批次视为已校验，
  通过无条件的 `ime_flush()` 应用。

### sync8 修正：serial 漂移与「永不丢文本」（2026-11-25 实机取证）

sync7 实机（`waylandcraft-ime.log` 16:29–16:48）：拼音、数字选词链路全通
（fcitx5.log 有候选），但 relay 打出 51/51 条 `im2 commit(serial=N)
DROPPED`，每个汉字都被丢弃——「伪可用」。两个叠加缺陷：

1. **双计数器漂移**：`relay.ime_done_count` 在命令*排队*时乐观自增，
   wire 层 `Im2Instance.done_count` 在 done *实发*时自增。三处「relay 记了
   数但 wire 没发」路径（`clear_focus` 末尾丢弃 focus_lost 返回的命令、
   `push_app_state` 返回值被 `let _ =` 吞掉、im2 对象顶替换代时 wire 归零
   而 relay 保留旧值）使两者永久错位，此后所有 commit 全灭。
2. **不符即丢文本违反协议**：serial 校验基准被错读。协议要求 mismatch 时
   文本照常处理、仅不改变对象 current state——relay 层近似无操作。

修复形态：done 计数单一事实源 = wire 层 per-代际计数（`ime_commit` 显式
传入校验；relay 侧只留 `mirror_done_count` 诊断镜像）；mismatch 批次照常
交付并打双计数 WARN 进 ⑥ error ring（相邻去重）；无内容裸 commit（fcitx5
`updatePreeditDelegate` 面板清空时的正常行为）单独安静记录，不再共用
DROPPED 文案。回归覆盖：relay 单测（wire 计数为 0/落后/镜像污染三形态）、
tests.rs 全事件序列 + serial 竞态 + 换代顶替、tests_real.rs 真 fcitx5
`real_fcitx5_serial_drift_still_commits`（在 SPACE 选词键事件之后注入一次
状态 done，确定化复现 serial 落后一拍）。

## 6. Preedit / Commit 工作方式

一个组合周期（以拼音 nihao → 你好 为例）：

1. IME 每步发送 `preedit_string("n"…"nihao") + commit(serial)`；
2. Relay 缓冲操作，`commit(serial)` 到达即整批放行（serial 与 wire 计数
   不符也放行——sync8 起永不丢文本，不符仅不做状态确认并出 WARN 诊断）；
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

- **relay 单元测试**：纯逻辑状态机的 serial 链、mismatch 永不丢文本
  （sync8 回归：wire 计数为 0 / 陈旧 serial / 镜像污染三形态）、空批与
  mismatch 可区分、端点重连、焦点丢失、组合全流程。
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

## v0.14 嵌套 IM 守护进程路线（Nested IM Daemon）

### 背景：为什么必须放弃"宿主穿透为主"

host dbus-ibus/dbus-fcitx5 路线在实测中的硬天花板：宿主桌面会话
（GNOME/mutter、KWin）自己管理 IME 的 focus 状态，waylandcraft 进程提交的
按键在宿主引擎里**永远不会让对应 input context 成为 focused**。实测每个
`ProcessKeyEvent` 都返回 `consumed=false`，引擎永不激活——英文/符号靠
raw 直通可用（sync4/5 修复），**中文始终打不出**。这是宿主会话隔离的
结构性后果，不是我们代码的 bug，无法在 dbus 客户端侧绕过。

结论：要兼容全部输入法、全部桌面端，**输入法引擎的所有权必须夺回嵌套
会话**——不再问宿主"这个键你消费吗"，而是在我们自己的合成器里跑一个
独立引擎。

### 架构

```
   嵌套 WAYLAND_DISPLAY（我们的 socket）
   ├─ 我们导出三个 global：
   │     zwp_text_input_manager_v3        （嵌套应用：firefox/GTK/Qt…）
   │     zwp_input_method_manager_v2      （嵌套 IME 客户端：daemon）
   │     zwp_virtual_keyboard_manager_v1  （IME 未消费键的直通回注通道）
   │
   ├─ 应用（ti3 client）──enable/commit──┐
   │                                     ▼
   │                             Relay（serial 记账/原子批次/丢弃判定）
   │                                     ▲
   ├─ 嵌套 daemon（im2 client）─commit───┘
   │     fcitx5（首选，5.1.12 实测 libwaylandim.so 含 im2 + VK 符号）
   │     或 ibus-daemon（独立 --address，前台由我们管；无 im2 时如实回退）
   └─ 宿主引擎不再参与中文组合
```

- `ime/input_method_v2.rs`：im2 服务器 wire 层（Manager/InputMethod/
  KeyboardGrab/Popup 四对象 Dispatch）。daemon bind 后按 grab→forward 送键，
  commit/preedit/delete/lookup 上行灌入 Relay，commit(serial) 做协议强制的
  serial 校验（不符整批丢弃）。
- `ime/virtual_keyboard.rs`（**新增**）：`zwp_virtual_keyboard_v1` 服务器。
  fcitx5 在引擎**未消费**某键时，不丢弃而是经 VK 重新注入合成器
  （`waylandimserverv2.cpp::sendKeyToVK/mayCommitAsText`）——本层把注入的
  key 落回 seat 原始路由。**没有它，grab 期间方向键/Esc/快捷键/裸字母
  全部被吞**。这是 sync4 "consumed 分流" 在 im2 通道上的对偶。
- `ime/nested_daemon.rs`（**新增**）：daemon 生命周期状态机 + 隔离
  spawn。纯逻辑（`decide`）与 spawn 计划（`build_spawn_plan`）可无进程
  单测；实 spawn 用 process.rs 的双 fork + setsid 惯例，孙 pid 写 pidfile。

### 隔离纪律（绝不污染宿主）

- 环境：`WAYLAND_DISPLAY`=我们的 socket；`HOME`/`XDG_CONFIG_HOME`/
  `XDG_DATA_HOME`/`XDG_CACHE_HOME`=`gameDir/waylandcraft/ime-nested`
  隔离目录；`XDG_RUNTIME_DIR` 沿用宿主（socket 文件在那）；**删除**
  `DBUS_SESSION_BUS_ADDRESS`/`DISPLAY`/`XAUTHORITY`。
- 参数：fcitx5 `-D --disable=dbus`（前台、禁 dbus addon，避免
  org.fcitx.Fcitx5 与宿主实例抢总线）；ibus 用独立 `--address=unix:path=…`
  且禁 panel/config/emoji 子进程。实测（fcitx5 5.1.12，`ime/tests_real.rs`）
  pinyin 引擎**不依赖** dbus——libpinyin 直链，`--disable=dbus` 下
  preedit/候选/commit 全通。
- **spawn 前种子配置 v2（`seed_fcitx5_home_v2`）——全部已装引擎默认兼容**：
  隔离 HOME 缺少 fcitx5 全局配置时，每个输入上下文都以"未激活"启动——
  fcitx5 `Instance::inputMethod(ic)` 在未激活时用**组内第一个 IM**
  （keyboard-us 直通引擎），这就是 sync6 实机"每键未消费、汉字永不出"
  的根因（`[Behavior] ActiveByDefault` 无显式默认值=false；取证链见
  `nested_daemon` 模块注释与 `tests_real` 三组对照实验）。v2 在此之上把
  默认组扩成**用户在宿主上装的全部引擎**：
  - 引擎发现：扫 `$XDG_DATA_HOME`/`$XDG_DATA_DIRS` 的
    `fcitx5/inputmethod/*.conf`（默认值与 fcitx5 standardpath.cpp 一致）。
    **引擎内部名=文件名 stem**（`inputmethodloader` 用
    `fileName.substr(0,len-5)` 作 uniqueName；conf 里 `[InputMethod] Name=`
    是显示名、语言键是 `LangCode=`），`Enable=False` 如实排除（如本机
    cangjie 出厂即禁用）。宿主用户目录（`~/.local/share`）发现的引擎
    会复制进隔离 `data/fcitx5/inputmethod`——嵌套实例的 XDG_DATA_HOME
    指向隔离目录，不复制就只在 profile 挂名。
  - profile 策略：宿主 `$XDG_CONFIG_HOME/fcitx5/profile` 存在且有效时
    **镜像其组/条目**（组名 ASCII 化、去重、Layout 细节不拷）；否则
    全量枚举默认组 `[keyboard-us, pinyin, …全部发现引擎升序]`，
    DefaultIM=pinyin（存在时）否则 keyboard-us。
  - 所有权标记 `# waylandcraft-seeded-v2`：带标记 → 启动时按最新发现
    重写（升级路径，保留 fcitx5 记下的当前引擎）；无标记但恰为旧
    v1/手种形态（{keyboard-us,pinyin} 集）→ 视为我们的旧种子升级；无标记
    但条目集等于上次种子结果（fcitx5 退出保存会重写 profile 并丢标记）→
    仍判 ours 重写；真正的用户自定义 → **绝不覆盖**，只在 ime.log 如实
    列出缺少的已装引擎。
  - `config/fcitx5/config`：`[Behavior] ActiveByDefault=True` +
    **`[Hotkey/EnumerateForwardKeys] 0=Control+Shift+B`**。两个取证要点：
    5.1.12 的 EnumerateForward/BackwardKeys **出厂默认为空**（"Ctrl+Shift+B
    循环引擎"不是内置绑定，内置的只有 Ctrl+Space 触发+重复按枚举）；
    且 fcitx5 的 KeyList 选项 INI 走 `vector<Key>` 子节点格式——必须写
    嵌套节 `[Hotkey/EnumerateForwardKeys]` + `0=`，扁平
    `EnumerateForwardKeys=Control+Shift+B` 会被解析为空列表（真机探针
    验证）。config 的所有权判定与 profile 同构；用户文件不覆盖，缺
    ActiveByDefault 如实警告。
- **游戏内切换引擎**：`Ctrl+Shift+B` 逐个切到组内下一个引擎（循环，
  回到 keyboard-us 档=停用）；`Ctrl+Space`（fcitx5 内置默认）激活/停用，
  配合重复按可枚举。组合键经 im2 grab 送达引擎并被消费时**绝不 VK
  直通**（wire 断言见 `nested_daemon::tests::ctrl_shift_b_chord_reaches_grab_with_modifiers`）。
- **绝不 `--replace`/`-r`**——那会踢掉用户的宿主输入法实例；单测
  `spawn_plan_is_isolated_and_never_replaces` 把这条纪律写死在断言里。

### 路由决策表

| 条件（每帧 `nested_tick` 裁决） | phase | im2 路由 | grab 键去向 | 无 grab 键去向 |
|---|---|---|---|---|
| settings `imeNested=off` | Disabled | 关 | host_bridge/raw | host_bridge/raw |
| 用户 `/wl ime nested stop` | Disabled(锁) | 关 | host_bridge/raw | host_bridge/raw |
| 配置未同步（Java 未注入 home） | Unconfigured | 关 | host_bridge/raw | host_bridge/raw |
| PATH 无 fcitx5/ibus-daemon | NoBinary | 关 | host_bridge/raw | host_bridge/raw |
| 已 spawn，等 im2 bind（≤10s） | Starting | 关 | host_bridge/raw | host_bridge/raw |
| **im2 客户端已 bind** | **Active** | **开** | **im2 grab** | **raw 直通（跳过 host）** |
| daemon 进程消失/连续 3 次超时 | Failed | 关（回退） | host_bridge/raw | host_bridge/raw |

- 探活主信号 = **im2 client bind**（比 pidfile 强：进程活着但连不上我们
  的 socket 没有意义）。
- bind 超时 10s、失败退避 5s、连续 3 次封顶停手（防 fork 炸弹），手动
  `start` 复位。全部决策行走 `ime.log` ⑥ 时间戳，失败进 ⑥ error ring。
- host_bridge（dbus 穿透）的路由门（**v0.14.1 收紧**）：
  `nested_route_live()` 为 true 时 `set_focus`/`clear_focus`/
  `apply_ti3_outcome` **完全不再向宿主提交** FocusIn/FocusOut/
  Surrounding/CursorRect——sync6 实机取证：嵌套主路径已激活，
  dbus-ibus 仍持续收到 FocusIn/Out（宿主输入法弹自己的候选窗、
  维持热焦点，纯干扰且语义含糊）。daemon 掉线回退后 route_live
  变 false，下一次 ti3 事件即恢复 v0.13 的双发喂法（submit 幂等，
  宿主状态自动补热）。wire 断言见 `tests::nested_live_suppresses_host_bridge_submits`。

### 命令与配置

- settings 键 `imeNested`（auto|on|off，默认 auto），`/wl settings set
  imeNested auto|on|off`（别名 `nestedIme`）；responder 注册即触发启动期
  同步。
- `/wl ime nested status` → mode/phase/kind/pid/im2/grab/vk 一行，种子执行
  后追加 `; engines=N`（N=生效引擎数，不含 keyboard-us 直通档）；
  `start` 复位重试；`stop` 杀 pidfile 进程 + 进 Disabled 锁。
- `status.log` 新增 `nested_ime` 子系统 + metrics `ime_route`
  （im2-grab|im2|host_bridge|raw）。ime.log 种子行输出发现引擎数量与
  列表（截断）、镜像/枚举策略、所有权判定结果。

### 端到端实测清单（逻辑与 wire 层已用模拟 client 全测）

1. **[已修]** 隔离 HOME 首次 profile/全局 config：spawn 前自动种默认
   （v2 起为**全部已装引擎**，见上文"种子配置 v2"；用户自定义文件不
   覆盖）。sync6 的"每键未消费"根因即缺 `[Behavior] ActiveByDefault=True`
   （IC 未激活 → keyboard-us 直通），与 dbus/协议细节无关。
2. **[已验证]** 真 fcitx5 端到端（`ime/tests_real.rs`，#[ignore]，
   本机跑通）：`real_fcitx5_production_spawn_path` 走完整生产链路
   nested_tick→种子→build_spawn_plan→spawn_daemon，nihao+空格 →
   preedit×5 + `commit("你哈登")` 经 im2→relay→ti3 回到编辑器；
   `real_fcitx5_seed_config_active_by_default` / `real_fcitx5_private_dbus`
   对照证明 ActiveByDefault 是唯一决定变量、私有总线形态行为一致。
3. **[已验证]** 种子 v2 多引擎切换（`real_fcitx5_seed_v2_all_engines_switchable`，
   本机跑通）：v2 种子 profile 带标记含 9 引擎、config 带嵌套节枚举热键
   → 真 fcitx5 默认拼音 commit → 6 次 `Ctrl+Shift+B` 切到 wbpy（五笔拼音）
   后同编码再次 commit → SIGTERM 退出保存 profile `DefaultIM=wbpy` 落盘
   （切换真实发生的硬证）；wire 层另有
   `nested_daemon::tests::ctrl_shift_b_chord_reaches_grab_with_modifiers`
   证明和弦 press+release 与 Ctrl+Shift 修饰态按序到达引擎 grab。
4. 待用户实测：游戏内（Minecraft 聊天框）打中文、切引擎。
5. ibus 路预计不 bind im2 → Starting 超时 → Failed 回退（如实上报，非 bug）。


## 内置拼音引擎（ime_source，「装 mod 即用中文」零依赖路径）

fcitx5 嵌套 daemon 路线要求宿主机装有 fcitx5/ibus-daemon。为把依赖降到
「装上 mod 就能打中文」，`ime/builtin/` 提供**纯 Rust 内置拼音引擎**：

- **词库**：`native/assets/ime_builtin_pinyin.bin`（4.14MB，11.4 万词条/
  420 音节/7.2 万节点的音节 trie），`include_bytes!` 内嵌 cdylib。来源与
  许可：jieba dict.txt（MIT，词频）+ pypinyin 0.55.0（MIT，单字/短语读音），
  生成管线与复现步骤见 `tools/ime-dict/README.md`。格式唯一事实源
  `ime/builtin/dict.rs`（编译工具 `native/tools/ime-dict/dictgen.rs`
  `#[path]` include 同一份代码）。运行时零拷贝加载（release parse ~0.2ms，
  在 WLCState::new init 线程完成，不占游戏 tick）。
- **引擎**：字母流在 trie 上枚举全部切分路径（nihao=ni+hao、xian=xi+an
  类歧义按词频裁决），完整覆盖候选优先、延长候选 top-k（128）有界堆 +
  子树最大频率剪枝，每键 13~255µs。键语义与 fcitx5 pinyin 对齐（数字/
  空格选词、左右移动高亮、PgUp/PgDn/-/= 翻页、退格编辑、回车原样提交、
  Esc 取消、撇号断句、裸 Shift 切西文、Ctrl/Alt/Super 组合永不消费）。
  局限：无句级联想/bigram；物理 US 布局假设（非 US 布局走 fcitx5 路线）。
- **键路由 seam**（`ime/mod.rs::handle_key` 单点）：`ime_source:
  Auto|Fcitx5|Builtin`（settings 键 `imeSource`，`/wl ime source <mode>`
  即时切档持久化）。Auto 档**仅当** nested daemon 判死（NoBinary/Failed）
  才回退内置——fcitx5 在场时行为与既有逐字节一致（回归红线）。内置批次
  走 relay→emit_ti_batch 既有出口（宿主穿透同族的无条件应用）；无激活
  ti3 会话不吃键；`/` 前导命令行语境直通。
- **候选窗**：内置无 wl surface 像素帧，改为**结构化候选文本**交付：JNI
  `imeBuiltinCandidates()`（仅变化时返回 JSON，空串=无新状态）+
  `imeBuiltinSelect/Page`。Java 侧 `ImeBuiltinPanel` 原版字体渲染，GUI 形态
  锚聚焦输入框（复用 positionOverlay/命中钳制），键盘捕获场景走 HUD 形态
  （屏幕底部居中）；与 fcitx5 像素帧模式互斥（来源档天然决定哪路产数据）。
- **可观测**：`/wl ime info` 首行 source/effective/词库规模/组合态；
  `/wl ime eng` 引擎清单（内置=拼音如实）；`status.log` 新增 `ime_builtin`
  子系统与 `ime_route=builtin` 取值；日志前缀 `[ime][builtin]`。
