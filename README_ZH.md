<p align="center">
  <h1 align="center">🎮🪟 WaylandCraft</h1>
  <p align="center"><b>在 Minecraft 里运行真实的 Linux 桌面应用。</b></p>
  <p align="center">
    <a href="README.md">English</a> · <a href="README_ZH.md">中文</a> · <a href="README_JA.md">日本語</a>
  </p>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Minecraft-26.1.2-green" />
  <img src="https://img.shields.io/badge/Fabric%20Loader-0.19.2+-blue" />
  <img src="https://img.shields.io/badge/Fabric%20API-0.147.0%2B-blue" />
  <img src="https://img.shields.io/badge/Java-25-orange" />
  <img src="https://img.shields.io/badge/Platform-Linux%20%28capture%2Bshare%29-lightgrey" />
  <img src="https://img.shields.io/badge/Platform-Win%2FmacOS%2FAndroid%20%28viewer%29-lightgrey" />
  <img src="https://img.shields.io/badge/Version-v0.9.16-brightgreen" />
  <img src="https://img.shields.io/badge/License-MIT-blue" />
</p>


> ⚠️ **免责声明** — 本项目基于原版 [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) 二次开发，多人共享等功能由 AI 实现。**功能与安全性不做任何保证**，请自行承担使用风险。

---

## 目录

- [✨ 特性](#-特性)
- [🗺️ 平台支持](#️-平台支持)
- [🚀 快速开始](#-快速开始)
- [📖 使用指南](#-使用指南)
- [📚 命令参考](#-命令参考)
- [⚙️ 配置](#️-配置)
- [🎨 光影兼容](#-光影兼容)
- [💡 技巧与最佳实践](#-技巧与最佳实践)
- [❓ 常见问题](#-常见问题)
- [🚧 已知限制](#-已知限制)
- [🏗️ 从源码构建](#️-从源码构建)
- [🧱 架构](#-架构)
- [🤝 贡献](#-贡献)
- [📜 变更日志](#-变更日志)
- [📄 许可证](#-许可证)

---

## ✨ 特性

### 🖥️ 在游戏里运行真实 Linux 应用

把任意 Wayland 窗口变成游戏内物体：启动应用、在虚拟屏幕上查看、与之交互——全程不用退出 Minecraft。

- **纯命令行模式** — 所有操作通过 `/wl` 命令完成；回归原版渲染，无科幻 UI 干扰
- **统一渲染** — 本地窗口与远程共享窗口走同一渲染路径，画面完全一致
- **窗口自由摆放** — 拖动、缩放、固定、隐藏、旋转；模板一键保存/恢复布局
- **完整键盘穿透** — 单键、组合键（Ctrl/Shift/Alt）、长按 REPEAT 全部直达焦点窗口；捕获分工 **G=纯键盘、J=键盘+鼠标**
- **X11 应用支持** — 内置 `xwayland-satellite` 自动为 X11 程序提供 `DISPLAY`（已包含 `x86_64` 与 `arm64` 双架构二进制）

### 👥 多人窗口共享

把桌面实时串流给队友，作为一等公民的服务端特性原生实现，而非事后补救。

- **实时共享** — 其他玩家在游戏世界里看到你的窗口
- **手机观看** — Android 客户端零配置即可查看共享窗口
- **服务端中继** — 帧转发移出 Server 线程；多窗口共享按窗口分片到多个工作线程，一个慢的观众不会阻塞其他窗口
- **自适应画质** — 可配置缩放、JPEG 质量、帧率、码率，内置多档预设

### 🔐 精细化权限

每个玩家 × 每个窗口的四级权限模型：

| 级别 | 含义 |
|------|------|
| `NONE` | 窗口对玩家不可见、不可知 |
| `VIEW` | 可在世界中看到共享窗口 |
| `INTERACT` | 可向窗口发送鼠标/键盘事件 |
| `CONTROL` | 可调整大小、位置并管理权限 |

支持白名单、黑名单与按窗口覆盖。

### ⚡ 性能工程

- PBO 异步回读 + GPU 缩放（`glBlitFramebuffer`）
- 差异帧传输：只编码变化区域
- 空闲窗口心跳帧；`PNG`/`JPEG` 按透明需求自动切换
- 超限帧只降 JPEG 质量——**UI 尺寸永远不变**（含透明像素的窗口超限时强制转 JPEG 并把 alpha 合成到黑色背景上，保证降级真正生效）

### 🎨 光影兼容

自动检测 Iris 并回退到原版实体渲染管线——开光影也能正确显示窗口，始终全亮度。

---

### 🗺️ 平台支持

| 平台 | 捕获本地窗口 | 查看共享窗口 | 下载文件 |
|------|:---:|:---:|---|
| Linux x86_64 | ✅ | ✅ | `waylandcraft-linux-x86_64.jar` |
| Linux arm64 | ✅ | ✅ | `waylandcraft-linux-arm64.jar` |
| Android x86_64（仅查看） | ❌ | ✅ | `waylandcraft-android-x86_64.jar` |
| Android arm64（仅查看） | ❌ | ✅ | `waylandcraft-android-arm64.jar` |
| Windows x86_64（仅查看） | ❌ | ✅ | `waylandcraft-windows-x86_64.jar` |
| Windows arm64（仅查看） | ❌ | ✅ | `waylandcraft-windows-arm64.jar` |
| macOS x86_64（仅查看） | ❌ | ✅ | `waylandcraft-macos-x86_64.jar` |
| macOS arm64（仅查看） | ❌ | ✅ | `waylandcraft-macos-arm64.jar` |
| iOS arm64（仅查看，实验性） | ❌ | ✅ | `waylandcraft-ios-arm64.jar` |
| 独立服务器（任意架构） | — | —（承载 give/共享/权限逻辑） | `waylandcraft-server.jar` |
| 通用（任意平台，兜底） | ✅ | ✅ | `waylandcraft-universal.jar` |

> **一个平台一个 jar。** 从 [Releases](https://github.com/scapking/waylandcraft/releases) 下载对应你设备的文件——每个平台 jar 只携带自己平台的 native 组件，因此 Windows/macOS/iOS 是精简的仅查看 jar（约 0.4 MB），自动检测平台并禁用本地捕获。不确定选哪个？`waylandcraft-universal.jar` 打包了全部 native 平台作为兜底；独立服务器可用轻量的纯 Java `waylandcraft-server.jar`。

- **完整模式（Linux）** — 可捕获、共享、查看。
- **仅查看模式（Android / Windows / macOS）** — 安装对应平台的 jar；mod 自动检测平台、禁用本地捕获、继续接收共享窗口。
- **iOS（仅查看，实验性）** — 通过 [PojavLauncher](https://github.com/PojavLauncherTeam/PojavLauncher_iOS)（或其继任者 [Amethyst](https://github.com/AngelAuraMC/Amethyst-iOS)）在 iOS 上运行 Minecraft Java 版 + Fabric，安装 `waylandcraft-ios-arm64.jar` 即仅查看模式。尚未实机验证。
- **服务器** — 独立服务器安装 `waylandcraft-server.jar`（纯 Java，无 native 负载）；服务端的 give/共享/权限逻辑也内置在每一个其他 jar 里，所以服务端装客户端 jar 同样可用。
- **通用** — `waylandcraft-universal.jar` 打包全部 native 平台（linux-gnu + android × x86_64/arm64）。不确定自己平台时用它；体积比单平台 jar 大（约 6 MB）。

---

## 🚀 快速开始

### 前置要求

- Minecraft **26.1.2**（Java 版）
- Fabric Loader **0.19.x** + Fabric API **0.147.0+26.1.2**
- **Java 25**
- 完整模式需要 Linux **Wayland** 会话（捕获依赖 Wayland；仅 X11 的会话不支持）——Windows/macOS/Android 为仅查看模式，见 [平台支持](#️-平台支持)

### 安装

1. 安装 Fabric Loader 与 Fabric API。
2. 从 [Releases](https://github.com/scapking/waylandcraft/releases) 下载对应你平台/架构的 jar（见 [平台支持](#️-平台支持)）放入 `.minecraft/mods/`——桌面 Linux 用 `waylandcraft-linux-x86_64.jar`，手机用 `waylandcraft-android-arm64.jar`，Apple Silicon 用 `waylandcraft-macos-arm64.jar`。
3. **多人模式：服务端也必须装 mod**（give/权限/共享逻辑在服务端；不装则这些功能静默失效）。
4. 启动游戏——单机世界内嵌服务端，共享同一个 `mods/` 目录。

### 第一步

```text
/wl launch firefox              # 启动应用（或按 V 键）
/wl list windows                # 列出窗口；行尾 4 位随机码即窗口别名
/wl give <handle>               # 把窗口变成物品；右键长按放置到世界
/wl grab <handle>               # 抓取窗口拖动（G 键切换键盘捕获）
/wl share start <handle>        # 把窗口共享给队友
```

> 💡 **手机观看？** 安装 `waylandcraft-android-<arch>.jar`（多数手机为 arm64）并加入服务器——共享窗口自动出现，无需任何配置。

---

## 📖 使用指南

### 窗口管理

| 操作 | 命令 |
|------|------|
| 列出可启动应用 | `/wl list` · `/wl list apps` |
| 列出窗口 | `/wl list windows` |
| 捕获桌面窗口 | `/wl capture` |
| 启动应用 | `/wl launch <app>` |
| 窗口变为物品 | `/wl give <handle>` |
| 取回窗口 | `/wl take <handle>` |
| 抓取/拖动窗口 | `/wl grab <handle>` |
| 在世界中显示/隐藏 | `/wl show <handle|all>` / `/wl hide <handle|all>` |
| 固定（常显） | `/wl pin <handle>` / `/wl unpin <handle>` |
| 关闭应用 | `/wl close <handle>` |
| 调整大小 | `/wl resize <handle> <w> <h>` |
| 查看位置 | `/wl pos <handle>` |
| 移动（绝对值或 `~` 相对值） | `/wl move <handle> <x> <y> <z>` |
| 旋转（角度） | `/wl rotate <handle> <angle>` |
| 列出 X11 桌面窗口 | `/wl x11 list` |
| 直接共享 X11 窗口 | `/wl x11 share <index>` |
| 停止 X11 共享 | `/wl x11 stop <handle>` |

**句柄格式** — `<handle>` 支持：`0x` 短句柄、完整句柄、**实例别名**（4 位随机，如 `k7xq`，来自 `/wl list windows`，本次会话内唯一）、应用别名（如 `firefox_esr`）。同一应用多窗口用 `别名:N`（如 `firefox:2`）。

### 布局模板

| 命令 | 作用 |
|------|------|
| `/wl template save <name>` | 保存当前布局（临时，重启丢失） |
| `/wl template savep <name>` | 保存永久模板（应用+位置+分辨率，落盘） |
| `/wl template apply <name>` | 恢复临时模板 |
| `/wl template applyp <name>` | 应用永久模板：自动启动应用并按位置摆放 |
| `/wl template list` | 列出全部模板 |
| `/wl template remove <name>` / `removep <name>` | 删除临时/永久模板 |

### 自动布局（立方体 / 球体）

窗口围绕固定的初始化原点自动排列（不再跟随玩家）。默认关闭。

| 命令 | 作用 |
|------|------|
| `/wl layout init [<x> <y> <z> [<yaw>]]` | 初始化布局中心+朝向（无参数=玩家位置） |
| `/wl layout cube` | 切换立方体模板（4 面 × 每面 N 个窗口）并启用 |
| `/wl layout sphere` | 切换球体模板（VR 幕墙环，向上堆叠）并启用 |
| `/wl layout on` / `off` / `toggle` | 启用 / 禁用 / 切换自动布局 |
| `/wl layout status` | 显示模板、中心、半径、间距、核心窗口 |
| `/wl layout list` | 列出布局内窗口（`➤` 标记核心窗口） |
| `/wl layout add <handle>` / `remove <handle>` | 手动添加/移除窗口（`layoutAutoJoin` 关闭时） |
| `/wl layout core <handle>` | 显式指定核心窗口 |

* `Ctrl` + 方向键移动**核心标记**到该方向的相邻窗口——任意窗口都能成为核心（左右环绕、上下跨层、无限制）。核心窗口在游戏内以青色轮廓高亮。自动布局关闭时，`Ctrl` + 方向键仍可手动移动悬停窗口。
* `G` 捕获键盘；默认 `H` 键切换光标（均可在原版按键设置中改绑）。

### 共享

| 命令 | 作用 |
|------|------|
| `/wl share start <handle>` | 开始共享窗口（`all` / `*` = 一键共享全部） |
| `/wl share stop <handle>` | 停止共享（`all` / `*` = 停止全部） |
| `/wl share quality <handle> <s> <q> <fps>` | 设置缩放/质量/帧率 |
| `/wl share preset <handle> <preset>` | 应用预设（见 [配置](#️-配置)） |
| `/wl share config <handle> <param> <value>` | 调整单个参数 |
| `/wl share reset <handle>` | 恢复默认 |
| `/wl share info <handle>` | 查看当前共享配置 |
| `/wl share resolution <handle> <w> <h>` | 设置目标分辨率 |
| `/wl share stats <handle>` | 查看共享统计 |

### 权限

| 命令 | 作用 |
|------|------|
| `/wl permission list` | 列出全部权限 |
| `/wl permission default <PERM>` | 设置默认权限 |
| `/wl permission allow <player> <PERM>` | 白名单玩家 |
| `/wl permission deny <player>` | 黑名单玩家 |
| `/wl permission remove <player>` | 移除玩家 |

`PERM`：`NONE` / `VIEW` / `INTERACT` / `CONTROL`

---

## 📚 命令参考

### 设置

| 命令 | 作用 |
|------|------|
| `/wl settings list` | 查看当前设置 |
| `/wl settings set <key> <value>` | 修改设置 |

### 共享画质参数

`/wl share config <handle> <参数> <值>` 支持：

| 参数 | 说明 | 范围 |
|------|------|------|
| `scale` | 分辨率缩放 | 0.1 – 1.0 |
| `quality` | JPEG 质量 | 0.1 – 1.0 |
| `fps` | 最大帧率 | 5 – 120 |
| `bitrate` | 最大码率（kbps） | 0 = 不限 |
| `diffThreshold` | 像素变化阈值 | 0.001 – 1.0 |
| `diff` | 差异帧传输开关 | true / false |
| `buffer` | 帧缓冲数 | 1 – 8 |
| `latency` | 延迟补偿（ms） | 0 – 500 |
| `prediction` | 运动预测开关 | true / false |
| `compression` | 压缩方式 | 如 `lz4` / `zlib` / `none` |

### 预设

| 预设 | 缩放 | 质量 | 帧率 | 码率 |
|------|------|------|------|------|
| `performance` | 0.25 | 0.5 | 60 | 1000 kbps |
| `balanced` | 0.5 | 0.7 | 30 | 2000 kbps |
| `quality` | 1.0 | 1.0 | 30 | 不限 |
| `lowlatency` | 0.35 | 0.6 | 60 | 1500 kbps |

### X11 窗口共享

直接共享 X11 桌面的窗口（经 `xwayland-satellite`），无需 Wayland 顶层窗口：

| 命令 | 作用 |
|------|------|
| `/wl x11 list [display]` | 列出 X11 桌面窗口（默认 satellite 显示器） |
| `/wl x11 share <index>` | 共享列表中第 N 个窗口 |
| `/wl x11 stop <handle>` | 停止共享 X11 窗口 |

### 全局设置

`/wl settings set <key> <value>` 支持以下全部键（`/wl settings list` 亦可查看）：

| 键 | 默认值 | 说明 |
|----|--------|------|
| `pixelsPerBlock` | `500` | 每方块窗口像素密度 |
| `windowAntialiasing` | `false` | RGSS 抗锯齿（仅无光影时） |
| `focusOnHover` | `false` | 悬停自动聚焦窗口 |
| `hideCursor` | `false` | 控制窗口时隐藏虚拟鼠标光标 |
| `layoutEnabled` | `true` | 布局默认开启（v0.2.37 行为；未初始化时自动用玩家位置初始化） |
| `layoutAutoJoin` | `true` | 新窗口自动加入布局（false = 只排 `/wl layout add` 手动指定的窗口） |
| `layoutTemplate` | `cube` | 布局模板：`cube` 或 `sphere` |
| `layoutInitialized` | `false` | 是否已执行 `/wl layout init`（未初始化布局不可用） |
| `layoutInitX` / `Y` / `Z` | `0.0` | 布局中心坐标 |
| `layoutInitYaw` | `0.0` | 布局朝向（度，0=朝+Z，顺时针） |
| `layoutRadius` | `6.0` | 布局半径（格，中心到窗口水平距离） |
| `layoutSpacing` | `0.4` | 同层窗口最小水平间距（格） |
| `layoutStackSpacing` | `0.4` | 层间垂直间距（格） |
| `layoutCubePerFace` | `2` | 立方体模板每面窗口数（4 面共 8 个） |
| `layoutDefaultWidth` | `1080` | 加入布局的窗口自动调整到的分辨率 |
| `layoutDefaultHeight` | `540` | 加入布局的窗口自动调整到的分辨率 |
| `groundClearance` | `0.4` | 窗口底部距地面最小净空（格） |
| `moveStep` | `0.5` | Ctrl+方向键手动移动步长 |

---

## 🎨 光影兼容

- 开启 Iris 光影时，窗口走**原版实体渲染管线**、全亮度渲染——不受光影光照影响。
- 未开光影时使用自定义管线，可选 RGSS 抗锯齿（`windowAntialiasing`）。
- 两种模式下窗口正面贴纹理、**背面纯黑**——行为完全一致。

---

## 💡 技巧与最佳实践

- **窗口保持垂直** — 窗口始终竖直；拖动时锁定高度轴，底部永不低于地面 **0.4 格**。`Ctrl+滚轮` 旋转朝向（仍保持垂直）。
- **精确定位** — 先用 `/wl pos <handle>` 读取当前姿态，再用 `/wl move` 设置精确坐标（支持 `~` 相对偏移），用 `/wl rotate` 设置朝向。
- **服务端必须装 mod** — 多人模式下 `give` / `permission` / `share` 依赖服务端逻辑；服务端没装 mod 时请求会被静默丢弃。
- **圆角/阴影处轻微锯齿属正常**——JPEG 压缩所致；透明窗口会自动改用 PNG 保留 alpha。
- **游戏内完整帮助**：`/wl help`。

---

## ❓ 常见问题

**问：为什么服务端也必须装 mod？**
答：`give`、权限与共享逻辑注册在服务端。服务端没装 mod 时这些功能会静默失效。

**问：手机能看共享窗口吗？**
答：能。安装 `waylandcraft-android-<arch>.jar` 并加入服务器即可。PC 队友共享的窗口会自动出现，手机端无需额外操作。

**问：X11 应用能用吗？**
答：能。`xwayland-satellite` 已内置进 jar（`x86_64` 与 `arm64` 双架构），X11 程序会自动获得 `DISPLAY`。系统仍需 `Xwayland`——几乎所有 Wayland 桌面都自带。

**问：支持 Windows / macOS 吗？**
答：支持仅查看模式。安装同一份 `waylandcraft.jar`——mod 自动检测平台、禁用本地窗口捕获、仍可接收共享窗口。iOS 可用 PojavLauncher/Amethyst 运行 Java 版 + Fabric（实验性，未实机验证）。

**问：共享画面模糊/卡顿，怎么提升？**
答：调高画质或帧率：`/wl share quality <handle> <缩放> <质量> <帧率>`，或直接应用 `quality` 预设。默认是均衡档；画质降级时 UI 尺寸保持不变。

**问：开光影后窗口透明/黑屏？**
答：已自动检测 Iris 并回退原版管线。请确认所有客户端与服务端使用同一版本（≥ v0.2.32）。

**问：和上游 WaylandCraft 有什么区别？**
答：本 fork 在原版基础上（AI 辅助）实现了多人窗口共享、权限体系、纯命令行模式、手机观看以及上文所述的性能/画质工程。

---

## 🚧 已知限制

1. **窗口移动为受控模式** — 窗口固定垂直放置、拖动时锁定高度轴（保证底部高于地面 0.4 格），这是有意的简化设计；如需更自由的摆放方式可后续扩展。
2. **共享画质与延迟权衡** — 为保持与共享端一致的 UI 尺寸，超限时只降 JPEG 质量、不降分辨率；高分屏窗口在弱服务器/手机上仍有转发与解码压力。

---

## 🏗️ 从源码构建

```bash
# 前置要求：Java 25、Rust 工具链、Wayland 开发库
apt install libwayland-dev libxkbcommon-dev pkg-config libclang-dev

# 1. 编译 Rust 原生库（必须用 release 构建，build.gradle 会优先打包 release .so）
source ~/.cargo/env
cd native && cargo build --release

# 2. 编译 Java mod
cd .. && ./gradlew clean build

# 输出：build/libs/waylandcraft.jar（约 6.0MB，内置 x86_64 与 arm64 双架构 xwayland-satellite）
```

> ⚠️ 发布前务必确认打包的是 `native/target/release/libwaylandcraft.so`（约 3.7MB）。若误打包 debug 构建（176MB 带调试符号），jar 会膨胀到 39MB。

> 📦 内置的 `xwayland-satellite` 二进制由 `native/build-satellite.sh` 构建——分别对 `x86_64` 和 `arm64` 各执行一次（arm64 交叉编译需 `aarch64-linux-gnu-gcc` + `cargo build --target aarch64-unknown-linux-gnu`），然后把两个二进制都放到 `native/` 下。

---

## 🧱 架构

```text
┌─────────────────────────────────────────────────────────────┐
│                       Minecraft 客户端                       │
│  ┌──────────────┐   ┌───────────────────┐   ┌────────────┐  │
│  │  窗口视图     │   │  WindowShare      │   │  /wl CLI   │  │
│  │  (渲染)      │◄─►│  (捕获/发送)      │◄─►│  (命令)    │  │
│  └──────┬───────┘   └────────┬──────────┘   └────────────┘  │
│         │  PBO/GPU 回读      │ Fabric Networking API        │
└─────────┼────────────────────┼──────────────────────────────┘
          │                    │
┌─────────┼────────────────────┼──────────────────────────────┐
│         ▼                    ▼            Minecraft 服务端   │
│  ┌─────────────────────────────────────────────┐            │
│  │  SharedWindowManager（权限、状态）            │            │
│  └──────────────────────┬──────────────────────┘            │
│                         │ 帧                                │
│  ┌──────────────────────▼──────────────────────┐            │
│  │  SharedWindowFrameRelay（工作线程分片）       │            │
│  └──────────────────────┬──────────────────────┘            │
└─────────────────────────┼────────────────────────────────────┘
                          │ 广播
          ┌───────────────▼────────────────┐
          │  观看端（PC 或 Android 客户端）  │
          │  异步解码 → 世界内渲染          │
          └────────────────────────────────┘
```

| 层级 | 技术 |
|------|------|
| 游戏 | Java 25, Fabric Loader 0.19.2+, Fabric API 0.147.0+ |
| 原生桥接 | Rust, JNI |
| Wayland | Smithay, wayland-client, wlr-foreign-toplevel-management |
| 图像 | PBO 异步回读, glBlitFramebuffer, JPEG/PNG, MemoryUtil |
| 网络 | Fabric Networking API, 自定义 Payload 协议 |

---

## 🤝 贡献

欢迎贡献！这是 [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) 的 fork，多人功能由 AI 实现——粗糙之处在所难免。

- 通过 [GitHub Issues](../../issues) 报告 bug 与功能请求
- 通过 [Pull Requests](../../pulls) 提交修复与改进
- 保持 Java 与 Rust 两侧都能构建：在 `native/` 下跑 `cargo build --release`，仓库根目录跑 `./gradlew build`
- 修改用户可见行为时，同步更新 README（EN/ZH/JA）

---

## 📜 变更日志

完整历史见 [Releases](https://github.com/scapking/waylandcraft/releases) 页面。

**近期亮点：**

- **v0.9.16** — 新增 `/wl show all` / `/wl hide all`（也支持 `*`）一键显示/隐藏全部窗口；`hide all` 一并解除钉住。
- **v0.9.15** — 布局排序在窗口关闭/新增后实时更新：序号紧凑重排填补空洞。
- **v0.9.14** — Ctrl+方向键改为与相邻窗口交换位置（swapCore），并修正左右方向对调。
- **v0.9.13** — Ctrl+方向键恢复为移动鼠标指向的窗口（manualOffset 不被每帧布局重排覆盖）。
- **v0.9.12** — `/wl share start all` 一键共享全部窗口（`stop all` 同理）；Ctrl+方向键切换布局核心标记。
- **v0.9.11** — 修复键盘穿透总根因：`xkb_state.update_key` 误用 evdev 键码（`key-8`）更新状态，但 xkbcommon 要求 xkb keycode（evdev+8）。无效键码被静默忽略 → Ctrl/Shift/Alt 修饰位永远置不上 → 单键看似正常、组合键（Ctrl+L 等）全部失效。kb.log 中"Ctrl 按下但 mods(depressed=0)"实锤。**组合键穿透现已完全正常。**
- **v0.9.10** — 修复 `setKbLogFileNative` JNI 注册名不匹配（Rust 宏 snake→camel 自动生成 vs 显式命名）。
- **v0.9.9** — Rust 键盘日志独立写文件 `waylandcraft-kb.log`（setKbLogFile），便于上传定位 Rust 侧焦点/发送状态。
- **v0.9.8** — 修复键盘穿透主根因：`correctScancode` 在 Wayland 平台去掉 +8（键码双重偏移）。
- **v0.9.7** — 日志降噪；`keyboard_key` 每键打印修饰键状态；tick 焦点日志节流；`keyboard_focus` 幂等静默。
- **v0.9.6** — 键盘穿透全链路 debug 日志（mixin 入口/onKeyPress 分支/本地转发/bridge/Rust 焦点/每键发送）。
- **v0.9.5** — 修复本地窗口键盘穿透（场景 B）：焦点兜底 + 转发自愈 + 诊断日志。
- **v0.9.4** — 修复 Ctrl+方向键方向相反 + G 绑定下 J 键误触发。
- **v0.9.3** — 共享窗口长按 REPEAT 透传补全（`forwardSharedKey` Repeat 分支，需求 1 补全）。
- **v0.9.2** — Ctrl+方向键改为交换布局排序（方案 A）——无任何范围限制。
- **v0.9.1** — 修复 G 键绑定后按键全失效——绑定/悬停时设置键盘焦点（focusSurface）。
- **v0.9.0** — 键盘输入子系统重构（方案 C）：长按 REPEAT 事件完整透传（修复长按失效）；组合键/大小写由 Rust xkb 状态机全权维护，Java 只做透传；Ctrl+方向键**永远移动窗口**（恢复 v0.2.37 语义，布局核心切换解绑）；捕获分工 **G=纯键盘、J=键盘+鼠标**；release 自动生成按版本变更描述。
- **v0.2.35** — 新增 iOS 检测（PojavLauncher/Amethyst 运行时）：仅查看模式、同一份 jar、共享窗口可渲染；平台矩阵更新。
- **v0.2.34** — Windows/macOS 支持**仅查看模式**：自动检测平台并跳过本地捕获；同一份 jar 在 Linux/Windows/macOS/Android 通用；共享窗口仍可渲染。
- **v0.2.33** — 窗口实例别名改为 4 位随机码（如 `k7xq`），不再用 w1/w2…；剔除易混字符 `0/o/1/l/i`，更好输入。
- **v0.2.32** — 透明窗口强制 JPEG 降级（质量调节真正生效）；单帧上限 600 KB → 1.8 MB。
- **v0.2.31** — 服务端帧中继按窗口分片 N 线程（同窗口保序、异窗口并行）；注册/注销移出 netty 线程。
- **v0.2.30** — 帧转发整体移出 Server 线程；PBO 永久降级；默认 q0.85 / 10 fps。

---

## 📄 许可证

MIT License — 详见 [LICENSE](LICENSE)。

## 致谢

- [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) — 原版项目
- [Smithay](https://github.com/Smithay/smithay) — Wayland 合成器框架
- [Fabric](https://fabricmc.net/) — Minecraft 模组加载器
