# WaylandCraft 🎮🪟

**在 Minecraft 里运行 Linux 桌面应用** — 一个 Fabric mod，将 Wayland compositor 功能集成到 Minecraft 中，让玩家可以在游戏世界中查看和交互 Linux 桌面窗口，并支持多人窗口共享。

> ⚠️ 本项目基于 [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) 原始项目，多人显示等功能由 AI 辅助实现。**功能和安全性不保证稳定，请自行承担使用风险。**

<p align="center">
  <img src="https://img.shields.io/badge/Minecraft-26.1.2-green" />
  <img src="https://img.shields.io/badge/Fabric%20Loader-0.19.2+-blue" />
  <img src="https://img.shields.io/badge/Fabric%20API-0.147.0%2B-blue" />
  <img src="https://img.shields.io/badge/Java-25-orange" />
  <img src="https://img.shields.io/badge/Version-v0.2.32-brightgreen" />
</p>

---

## 下载

👉 **[最新 Release(v0.2.32)](https://github.com/scapking/waylandcraft/releases/latest)** — 下载 `waylandcraft.jar`，放入 `mods/` 文件夹即可。

> 上游仓库（almightydb）的 Releases 页面同步滞后，如需最新版本请从上述链接获取。

---

## 版本亮点（v0.2.32）

- **服务端多线程帧转发** — 帧按窗口句柄分片到 N 个线程（同窗口保序、异窗口并行）；注册/注销切服务端主线程执行，netty 线程不再被窗口列表广播阻塞。
- **降级真正生效** — 含透明像素的窗口不再因走 PNG 无损路径导致降级无效；共享帧超限时强制转 JPEG（透明混合黑背景），只降画质、不降 UI 尺寸。
- **单帧上限放宽** — 600 KB → 1.8 MB（对齐服务端协议上限），普通高分屏窗口不再被丢帧。

---

## 功能特性

| 功能 | 说明 |
|------|------|
| 纯命令行模式 | 移除科幻风格 UI，回归原版渲染风格，所有操作通过 `/wl` 命令完成 |
| 窗口世界化 | 将 Wayland 窗口显示在游戏世界中，可拖动、缩放、钉住、隐藏 |
| 统一渲染 | 本地窗口与远程共享窗口走同一渲染路径，显示效果一致 |
| 多人窗口共享 | 将窗口共享给其他玩家，在对方游戏世界中实时渲染 |
| 桌面窗口捕获 | XDG Desktop Portal + PipeWire 捕获桌面窗口 |
| 权限管理 | 四级：NONE / VIEW / INTERACT / CONTROL |
| 光影（Iris）兼容 | 检测到 Iris 时自动降级用原版管线渲染，开光影也能正常显示窗口 |
| 自适应画质 | 可配置缩放比例、JPEG 质量、帧率、码率，内置预设 |
| 性能优化 | PBO 异步回读、GPU 缩放、差分帧传输、静止心跳帧、PNG/JPEG 自动切换、服务端多线程帧转发（不占主线程） |

---

## 演示截图

<p align="center">
  <img src="assets/demo_1.jpg" width="49%" alt="Demo 1" />
  <img src="assets/demo_2.jpg" width="49%" alt="Demo 2" />
</p>

> 以上图片为演示示例。

---

## 安装

### 前置要求

- Minecraft **26.1.2**（Java Edition）
- Fabric Loader **0.19.x** + Fabric API **0.147.0+26.1.2**（或对应版本）
- **Java 25**
- Linux + **Wayland** 会话（原生库负责窗口捕获，X11 下不可用）
- **xwayland-satellite 已内置** — X11 应用自动获得 `DISPLAY`，无需手动安装（仍需要系统自带 `Xwayland`，几乎所有 Wayland 桌面都包含）。jar 内同时携带 **x86_64 与 arm64** 两种架构的二进制，ARM64 主机（如树莓派）同样开箱即用。

### 步骤

1. 安装 Fabric Loader 与 Fabric API
2. 将 `waylandcraft.jar` 放入 `.minecraft/mods/`
3. **多人游戏：服务端也必须安装本 mod**（`give` 物品发放、权限管理、共享窗口等逻辑注册在服务端；服务端未装 mod 时这些功能不生效——例如 `/wl give` 会静默失败）
4. 启动游戏（单人世界即内置服务器，客户端服务端共用 `mods/` 目录即可）
5. **Android 手机端（纯查看）**：安装 `waylandcraft-android-<arch>.jar`（无本地 Wayland 时自动禁用本地功能，不会崩溃）。加入装有本 mod 的服务器后，可**直接看到电脑队友共享的窗口**（`/wl share start <handle>` 由电脑端发起，手机端无需任何额外操作）

---

## 快速上手

1. 启动一个应用：`/wl launch firefox`（或按 `V` 键启动器）
2. 查看窗口列表：`/wl list windows`
3. 把窗口变成物品：`/wl give <handle>` → **右键长按物品**放置到世界中
4. 捕获键盘操作窗口：`/wl grab <handle>`（或按 `G` 键切换捕获/释放）
5. 共享给队友：`/wl share start <handle>`

### 按键

| 按键 | 功能 |
|------|------|
| `B` | 打开窗口管理器屏幕 |
| `G` | 捕获 / 释放键盘（抓取状态时按键直接透传给窗口） |
| `右键长按` + WindowItem | 在世界中显示窗口 |

> 所有其他操作均通过 `/wl` 命令完成，完整列表见下。

---

## 命令系统

`<handle>` 支持四种格式：`0x` 短句柄 / 完整句柄 / **实例别名 `wN`（`/wl list windows` 直接获得，会话内唯一）** / 应用别名（如 `firefox_esr`）；多个同名窗口可用 `别名:N` 指定第 N 个（如 `firefox:2`）。

### 窗口管理

| 命令 | 功能 |
|------|------|
| `/wl list` | 列出可启动的应用（默认） |
| `/wl list windows` | 列出 compositor 中的窗口 |
| `/wl list apps` | 列出可启动的应用 |
| `/wl list desktop` | 列出可捕获的桌面窗口 |
| `/wl launch <app>` | 启动应用（支持名称/精确别名；同前缀应用用完整别名区分，如 `visual_studio_code`） |
| `/wl capture` | 弹出 Portal 选择窗口，捕获桌面窗口 |
| `/wl give <handle>` | 把窗口变为物品放入背包 |
| `/wl take <handle>` | 从背包收回窗口物品 |
| `/wl grab <handle>` | 抓取窗口（鼠标在世界中拖动，滚轮前后移动） |
| `/wl show <handle>` | 在世界中显示窗口 |
| `/wl hide <handle>` | 从世界中隐藏窗口显示 |
| `/wl pin <handle>` | 钉住窗口（世界中保持显示，不受隐藏/最小化影响） |
| `/wl unpin <handle>` | 解除钉住 |
| `/wl close <handle>` | 终止应用进程（关闭窗口） |
| `/wl resize <handle> <w> <h>` | 调整窗口分辨率 |
| `/wl pos <handle>` | 查看窗口位置（x/y/z）、朝向角度、缩放、分辨率 |
| `/wl move <handle> <x> <y> <z>` | 设置窗口坐标（绝对如 `100.5`，或相对偏移如 `~0.5` / `~-1` / `~`） |
| `/wl rotate <handle> <angle>` | 设置窗口朝向角（度，绕 Y 轴；绝对如 `90`，或相对如 `~15`；`0`=朝+Z, `90`=朝+X） |
| `/wl template save <name>` | 保存当前区块内所有窗口布局为临时模板（重启失效） |
| `/wl template savep <name>` | 保存永久模板（app + 位置 + 分辨率，写入磁盘） |
| `/wl template apply <name>` | 应用临时模板，恢复窗口位置 |
| `/wl template applyp <name>` | 应用永久模板：自动启动应用并按记录放置 |
| `/wl template list` | 列出所有模板 |
| `/wl template remove <name>` / `removep <name>` | 删除临时 / 永久模板 |

### 共享管理

| 命令 | 功能 |
|------|------|
| `/wl share start <handle>` | 开始共享窗口 |
| `/wl share stop <handle>` | 停止共享 |
| `/wl share quality <handle> <s> <q> <fps>` | 设置画质（缩放、质量、帧率） |
| `/wl share preset <handle> <preset>` | 应用预设（见下） |
| `/wl share config <handle> <param> <value>` | 设置单个参数 |
| `/wl share reset <handle>` | 重置画质为默认值 |
| `/wl share info <handle>` | 显示当前共享配置 |
| `/wl share resolution <handle> <w> <h>` | 设置目标分辨率 |
| `/wl share stats <handle>` | 显示共享统计 |

### 权限管理

| 命令 | 功能 |
|------|------|
| `/wl permission list` | 列出所有权限 |
| `/wl permission default <PERM>` | 设置默认权限 |
| `/wl permission allow <player> <PERM>` | 加入白名单 |
| `/wl permission deny <player>` | 加入黑名单 |
| `/wl permission remove <player>` | 移除玩家 |

> `PERM`：`NONE` / `VIEW` / `INTERACT` / `CONTROL`

### 设置

| 命令 | 功能 |
|------|------|
| `/wl settings list` | 列出当前设置 |
| `/wl settings set <key> <value>` | 修改设置 |

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `pixelsPerBlock` | `500` | 窗口在世界中每方块对应的像素密度 |
| `windowAntialiasing` | `false` | 窗口渲染抗锯齿（RGSS，仅无光影时生效） |
| `focusOnHover` | `false` | 鼠标悬停窗口时自动获得焦点 |

### 共享画质参数与预设

| 参数 | 说明 | 范围 |
|------|------|------|
| `scale` | 分辨率缩放 | 0.1 – 1.0 |
| `quality` | JPEG 质量 | 0.1 – 1.0 |
| `fps` | 最大帧率 | 5 – 120 |
| `bitrate` | 最大码率 (kbps) | 0 = 无限 |
| `diffThreshold` | 像素变化阈值 | 0.001 – 1.0 |

| 预设 | 缩放 | 质量 | 帧率 | 码率 |
|------|-------|---------|-----|---------|
| `performance` | 0.25 | 0.5 | 60 | 1000 kbps |
| `balanced` | 0.5 | 0.7 | 30 | 2000 kbps |
| `quality` | 1.0 | 1.0 | 30 | 无限 |
| `lowlatency` | 0.35 | 0.6 | 60 | 1500 kbps |

---

## 光影（Iris）兼容

- 开着 Iris 光影也能正常显示窗口：检测到 Iris 时窗口自动改用**原版 entity 管线**渲染，内容始终满亮度（不受光影光照影响）
- 无光影时使用自定义管线，支持 RGSS 抗锯齿（`windowAntialiasing`）
- 两种模式下窗口**正面贴图、背面纯黑**，行为一致

---

## 使用提示

- **窗口固定垂直**：窗口始终竖直放置（不可倾斜），拖动时垂直轴（y）锁定、只能水平移动，且窗口底部不低于该位置地面之上 **0.4 格**；`Ctrl+滚轮` 可旋转朝向（保持竖直）
- **精确摆放**：`/wl pos <handle>` 查看当前位置/角度后，可用 `/wl move <handle> <x> <y> <z>` 精确设置坐标（支持 `~` 相对偏移），用 `/wl rotate <handle> <angle>` 精确设置朝向角（度）
- **服务端必须装 mod**：多人模式下 `give` / `permission` / `share` 等服务端功能依赖服务端安装 mod，否则请求会被静默丢弃
- 窗口圆角/阴影的轻微锯齿是 JPEG 编码的正常现象；含透明像素的窗口会自动改用 PNG 保留 alpha（共享帧超限时自动强制 JPEG 降级：透明像素混合黑背景，仅降画质、不降 UI 尺寸）
- 桌面捕获（`/wl capture`）依赖系统 XDG Desktop Portal，需要 Wayland 会话
- 完整命令帮助可在游戏内查看：`/wl help`

---

## 已知不足（待完善）

当前版本仍有以下不完善之处，将在后续版本中持续改进：

1. **窗口移动为受控模式** — 窗口固定垂直放置、拖动时锁定高度轴（保证底部高于地面 0.4 格），这是有意的简化设计；如需更自由的摆放方式可后续扩展。
2. **共享画质与延迟权衡** — 为保持与共享端一致的 UI 尺寸，超限时只降 JPEG 质量、不降分辨率；高分屏窗口在弱服务器/手机上仍有转发与解码压力。

---

## 构建

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

## 技术栈

| 层级 | 技术 |
|------|------|
| 游戏 | Java 25, Fabric Loader 0.19.2+, Fabric API 0.147.0+ |
| 原生桥接 | Rust, JNI |
| Wayland | Smithay, wayland-client, wlr-foreign-toplevel-management |
| 图像 | PBO 异步回读, glBlitFramebuffer, JPEG/PNG, MemoryUtil |
| 网络 | Fabric Networking API, 自定义 Payload 协议 |

---

## 许可证

MIT License

## 致谢

- [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) — 原始项目
- [Smithay](https://github.com/Smithay/smithay) — Wayland compositor 框架
- [Fabric](https://fabricmc.net/) — Minecraft mod loader
