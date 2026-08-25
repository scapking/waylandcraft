# WaylandCraft 输入法（IME）架构

> 本文档描述 v0.9.27+ 的输入法实现。该版本是一次**协议层重构**：
> 删除了旧的 text-input-v1 / input-method-v1 路径与全部 workaround，
> 以标准现代协议栈（zwp_text_input_v3 + zwp_input_method_v2）为核心重建。

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
cd native && cargo test          # 全部 21 个测试
cargo test --lib ime::           # 仅 IME 子系统（19 个）
```

两层测试：

- **relay 单元测试**（9 个）：纯逻辑状态机的 serial 链、丢弃语义、
  端点重连、焦点丢失、组合全流程。
- **线缆级集成测试**（10 个，`ime/tests.rs`）：真实 `Display<WLCState>`
  （无 GPU 模式，dmabuf 关闭）+ 两个真实 wayland-client 连接——
  「编辑器」（ti3 客户端）与「模拟 fcitx5」（im2 客户端），覆盖：
  英文原始键路径（seat 测试）、enable 激活、逐键拼音组合、组合中退格、
  组合中移动光标、候选选定提交、选区删除重组保序、过期 serial 丢弃、
  焦点 A→none→A、enable/disable/enable 循环、键盘 grab 分流与释放、
  穿透入站保序应用、穿透出站反向同步。

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
2. **X11/XWayland 后端不支持宿主穿透**：结构性限制——自建连接没有
   wl_surface，宿主的 enter 需要 client/surface 关联。启动时检测并给出
   明确诊断（建议原生 Wayland 会话）。这不是可修复的 bug，是能力边界。
3. **游戏内 IBus（端点 A）不可用**：ibus 引擎客户端仅实现了 im-v1；
   现代栈下请用 fcitx5 直连游戏合成器，或直接使用穿透路径（推荐，
   无需在游戏内运行任何输入法进程）。
