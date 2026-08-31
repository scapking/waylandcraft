# IME 修复 - 联网调研笔记

## 用户实测环境
- `XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=wayland-0 DISPLAY=:0 XDG_CURRENT_DESKTOP=GNOME`
- 主 session 是 **mutter**（GNOME 49.2）
- 启动 waylandcraft（`/wl launch firefox-esr`），firefox 跑在 waylandcraft 自建的 `wayland-1`
- **Minecraft 不在 waylandcraft 里**——Minecraft 跑在 mutter 自己的 X11/XWayland fallback（GLFW 启动 "Wayland: Platform not initialized"）
- ibus-daemon + ibus-portal + ibus-engine-libpinyin + ibus-engine-simple 跑在 mutter session

## 关键观察
1. Minecraft 聊天框可以输汉字（GLFW 通过 XIM/X11 自动接 ibus-daemon）
2. firefox 在 waylandcraft 嵌套里——必须由 waylandcraft 提供 IME 协议（zwp_text_input_v3）才能接 ibus
3. v1.2.5 我重建了 ti3 server，wire 看到 firefox 创建 zwp_text_input_v3 instance 并调 enable/set_surrounding/set_cursor_rectangle/commit
4. apply_ti3_outcome 调 host_bridge FocusIn → dbus-ibus `FocusIn` 真的发出
5. **但 ibus ProcessKeyEvent 全部 consumed=false** — ibus 不认这个 client context

## 关键协议发现（wayland.app 官方文档）

### zwp_text_input_v3 协议要点

**Client → server requests**:
- `enable()` — `set_surrounding_text/set_content_type/set_cursor_rectangle` 之后
- `disable()` 
- `set_surrounding_text(text, cursor, anchor)` — text 是 utf8 字符串
- `set_text_change_cause(cause)` 
- `set_content_type(hint, purpose)` — `hint: 0=none, 1=completion, 2=spellcheck, 4=auto_cap, 8=lower, 16=upper, ...`
- `set_cursor_rectangle(x, y, width, height)` — 像素坐标
- **`commit()` — "Atomically applies state changes recently sent" — 关键！**

**Server → client events**:
- `enter(surface)` — 通知 client 焦点进入某 surface
- `leave(surface)`
- `preedit_string(text, cursor_begin, cursor_end)` — pre-edit 文本
- `commit_string(text)` — 提交文本
- `delete_surrounding_text(before, after)`
- **`done(serial)` — 应用 state 变更；serial 必须等于 client 发的 commit 请求数**

### 协议状态机
- 状态变更（enable/disable/set_*）是 **double-buffered** — 必须 commit 才应用
- 每次 commit 计数器 +1
- done 事件的 serial == 该实例的 commit count

### 关键引用
> "Protocol requests modify the pending state, as opposed to the current state in use by the input method. A commit request atomically applies all pending state, replacing the current state. After commit, the new pending state is as documented for each related request."
>
> "The compositor must count the number of commit requests coming from each zwp_text_input_v3 object and use the count as the serial in done events."

## Compositor Support（2025-2026 实测）

| 合成器 | ti3 支持 |
|------|---------|
| Cage | ❌ |
| COSMIC | ✅ |
| GameScope | ❌ |
| Hyprland | ✅ |
| Jay | ✅ |
| KWin | ✅ |
| Labwc | ✅ |
| Louvre | ❌ |
| Mir | ✅ |
| Muffin (Cinnamon) | ✅ |
| **Mutter (GNOME)** | ✅ |
| niri | ✅ |
| phoc (Phosh) | ✅ |
| river | ✅ |
| Sway | ✅ |
| Treeland | ✅ |
| Wayfire | ❌ |
| Weston | ❌ |

**重要**：用户的宿主合成器 **mutter 完全支持 ti3**。但用户的 firefox 不连 mutter，连 waylandcraft 自建 wayland-1。

## IBus vs fcitx5 vs ti3 关系

### 关键发现：kime-ibus 前端（PR #751）
搜索发现 kime 项目（Rust wayland IME）专门为 **GNOME Wayland** 创建了 **kime-ibus** 前端：
> "Add a new **kime-ibus** frontend: an IBus engine that forwards input to the kime engine. This is the path for environments where the Wayland input-method protocols are unavailable — notably **GNOME Wayland**, whose Mutter compositor does not implement `zwp_input_method_v2`, so `kime-wayland` cannot work there."

**关键洞察**：
- `zwp_input_method_v2` (im2) — Mutter 不实现，但 ti3 实现
- **im2 协议** = ibus-daemon 当 client 接 compositor（反方向）
- **ti3 协议** = application 当 client 接 compositor（正方向）
- 两个协议配套：im2 给 ibus 接，ti3 给 application 接
- **Mutter 实现 ti3 但不实现 im2** — 因为 GNOME 有自己的 internal IME API

### 这意味着 waylandcraft 的正确做法
**不是实现 im2**（kime 的项目不实现 im2 是因为 Mutter 不支持——waylandcraft 不需要 im2）。
**只实现 ti3**（让 firefox 接入 mod → mod 通过 dbus-ibus 直接调 ibus）—— **这正是 v1.2.5+ 我做的路径**。

**但 kime 项目为了 GNOME Wayland 创建 kime-ibus 前端**——他们也觉得 ti3+dbus-ibus 路径不通，必须搞 kime-ibus 才走通。
**这暗示** ti3 → dbus-ibus 路径在 GNOME + waylandcraft 这种嵌套场景下**确实有问题**。

## 关键问题：为什么 ibus 拒收 waylandcraft 的 ProcessKeyEvent？

### 假设 1：ibus context 冲突
waylandcraft 通过 `CreateInputContext("waylandcraft")` 创建的 ibus context 是**全局的**——和 firefox 自己的 ibus context（通过 ibus-portal）**不同**。ibus 引擎（ibus-libpinyin）可能按 client context 路由 commit/preedit。

**问题**：waylandcraft 创建了一个 ibus context，但**这个 context 的 focus 切换**没和 firefox 自己的 context 同步。所以 firefox 自己的 input box 仍以为它在 firefox 自己的 ibus context 上。

### 假设 2：missing enter event
firefox 通过 ti3 创建了 zwp_text_input_v3 instance 并 enable，但 waylandcraft 是不是漏发了 **enter event** 给 firefox 的 instance？

waylandcraft 1.2.5 日志显示：`keyboard focus switched (enter new surface)` 触发一次，但 firefox 多次 enable/disable 同 surface（firefox 在 chrome 切换焦点时反复 enter/leave）。**`ti3.enter` 内部 early-return 防止重复 enter**——firefox 在 re-enter 没收到 enter 时不会 enable，导致 firefox 自己的 input state 不同步。

但 firefox 实际**调了 commit**（v1.2.6 日志），说明 firefox 至少在某时刻是 enable 状态。

### 假设 3：ibus portal focus state
ibus-portal 维护**全局焦点状态**。mutter 的 input panel 状态**完全由 ibus-portal 决定**。waylandcraft 在 nested wayland session 里，**没有通过 portal 注册 focus**——所以 mutter 的 ibus panel 不知道有 focus change，input panel 仍显示 "no focused input"。

**这是最可能的根因**：
- mutter 的 ibus candidate window 是基于 portal 给的 focus state 显示的
- waylandcraft 是 nested compositor，**不会**主动调 `org.freedesktop.portal.Desktop` 的 `NotifyFocus` 或 `SetGlobalShortcuts`
- 所以 mutter 看到 waylandcraft 的窗口（firefox）时，ibus 的"现在在 input 模式"状态没激活
- 用户在 mutter session 角度看：firefox 窗口**不是** active input — firefox 也不显示 candidate window
- waylandcraft 自己的 dbus-ibus 调 `ProcessKeyEvent` —— 但 ibus 引擎（libpinyin）看的是 portal 的 focus state，**没焦点就不消费** → `consumed=false`

## 验证这个假设的方法

**关键实验**：用 `dbus-send` 调 ibus-portal 的 `NotifyFocus` 让 mutter 知道 firefox 在输入。

```bash
# 1. 看 ibus-portal 状态
dbus-send --session --print-reply --dest=org.freedesktop.portal.Desktop \
  /org/freedesktop/portal/desktop org.freedesktop.DBus.Properties.GetAll \
  string:org.freedesktop.portal.Focus

# 2. waylandcraft 应该在 firefox enable ti3 时调 portal NotifyFocus
#    或至少：让用户手动运行 dbus-send 测试 mutter 看到 focus 后能否输入
```

**这个实验能立刻验证假设 3 是否正确**——但**用户操作**，不是我能做的。

## 联网继续找资料

我需要：
1. 找 GNOME Mutter 文档关于 "input method panel focus state" 
2. 找 ibus-portal NotifyFocus API
3. 找 kime-ibus 是怎么解决"kime-wayland 走 ti3 但 ibus 引擎不响应"的问题
4. 找 wlroots/sway 等已知能 work 的合成器是怎么处理"嵌套 wayland session 里的 IME"

## 不确定的点

- 我对"ibus context 隔离"的假设可能不准确——ibus 引擎按 ProcessKeyEvent 的 keycode 处理，不按 context 名字
- ime-portal 假设需要验证——但**没有 mutter source 看** 很难 100% 确认
- waylandcraft 可能是**第一个**在 mutter nested wayland session 里跑嵌套 wayland 客户端的项目，**没有现成参考**

## 推荐方案（v0.14 改写）

不调 im2 / 不动 mutter / 接受 mod 没法当 IME 引擎的限制。

**方案 A**：**让 waylandcraft 在 firefox enable ti3 时调 ibus-portal `NotifyFocus`**，让 mutter 知道嵌套窗口有 text input focus
- 优点：保留嵌套架构
- 缺点：portal API 不熟，可能有权限问题

**方案 B**：**firefox 直接在 mutter 里跑**（不再嵌套在 waylandcraft）
- 优点：firefox 走 mutter ti3 + mutter's internal IME → 直接工作
- 缺点：失去"waylandcraft 内显示 firefox 窗口"的能力

**方案 C**：**waylandcraft 自己实现 mutter-style 的 ime manager**（GNOME 有自己的 at-spi 桥接——但这个复杂度爆炸）
- 不推荐

**推荐 A + 文档化**——waylandcraft 可以尝试调 portal 让 mutter 知道嵌套 firefox 是 input focus；不 work 就用文档告诉用户：在 mutter 主 session 里跑 firefox，不要嵌套。