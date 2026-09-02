# Troubleshooting

用户报告问题及解决方法汇总。

---

## IME: firefox / chromium 在 waylandcraft 里输不进中文

### 现象

- 启动 1.2.5+ 版本 `/wl launch firefox`，在 firefox 的输入框里按字母
- 字母（拼音）能输入到 firefox，**汉字不出现**
- 日志 `waylandcraft-ime.log` 里所有 `ProcessKeyEvent` 都是 `consumed=false`
- 调 `wl ime diagnostic` 看到 ti3 协议正常握手（firefox enable + commit）
- 但 ibus 引擎不响应 — 永远不进入拼音预编辑模式

### 根因（已通过联网调研确认）

**ibus 的 global focus state 是按 host session 维护的。**

waylandcraft 嵌套在用户的桌面合成器里（mutter / sway / KDE Plasma 等）。
host 合成器看不到 waylandcraft 内部的 focus 变化。

具体路径：
```
Minecraft 窗口在 host 合成器 (mutter) → user 直接看到
firefox 跑在 waylandcraft 自己的 wayland-1 (嵌套) → 内部看到
```

ibus 引擎（ibus-libpinyin）决定是否进入预编辑模式，依据是 **host session** 的 focus state：

- mutter session 的 firefox（原生的）→ ibus 看到 focus → 拼音激活 ✅
- waylandcraft 内部的 firefox → mutter 不知道它的存在 → ibus 看不到 focus → 拼音不激活 ❌

`waylandcraft` 调 `host_bridge.submit(ProcessKeyEvent)` 给 ibus，
ibus **接到按键**但**引擎没激活** → 返回 `consumed=false`。
**这是 wayland ecosystem 层面的限制**（同 nested wayland + IME 在 sway / KDE / mutter 都未解决 — 见 [IME_RESEARCH_CONCLUSIONS.md](IME_RESEARCH_CONCLUSIONS.md)）。

### 验证

`wl ime diagnostic` 命令会显示：
- `[ime] FocusIn（set_focus 路径）` ← 发出 ✅
- `[host_bridge][dbus-ibus] ProcessKeyEvent ... -> consumed=false` ← 失败 ❌

如果**看到**前者 + **没看到** `[ime][host_bridge] handle_signal: CommitText`，
确认是这个问题（不是 waylandcraft 自己的 bug）。

### 解决方法

#### 方案 1：用稳定版 v1.2.4（推荐）

**v1.2.4 是当前推荐的 stable**。崩溃修复是稳定的，IME 部分行为和 v1.2.5+
**一样**（嵌套 IME 限制是生态问题，单 mod 改不动）。直接用 v1.2.4 避免 v1.2.5+
release page 顶部那条"嵌套 IME 限制"警告噪声。

下载：<https://github.com/scapking/waylandcraft/releases/tag/v1.2.4>

> **v1.2.5-v1.2.14 都**有"嵌套 IME 限制"问题——只有 v1.2.4 之前的 release 行为相同。
> v1.2.4 之后改的 (v1.2.5+): ti3 server 重启 + SetSurroundingText 类型修复 + satellite restart。
> 这些修的都是 waylandcraft 自己的协议逻辑——issue #1 嵌套 wayland + ibus focus
> state 隔离是 mutter/ibus/portal 三方问题，**单 mod 改不动**。
>
> **所以 v1.2.4 是最好的选择**——所有崩溃 bug 修了，所有 IME 限制保持原样。

#### 方案 2：在 host session 直接跑 firefox

最干净的方案。**不**用 waylandcraft 启动 firefox，而是：

```bash
# 1. host session 直接启动 firefox
firefox &

# 2. 找到 firefox 窗口的 wl_handle / xid（用 wayland-info 或 xwininfo）
# 3. 用 waylandcraft /wl add 把它当外部窗口加到 waylandcraft
wl add <firefox_handle>
```

这样 firefox 是 mutter 的直接 client → mutter ti3 → ibus 标准路径，IME 工作正常。

#### 方案 3：用 X11 应用

waylandcraft 启动 X11 应用（XWayland fallback）时，XIM 自动接 ibus：

```bash
/wl launch xterm    # XIM 接 ibus ✅
/wl launch gedit    # XIM 接 ibus ✅
/wl launch firefox --disable-features=UseOzonePlatform  # 强制 X11 ✅
```

注：firefox 强制 X11 后 GTK 工具栏字体可能变丑，但 IME 工作。

#### 方案 4：等 wayland 生态修复

mutter 49.2+ 已经在朝多 context IME 方向走。
如果 mutter 50.4+ 修了 `clutter: Let logical key events from IMs go through without a device`（MR !5121）后**嵌套 wayland 客户端**也能 work，但需要 mutter 进一步做 portal NotifyFocus 集成。
这个不在 waylandcraft 控制范围。

---

## 其他问题

（暂无）

---

## 报告新问题

- 提交 issue：<https://github.com/scapking/waylandcraft/issues/new>
- 附上：
  - **`waylandcraft/status.log`**（v1.2.9+）—— 单一文件包含所有子系统状态，**最优先**看这个
  - `waylandcraft-ime.log`
  - `latest.log.tail`（崩溃报告）
  - 你的 session 信息（`echo $XDG_SESSION_TYPE $WAYLAND_DISPLAY $XDG_CURRENT_DESKTOP`）
  - 你装的 waylandcraft 版本（`unzip -p mods/waylandcraft*.jar fabric.mod.json | grep version`）

### 一键打包诊断日志（v1.2.15+，需先 `apt install curl python3`）

```bash
# v0.13.11 改进版（推荐用这个）
bash waylandcraft/diag/wcdiag.sh
```

输出：`/tmp/wcdiag-<timestamp>.tar.gz` + catbox/tmpfiles URL。

**包含**（v0.13.11 改进）：
- `mods/waylandcraft*.jar`（v1.2.13 缺失——v0.13.11 修）
- `waylandcraft/satellite.log`（v0.13.10 satellite 子目录化）
- 系统诊断（wayland-scanner / Xwayland / java / glxinfo / gpu）
- catbox 失败自动 fallback tmpfiles

把脚本输出的 URL 贴到 issue 里。