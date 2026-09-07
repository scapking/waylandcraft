# Wayland / X11 / Minecraft 内部帧捕捉参考

> 来源：RenderCraft（`scapking/RenderCraft`，已于 2026-09-07 冻结）调研期间
> 对 `scapking/waylandcraft` 及周边 Minecraft Fabric 项目的实现阅读。原稿
> 包含 Java + native 代码级别的实锤，Mojang Forge Fabric / PipeWire
> / xdg-desktop-portal / JNA XGetImage / OpenGL framebuffer 几个关键路径
> 都覆盖了。
>
> **本文件状态**：历史沉淀。`PipeWireCaptureManager`（Java）+ native
> `portal_capture.rs` 已经覆盖了本文档中"Wayland 端"和"Minecraft
> 内部渲染侧"的实现路径；读这份文档的目的不是说要再做一遍，而是：
>
> 1. 知道每条路径的关键工程坑（避免再踩）
> 2. 当 native bridge 缺失 / libpipewire 不可用时，本仓库
>    `dev.evvie.waylandcraft.capture.fallback`（`WaylandPortalClient` +
>    `WaylandFrameGrabber`）提供的 dbus-send + grim PPM 路径就是基于
>    "Wayland 端" 那一节的设计，是 loom 1.13 主源集隔离环境的最小可行
>    fallback
> 3. 给未来的 `grim` / 进程级 fallback / 其他桌面环境适配留下设计依据

---

## 1. Wayland 端：XDG Desktop Portal ScreenCast + PipeWire

### 1.1 当前实现

- Java 入口：`dev.evvie.waylandcraft.capture.PipeWireCaptureManager`
- 原生入口：`WaylandCraftBridge.portalCaptureStart/Frame/Stop`（JNI）
- 原生实现：`native/src/portal_capture.rs`

### 1.2 流程

- `portalCaptureStart()` → D-Bus 调用
  `org.freedesktop.portal.ScreenCast.CreateSession/SelectSources/Start`
- 弹出系统选择对话框，用户选窗口后，拿到 PipeWire 节点 ID
- 原生侧用 `libpipewire` 连该节点，开独立线程读帧
- `portalCaptureFrame()` → `[width(4B), height(4B), rgba...]`
- `portalCaptureStop()` → 停掉 PipeWire mainloop / stream

### 1.3 关键细节

- 帧格式由 PipeWire 决定，可能是 BGRx / BGRA / RGBA
- native 侧统一转成 RGBA（每像素 4 字节，R,G,B,A）— 见
  `native/src/portal_capture.rs::convert_to_rgba`
- Java 侧拿到的是 `byte[]`，前 8 字节是宽高（小端），其余是像素
- PipeWire 读帧跑在独立线程里（`pw-stream`），Java 侧每 tick 取一次

### 1.4 Fallback 路径（loom 1.13 主源集隔离环境）

仓库的 `dev.evvie.waylandcraft.capture.fallback` 包提供了不依赖
libpipewire / native bridge 的最小 fallback：

- `WaylandPortalClient` 用 `dbus-send` 命令行工具驱动同一个
  `org.freedesktop.portal.ScreenCast` 接口（与 native 端功能等价）
- `WaylandFrameGrabber` shell-out 调 `grim -t ppm -`，纯 Java 解析
  PPM/P6 到 RGBA8
- 适用场景：native bridge 缺失 / libpipewire 0.3 缺失 / wlroots
  合成器（grim 可用）/ GNOME（grim 缺失但 dbus-send 仍可创会话）

### 1.5 易踩坑（已踩 / 已修）

- PipeWire 节点格式不固定，必须统一转换到 RGBA — native 侧已做
- Portal D-Bus 交互别用固定 token 硬编码推导路径，要从 Response
  信号里提取 session/node — `extract_session_handle` / `extract_node_id_from_response`
- gdbus monitor 别用 `timeout` 包完整流程，否则会极慢；应该用
  `grep -m1 Response` 类方式尽早退出 — 见
  `wait_portal_response` 的注释

---

## 2. X11 端：JNA XGetImage

> WaylandCraft 当前不在主路径里走 X11；X11 应用通过
> `xwayland-satellite` 走 XWayland，由 native bridge 桥接。这条
> 路径保留作为未来需要"独立 X11 抓帧"（比如 X11-only 服务器）
> 时的参考实现。

### 2.1 能力

- `X11Capture.captureRgba(displayName, xid)` → RGBA `ByteBuffer`（top-down）
- `X11Capture.getGeometry(...)` → 宽高 + 根窗口坐标
- `X11WindowLister.getDesktopWindows(...)` → 枚举顶层窗口
  （标题 / appId / pid / xid）

### 2.2 实现特点

- 方法内打开/关闭独立 X 连接，不保留跨调用状态
- 支持 32bpp 和 24bpp，解析 red/green/blue mask 和字节序
- 适合 xwayland 下的传统桌面窗口（尤其是不在 xdg toplevel 里的）

### 2.3 注意

- `DISPLAY` 要可配（X11 端独立运行时尤其需要）
- 抓帧 + 枚举最好分开两个类，跟 waylandcraft 的
  `WaylandCraftBridge`（native）+ `PipeWireCaptureManager`（Java）
  同款分离结构

---

## 3. Minecraft 内部渲染侧：framebuffer 捕捉 + JPEG 压缩 + PBO

> 这一节对应的是"把已经在 Minecraft 内渲染好的共享窗口纹理读回
> CPU / 编码后分发到其他客户端"——跟抓外部桌面窗口不同，但
> framebuffer 回读 + JPEG 编码 + 差异检测这套工程模式值得复用。

### 3.1 Java 入口

`dev.evvie.waylandcraft.render.ImageCapture`（共享窗口发送端）

### 3.2 优化点

- PBO 双缓冲异步回读，避免 GPU→CPU 同步阻塞
- GPU 侧缩放（`glBlitFramebuffer`），不走 CPU scale
- 直接 RGBA → JPEG 编码，跳过中间 `BufferedImage`
- 像素差异检测：无变化帧跳过发送
- 窗口句柄隔离的 PBO / 缩放 FBO 状态，避免多窗口相互串扰

### 3.3 兼容性真实坑（已修）

- MC 26.x 渲染器在 pass 之间会把 `GL_READ_BUFFER` 设为 `GL_NONE`
- 读像素前必须显式 `glReadBuffer(GL_COLOR_ATTACHMENT0)`
- 读完后最好恢复为 `GL_NONE`，否则可能污染后续渲染
- PBO 在某些 Mesa / EGL 环境下可能 `glGenBuffers` 返回 0，需要永久降级为同步读取

---

## 4. 共享 / 转发侧

### 4.1 视频帧

`SharedWindowFrameRelay`

- 按窗口 handle 缓存最新帧
- 服务端每 2 tick 收集后再分片多线程发送
- 超过 ~1.9 MB 的帧直接跳过

### 4.2 音频

`AudioFrameRelay`

- 每窗口一个队列，积压时丢最旧的帧
- 服务端 tick 批量出队，再分片发送

### 4.3 通用模式

- 视频：最新帧覆盖（overwrite-if-newer）
- 音频：有序队列（drop-oldest-on-overflow）
- 都带 `windowHandle`，用于路由和权限

---

## 5. 音频捕捉（进程粒度）

- Java 入口：`AudioCaptureManager`
- 原生入口：`WaylandCraftBridge.audioCaptureStart/Poll/Stop/Status`
- 原生实现：`native/src/audio_capture.rs`

### 5.1 思路

- 窗口 → 进程 PID → PipeWire 默认 sink 的 monitor 端口 → 捕获该源的 PCM
- 拿到 PCM 后，前 8 字节写入 `[sampleRate, channels]`，然后是 PCM 数据
- Java 侧按 `MAX_PACKET_BYTES` 分包发送

### 5.2 已知限制

- 粒度极限是进程，不能细到每个标签 / 子窗口
- 偏向"捕获默认输出的 monitor"，不是按应用节点精确匹配
- 窗口所属进程通过 SO_PEERCRED（原生 Wayland）或 `_NET_WM_PID`（X11）拿
- 如果 PID 解析不到，就不启动音频捕捉，但画面共享不受影响

---

## 6. 设计要点

### 6.1 捕捉会话模型（建议）

```
CaptureSession
  start(...)
  pollFrame() -> FrameSnapshot | null
  stop()
```

三种实现：

- `WaylandCaptureSession`：Portal + PipeWire（外部桌面窗口，主路径）
- `X11CaptureSession`：JNA XGetImage（X11 窗口，独立运行时）
- `InternalRenderCaptureSession`：从 Minecraft framebuffer / 纹理抓帧
  （共享发送端）

### 6.2 FrameSnapshot 字段建议

最少应有：

- `captureTimeMs`
- `width`, `height`
- `imageData`
- `format`（RGBA / JPEG / PNG / …）
- 来源类型（`WAYLAND` / `X11` / `INTERNAL`）
- 帧序号、是否差异帧、是否最新帧覆盖策略产物（共享时需要）
- 关联音频时间基准（同步时需要）

### 6.3 窗口元数据建议

- 窗口标识（Wayland surface/serial 或 X11 xid）
- 标题、应用标识
- 尺寸和可见性
- 几何信息
- 是否可捕捉

---

## 7. 易踩坑清单

- PipeWire 节点格式不固定，必须统一转 RGBA
- Portal D-Bus 别用固定 token 硬编码推导路径
- gdbus monitor 别用 `timeout` 包完整流程
- PBO 在有些环境下失效，必须有同步回退路径
- MC 渲染器会改 `GL_READ_BUFFER`，抓帧时要显式设置和恢复
- 多窗口共享时，PBO / 缩放 FBO 状态要按窗口隔离
- 透明像素的窗口若强制 JPEG，会出现黑边；PNG 走质量参数失效
- 音频按窗口抓取的粒度极限是进程
- `grim` 只能抓整个 output（或一个区域），不能抓 portal 选中的窗口
- dbus-send 在 dbus-daemon 没跑时会 fail-fast — 不要 retry，要给用户
  "Wayland capture unavailable" 的清晰诊断

---

## 8. 跟现状对照

| 路径 | 上游实现 | 本文位置 |
|------|----------|----------|
| Wayland PipeWire | `PipeWireCaptureManager` + `native/portal_capture.rs` | §1 |
| Wayland dbus+grim fallback | `capture/fallback/*` | §1.4 |
| X11 JNA | （未实现，主路径走 XWayland） | §2 |
| Minecraft framebuffer 回读 | `render/ImageCapture` | §3 |
| 共享 / 转发 | `SharedWindowFrameRelay` + `AudioFrameRelay` | §4 |
| 音频捕捉 | `AudioCaptureManager` + `native/audio_capture.rs` | §5 |