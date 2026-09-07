# 同类项目 & 外部 IME / Wayland 补丁 landscape

> 来源：RenderCraft 调研期间（2026-08-2026-09）对周边 Minecraft
> Fabric + Wayland IME 项目的梳理。本文件只收录对 **waylandcraft
> 上游本身** 有参考价值的部分（同类 fork、IME 解决方案对比、
> Minecraft 26.x 趋势）；RenderCraft 自身的协议抽象 / CLI 重构
> 等不属于本仓关心范围。
>
> **定位**：让你在排查"为什么别人这样做而 waylandcraft 不"时
> 能快速找到对应项目和 PR。

---

## 1. WaylandCraft 生态 fork / 衍生项目

| 项目 | 状态 | 主要内容 |
|------|------|----------|
| `YourSandwich/waylandcraft` | 维护中 | Xwayland 支持的独立 fork。X11 应用在 Minecraft 中展示为窗口，输入/焦点/剪贴板/拖放双向桥接。默认按键：V = app launcher、G = 键盘捕获、B = 窗口管理器。 |
| `Skycrafter-dev` / `PedroLuisBrilhadori` / `PainDeMie64/waylandcraft-extended` | 已弃置 | 曾加了 Xwayland、Steam/Proton 工作流、可配置普通+硬键盘捕获、真实 Wayland 客户光标 surface、Escape 键释放、X11 focus/stacking/全屏缩放修复、dmabuf 纹理生命周期修复。**教训**：这些功能最终上游合并或替代了它们——说明要有稳定的扩展点，而非散落在各 fork 的 patch 里。 |
| `jdkeke142/glfw-wayland-minecraft` + `WayFix` | 维护中 | LWJGL 的 GLFW 的 Wayland 问题修补集（分数缩放、光标、输入），配套 mod 修多屏全屏定位、分数缩放下的点击偏移等。测试于 MC 1.21.11 + Fabric + KDE Plasma 6.3.6 + NVIDIA。 |
| `bczhc/glfw` | 已过时 | GLFW 的一堆 patch，包括 IME 支持（`glfwSetPreeditCallback` / `glfwSetIMEStatusCallback` / `GLFW_IME` 模式 / `glfwSetPreeditCandidateCallback`），还有 X11 on-the-spot 输入法风格支持。注明 Minecraft ≥ 26.1 不需要某些补丁了——**Minecraft 在改善 IME，原来的 workaround 正在失效**。 |
| `0x484558/glfw-mc` | 维护中 | 小幅 Wayland 修复和改进，重基了 IME 支持 PR，改进输入修饰符剥离、滚动事件去重、窗口管理容错等。 |
| `moehreag/wayland-fixes` / `Wayland Fix (Modrinth)` | 维护中 | 宣称"no-compromises Wayland 兼容"，处理图标、输入法候选栏等，让游戏在 Wayland 下更原生。注意沙箱（flatpak）路径授权。 |

### 关键教训

- **多个 fork 重复做同一类事**（Xwayland 整合、输入法、鼠标捕获、窗口管理）— 后来有的被上游合并、有的弃用。
- waylandcraft 上游合并/替代了大部分功能（Xwayland → satellite、IME → ti3 rebuild、dmabuf → native bridge）— 路线选择是对的。

---

## 2. Minecraft IME 现状

### 2.1 Minecraft 自身的问题与趋势

- **MC-306616**：IME 无法在 Linux 上切换回英文
- **Minecraft 26.1 snapshot** 陆续有 IME 候选栏在游戏内显示的尝试（目前 Windows / macOS），社区在讨论 Linux 是否也跟进
- **Minecraft 26.3 Snapshot 4** 开始用 SDL3 替代 GLFW，Linux 下原生偏向 Wayland — 意味着游戏底层输入/文本输入 API 在变化，是个契机，也是个要 watch 的点

### 2.2 第三方 mod 的解决思路（核心参考）

- **`NLR-DevTeam/Fcitx5-Enhancer`** — 针对 Minecraft 26.1+，提供 Fcitx5 兼容
  - 核心问题：按键既是游戏快捷键又是输入法按键（如 Tab、Enter）时，事件被输入法和游戏同时处理，导致中断
  - 解法：提供可高度配置的 **IMBlocker（可视化元素选择器）**，把冲突键从游戏侧拦截/协调，同时增加原生 Wayland 环境的 IME 支持（可选 `libwayland_support.so`）
  - 使用本地库实现功能，给了 x86_64 glibc 2.31 的内置库，其他架构需自编译

### 2.3 waylandcraft 的相对位置

- waylandcraft 提供嵌套 wayland session，让用户能在 Minecraft 内启动 firefox / VSCode 等应用；这一类应用的 IME 走 waylandcraft 的 `zwp_text_input_v3` / `zwp_input_method_v2` 路径
- 嵌套 wayland + ibus 的组合是生态限制（见 [TROUBLESHOOTING.md §IME](../TROUBLESHOOTING.md) + [IME_RESEARCH_CONCLUSIONS.md](../IME_RESEARCH_CONCLUSIONS.md)）— waylandcraft 的 IMBlocker 思路可以借鉴但**不是同一个问题**

---

## 3. Wayland 协议 / 抓帧相关参考项目

| 项目 | 用途 |
|------|------|
| `Smithay/smithay` | Rust Wayland compositor 构建模块；示例 compositor: anvil / smallvil。waylandcraft 的 native 侧协议接口模式直接借鉴了 Smithay 的设计。 |
| `ext-image-capture-source-v1` / `ext-image-copy-capture-v1`（Smithay 文档） | Wayland 窗口图像捕获协议。Smithay docs/wayland/image_capture_source/。未来 waylandcraft 如果想做"客户端主动 grab 别人窗口"，应当走这个协议而不是 portal ScreenCast。 |
| `dominikh/xcapture` | 命令行 X11 窗口录制工具，按窗口 ID 捕获，支持帧率和尺寸控制。X11 端抓帧参考实现。 |
| `AndreyBarmaley/xcb-window-capture` | 基于 ffmpeg / xcb / pulseaudio 的 X11 窗口录制。证明 xcb 抓帧完全可行。 |

---

## 4. 音视频参考项目

| 项目 | 借鉴点 |
|------|--------|
| `Discord Go Live` 架构 | 多进程管道：streamer / viewer / backend 三端协调；捕获有 fallback 系统；服务端+客户端各 4-8 秒缓存；自适应编码器；音画同步走 RTP 分别发送、接收端同步。 |
| `Discord-RE/Discord-video-stream` | WebRTC + VP8/H264 + NVENC 硬件编码；自适应比特率控制；缓冲策略不要无限增长缓存（避免延迟飞升）。见 `PERFORMANCE.md`。 |
| `baoayano2/discord-selfstream` | 生命周期状态机；自动恢复（FFmpeg 失败、RTC 损失、网关重连）。见 `docs/PLAN.md`。 |
| `Tky567/stream-discord-rs` | Rust 实现；DAVE/E2EE、WebRTC ICE/DTLS-SRTP、所有视频编解码器、硬件编码器（NVENC/VA-API）、AV 同步（PTS 时间戳）。 |

---

## 5. CLI 设计范式参考

- **Brigadier**（Mojang 的树形命令库）— `waylandcraft` 的 `/wl ...` 命令已经用 Brigadier 树形结构组织
- **celestialfault/commander** — Kotlin 库，用 `@Group/@RootCommand/@Command` 注解把函数定义转成 Brigadier 命令，减少手写树的乏味工作
- **itzmetanjim/commander** — Kotlin DSL，可从命令文件生成，支持客户端/服务端区分

---

## 6. 教训

1. **同类项目 fork 多了会反复实现同一件事** — 上游要主动合并/替代，否则社区分裂
2. **IME 是生态问题** — MC-306616 + 嵌套 wayland + ibus focus state 隔离，三个独立维度，单 mod 改不动
3. **MC 26.3 换 SDL3 是大事件** — 之前所有 GLFW patch 都可能失效，需要重新评估
4. **不要把"客户端协议抓帧"和"进程级音视频流"混在一起** — 分两个项目做（Discord-video-stream vs stream-discord-rs）— waylandcraft 把 capture / audio 分两个 native 模块也是对的