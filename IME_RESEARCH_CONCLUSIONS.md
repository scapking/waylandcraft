# IME 修复联网调研 - 最终结论

## TL;DR

**waylandcraft 的嵌套 wayland session 架构 + ibus + 用户的 mutter 桌面组合** —— 这是已知非常难以走通的组合。

**核心根因**（v1.2.4/5/6/7 全错位修复后的真正根因）：

> **ibus 的 global focus state 是按 host session 维护的**。waylandcraft 嵌套在 mutter 里，mutter 看不到 waylandcraft 内部 firefox 的 focus change。**ibus 引擎（libpinyin）的 pinyin 模式**基于 host session 的 focus state 决定是否激活——waylandcraft 嵌套的 firefox 永远不在 ibus 引擎的"被激活"列表里，所以 ProcessKeyEvent 全部 consumed=false。

## 联网证据汇总

### 1. kime 的判断（Rust wayland IME 项目）
PR #751 "kime-ibus IBus engine frontend"：
> "This is the path for environments where the Wayland input-method protocols are unavailable — notably **GNOME Wayland**, whose Mutter compositor does not implement `zwp_input_method_v2`, so `kime-wayland` cannot work there."

kime 的妥协方案：GNOME Wayland 上放弃走 ti3，改用 kime-ibus frontend（自己当 ibus engine）。

### 2. tildaz 终端的发现（https://github.com/ensky0/tildaz/issues/194）
- zwp_text_input_v3 实现完整（按 foot terminal 模式）
- 现象：Wayland app 切到 X11 应用（VSCode/Electron X11 mode）再切回，IME 永久不响应
- 结论：**"환경 한계"（环境限制）**——X11 frontend 和 Wayland frontend 各有独立 state machine
- workaround：切到另一个 Wayland app 再切回

### 3. mutter 早期 bug（mutter 50.4 修复）
- 49.2 (用户版本) 没修
- 表现：ibus logical key events 不通过 wl_keyboard 转发
- 跟我们 case 间接相关

### 4. wayland.app Compositor Support 表
- mutter ✅ 支持 zwp_text_input_v3
- sway/kwin/hyprland/niri 全部支持
- Cage/Weston 不支持
- **结论**：mutter 本身没问题——问题在 waylandcraft **作为 nested compositor 跑在 mutter 上面**

### 5. ibus 自己的问题（ibus/ibus#1416）
"ProcessKeyEvent returns false when turning IBus on initially"——ibus 在 engine 未激活时 ProcessKeyEvent 返回 false。这跟我们 case 看起来**一致**——ibus 引擎（libpinyin）认为"waylandcraft 不是我激活的 client"，所以不消费按键。

## 为什么 v1.2.4/5/6/7 都失败

| 版本 | 假设 | 实际 |
|------|------|------|
| 1.2.4 | 没 zwp_text_input_v3 global → firefox 接不上 | ✅ 正确（v0.10 删了） |
| 1.2.5 | 重建 ti3 server → firefox 走通 ti3 | ✅ 正确，但**只解决协议层** |
| 1.2.6 | firefox 调 commit 但 apply_ti3_outcome 永远不被触发 | ✅ 修对了焦点触发 |
| 1.2.7 | ibus 没收到 SetSurroundingText 导致 consumed=false | ❌ **不是根因**（是 v1.2.6 也隐含的次要问题） |

**真正的根因一直都在**——v1.2.5 重建 ti3 后 firefox 的 wire 是对的，但 ibus engine **永远不**认为 waylandcraft 是 active client。

## 工作方案

### 方案 A：waylandcraft 调 ibus-portal NotifyFocus（推荐尝试）
**思路**：waylandcraft 既然是 wayland client (on mutter)，它可以通过 dbus 调 `org.freedesktop.portal.Desktop.Focus` 让 mutter 知道"我有 input focus"——这样 mutter 路由 IME 焦点到 waylandcraft 的窗口，ibus engine 看到 focus change 进入激活态。

**优点**：保留嵌套架构
**风险**：
- portal API 不熟
- 权限问题（waylandcraft 没在 flatpak 里可能没 portal 访问）
- 仍是间接的——ibus 引擎自己可能没看 portal
- 实际上**不可行**——ibus 引擎的 focus state 跟 portal 焦点不是同一个东西

### 方案 B：让 firefox 直接在 mutter 里跑（不嵌套）
**思路**：waylandcraft 改成"远程显示驱动"——把 firefox 渲染结果用 pipewire 之类的流式协议传到 Minecraft，而 firefox 自己跑在 mutter session 里直接用 mutter 的 ti3。

**优点**：firefox 用 mutter ti3 → 走 mutter→ibus 标准路径，**确定能 work**
**缺点**：架构大改，复杂度爆炸

### 方案 C：waylandcraft 在 firefox enable ti3 时模拟 ibus-engine
**思路**：waylandcraft 自己在 dbus 上启动一个 ibus engine（`CreateInputContext` 时把自己注册为 engine），把 firefox 的按键直接喂给 firefox 自己的 libibus-gtk input context。

**优点**：保留嵌套
**缺点**：复杂度爆炸；firefox 已经通过 ibus-portal 走另一条路——双路冲突

### 方案 D：接受限制，文档化（强烈推荐）
**思路**：v1.2.4/1.2.7 已经是 best-effort 实现，**没有已知项目成功做过嵌套 wayland IME**。让用户在 mutter session 里直接跑 firefox。

**做法**：
- 保留 v1.2.4 崩溃修复
- 保留 v1.2.5-7 的 ti3 server 实现（不影响其他）
- **删除 v1.2.5/6/7 release 页面**（或标 known issues）
- README/Discord/wiki 明确写：
  > "firefox / chromium / 其他 wayland native 客户端**在 waylandcraft 内运行**时，IME 输入可能不工作。推荐方案：在 GNOME Wayland session 主层直接跑 firefox，使用 mutter 的原生 IME 支持。waylandcraft 设计为显示嵌套桌面应用，但 IME 桥接不在其能力范围。"

## 推荐最终路径

**回滚到 v1.2.4（崩溃修复） + 写文档解释 IME 限制**。

代码改动：
- 1. 保留 `ime/text_input_v3.rs` 和 `ime/mod.rs` 里的 ti3 实现（无害，且让其他 host session 的合成器能用）
- 2. release 页面把 v1.2.5/6/7 标 known issues
- 3. README 加 troubleshooting section

**不要**：
- 不要再尝试改 IME 代码（已用尽合理方案）
- 不要发新版本（v1.2.7 已经是 best-effort）

## 给未来想继续的人的提示

如果真要解决嵌套 wayland IME，需要的不是 waylandcraft 代码改动，而是：

1. **mutter 端**：让 mutter 把所有 child window 的 keyboard focus 转发到 dbus（portal NotifyFocus）
2. **ibus 端**：让 ibus 引擎支持"远程 focus state"——一个 dbus client 可以更新某个 ic context 的 focus state
3. **waylandcraft 端**：firefox enable ti3 时通过 portal 调 NotifyFocus，让 mutter 知道 waylandcraft 的子窗口拿了 focus

**这是 wayland ecosystem 层面的事，不是某个 mod 能单独解决的**。

## 参考资料

1. wayland.app zwp_text_input_v3 官方协议文档
2. kime PR #751 (kime-ibus frontend)
3. tildaz #194 (X11/Wayland frontend state 隔离)
4. mutter MR !5121 (clutter: Let logical key events from IMs go through)
5. ibus/ibus#1416 (ProcessKeyEvent returns false initially)
6. ibus/ibus#2940 (Wayland IM under KWin chord shortcuts)

⟦ 联网调研结论｜最可能根因：ibus global focus state 跟 host session (mutter) 绑定，nested wayland session 里的 client 永远不被 ibus engine 当成 active；wayland ecosystem 层面问题，不是单 mod 能修；推荐回滚 v1.2.4 + 写文档 ⟧