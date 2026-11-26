# Windows 平台窗口/桌面捕获后端（win_capture_*）

native 模块：`native/src/capture_windows.rs`。目标：在 Windows 客户端上把任意应用
窗口或整个显示器捕获成游戏内窗口，玩家体验对齐 Linux 的 XDG Portal ScreenCast
路径（`/wl capture` → 选目标 → 窗口出现在面前）。

- Portal 路径（Linux）：**只读参照，未改动**。本模块刻意复刻它的消费形态
  （start → 共享帧槽 → 渲染线程轮询 → RemoteWindowRenderer 纹理 →
  SharedWindowDisplay 世界渲染），并修掉它的两个已知缺陷（见"与 portal 对齐/
  有意不对齐"）。
- 输入注入（鼠标/键盘）与窗口音频：**本期不做**，Phase 2 设计见文末。

## 1. native 接口（bridge.rs 注册，Java 名即下）

| Rust extern fn | Java native 方法 | 签名 | 语义 |
| --- | --- | --- | --- |
| `win_capture_targets` | `winCaptureTargets(long)` | `-> String` | 目标列表 JSON；不可用时 `available:false + reason` |
| `win_capture_start_window` | `winCaptureStartWindow(long, long hwnd)` | `-> byte[]` | 捕获指定窗口；回执 UTF-8 `"ok:<session_id>"`；失败抛异常 |
| `win_capture_start_monitor` | `winCaptureStartMonitor(long, int index)` | `-> byte[]` | 捕获指定显示器（0 起，来自 JSON monitors） |
| `win_capture_frame` | `winCaptureFrame(long)` | `-> byte[] \| null` | **null = 自上次取帧无新帧**（seq 去重，保持现纹理） |
| `win_capture_stop` | `winCaptureStop(long)` | `-> String` | 关停报告：`joined` / `thread-timeout-detached` / `no-session`；幂等 |

命名与语义对称 `portalCaptureStart/Frame/Stop`；非 Windows 平台函数注册保留
（`bind_java_type!` 的表不支持按条目 cfg），调用一律返回
`win_capture_unavailable`/`win_capture_start_failed` 异常——Java 侧**只在
Windows 上接线**。

### 1.1 targets JSON schema

```json
{
  "available": true,
  "reason": "…仅 available=false 时存在…",
  "windows": [
    {"hwnd": 12345678, "title": "无标题 - 记事本", "app_name": "notepad", "width": 800, "height": 600}
  ],
  "monitors": [
    {"index": 0, "name": "\\\\.\\DISPLAY1", "x": 0, "y": 0, "width": 2560, "height": 1440, "primary": true}
  ]
}
```

- `hwnd`：**十进制** long（直接回传 `winCaptureStartWindow`）。
- 过滤谓词（native 侧完成，Java 拿到的即可用）：可见、非 cloaked（UWP 挂起/
  其他虚拟桌面）、非工具窗、非无重定向位图、非本进程、非最小化、有标题、
  尺寸>0。
- `available=false` 的判定：`GraphicsCaptureSession::IsSupported()`
  （Win10 1903+、企业策略、Desktop SKU）。

### 1.2 session_id 约定

`start` 回执 `"ok:<id>"` 对齐 portal 的 `"ok:<node_id>"`（十进制可
`Long.parseLong`）：

- 窗口：`id = hwnd`（正数）。
- 显示器：`id = -1 - index`（**负数**，与 PipeWire node id、hwnd 正数域互斥；
  Java 可据符号分辨来源类型）。

### 1.3 帧 wire 编码合同

```
[width i32 LE][height i32 LE][seq u32 LE][pixels: width*height*4 字节]
```

- 像素为 **ARGB8888 小端**（内存字节序 B,G,R,A）——D3D11 BGRA8 读回原样，
  零转换。Java 按 int 像素解码：
  `ByteBuffer.wrap(px).order(LITTLE_ENDIAN).asIntBuffer()`，
  或直接灌 `BufferedImage(TYPE_INT_ARGB)` / `NativeImage`。
  注意与 `RemoteWindowRenderer.updateTextureRGBA` 的 **[R,G,B,A] 字节序不同**：
  那条路径是给 portal/JPEG 解码用的；Windows 帧建议走 int[] ARGB 上传
  （`writeArgbPixelsToNative` 同族逻辑），或在 Java 侧换序——接线时二选一，
  **不要**把 ARGB 字节喂给 updateTextureRGBA（红蓝会反）。
- seq：u64 内部计数截 u32 上 wire，仅做"与上一次不同则上传"去重；绕回安全。
- `null`（无新帧）与"取帧失败后纹理保持"是渲染正确性的一部分：Java 侧收到
  null 时**什么都不做**。

## 2. Java 侧接线清单（本 PR 不改 Java，合并后按此接）

现状 portal 路径的驱动点（行号基于 c7a60a9）：

1. `bridge/WaylandCraftBridge.java`（portal 声明在 ~1440-1449）
   新增：
   ```java
   private static native String winCaptureTargets(long instance);
   private static native byte[] winCaptureStartWindow(long instance, long hwnd);
   private static native byte[] winCaptureStartMonitor(long instance, int index);
   private static native byte[] winCaptureFrame(long instance);
   private static native String winCaptureStop(long instance);
   // + 对应 public 包装方法（同 portalCapture* 风格）
   ```

2. `capture/PipeWireCaptureManager.java` —— 新建对称的
   `capture/WindowsCaptureManager.java`（或在本类里按 `Platform.isWindows()`
   分发）。CaptureSession 复用现有骨架（`registerToplevel`/
   `SharedWindowDisplay`/`displayHandle` 取 id ^ 偏移防碰撞的做法一致）：
   - `startCapture()` → `winCaptureStartWindow(wlc.bridge, hwnd)`；
     回执解析 `ok:` 前缀同 portal。
   - `tick()` → `winCaptureFrame()`：null 直接 return；否则按 §1.3 解码，
     `renderer.updateTextureARGB(...)`（新增，int[] 版）+ `display.updateSize(w,h)`。
   - `stop()` → `winCaptureStop()`（返回值进 LOGGER.info，供现场诊断关停
     质量；`thread-timeout-detached` 出现即值得报警）。

3. `WaylandCraft.java:111 / :312-313` —— 捕获管理器 tick 注入点。Windows 上
   把 `captureManager` 实例化为 Windows 版（portal 版在 Windows 上天然不可用：
   没有 gdbus/pipewire）。

4. `command/WaylandCraftCommand.java`
   - `:82 /wl capture` → `captureWindow()`（:1319）：Windows 分支改为——
     `winCaptureTargets()` → 列表输出（handle 用 `0x{:x} hwnd`）→
     `/wl capture window <hwnd>` / `/wl capture monitor <index>` 显式选择
     （Windows.Graphics.Capture 没有 portal 的系统选择弹窗，且弹窗方案需要
     picker UI，Phase 2 再补；命令式选择与现有 `/wl` 交互习惯一致）。
   - 错误提示文案带 §1.1 的 `available:false reason`（区分"版本太老/策略禁用"）。

5. `build.gradle` / CI：windows 平台从 viewer-only 转为带 native ——
   `build.yml` 的 `compile-native-libs` matrix 需新增 windows 条目
   （runs-on: windows-latest，`cargo build --release --target=x86_64-pc-msvc`
   产物 `waylandcraft.dll`），且 native crate 的 Linux-only 依赖链
   （smithay/wayland-*/pipewire/rustix…）需要平台门控后才能整体编译——
   这是**合并后的大工程**，本模块本身已保证：portable 核心 + cfg(windows)
   平台层，windows 依赖走 `[target.'cfg(windows)'.dependencies]`，Linux/Android
   构建零影响。

## 3. 架构：与 portal 对齐点 / 有意不对齐点

对齐（消费形态）：

- 单会话模型（同一时间一个捕获；换目标先 stop）。
- `start → 独立线程写共享帧槽 → 轮询取帧 → 游戏内显示`；`ok:<id>` 回执格式。
- JNI 三件套命名/返回类型（start/frame/stop ↔ win_capture_*，外加
  targets 供无系统选择器的平台列目标）。

有意不对齐（portal 的缺陷不重演）：

| portal 现状 | win_capture 做法 |
| --- | --- |
| `portal_capture_stop` 空实现，PipeWire mainloop 僵尸线程直到进程退出 | **确定性关闭**：捕获线程自持 COM 生命周期，stop 置标志 → 线程内 `session.Close()` → `pool.Close()` → 逐个 Release → 线程返回；调用方 `join_with_timeout`（3s 预算，模式同 `audio_capture::join_timeout`）；结果进字符串报告可观测 |
| `get_frame()` 每轮盲克隆整帧（8MB 级 memcpy），无去重 | 帧槽 seq 水位 + `Arc<[u8]>` 快照：无新帧 take 返回 None → JNI null → 零拷贝零上传 |
| 无 resize 处理（窗口缩放后尺寸错乱） | 帧尺寸 ≠ 池尺寸时 `Direct3D11CaptureFramePool::Recreate` + staging 重建 |
| start 结果同步阻塞在 gdbus 上 | 捕获线程 mpsc 回执（`recv_timeout(10s)`），失败/超时路径同样 join+清理，不泄漏线程 |

## 4. COM 生命周期与线程模型（stop 硬验收的设计依据）

- 线程：`wc-win-capture`（Builder::name）。全部 Win32/COM/WinRT 对象
  （ID3D11Device/Context、IDirect3DDevice、GraphicsCaptureItem、
  Direct3D11CaptureFramePool、GraphicsCaptureSession、staging texture）
  **只在捕获线程创建与销毁**——跨线程只传 `Arc<AtomicBool>`（stop）与
  `Arc<FrameSlot>`（帧数据），二者是纯 Rust 同步原语。杜绝
  "外部线程 Drop 了 COM 对象、循环线程还在用它"这类关停竞态。
- 套间：线程内 `CoInitializeEx(COINIT_MULTITHREADED)`（RAII guard；
  已初始化/RPC_E_CHANGED_MODE 时不越权 `CoUninitialize`）。
- 帧循环：FreeThreaded 池 + `TryGetNextFrame()` 轮询（无帧 10ms 退避）；
  `MinUpdateInterval≈33.3ms`（30fps）从源头限流，静态画面 CPU 近零。
- 读回：`frame.Surface()` → `IDirect3DDxgiInterfaceAccess::GetInterface
  <ID3D11Texture2D>` → staging `CopyResource` → `Map(READ)` → 按 RowPitch
  压实 → publish → `Unmap`。staging 纹理按尺寸复用，仅 resize 时重建。
- stop 路径（`stop_locked`，必须持 `SESSION_LIFECYCLE`）：
  1. `ACTIVE.take()`——幂等的来源；第二次 stop 得 `no-session`。
  2. `stop=true`：线程在 ≤ poll 间隔 + 一帧读回内走到 teardown。
  3. `join_with_timeout(3s)`；超时 `mem::forget` handle——线程**必然**已自行
     完成 teardown 或在驱动 stall 解除后自行完成（其存活期间只引用自己的
     Arc<FrameSlot>，无外部资源可泄漏），调用方永不无限阻塞。
  4. `slot.clear()` 释放最后一帧像素缓冲。
- start 失败/超时路径同样 join-or-forget，全局状态回到 no-session，可重试。

## 5. Phase 2 设计（本期不做，只留接口意图）

- **鼠标/键盘注入（SendInput 定向）**：`win_capture_send_mouse(hwnd, x, y,
  buttons, wheel)` / `win_capture_send_key(...)`。要点：游戏内窗口坐标 →
  客户区坐标换算（`ClientToScreen`/DPI）后 `SetCursorPos` + `SendInput`
  绝对模式；目标非前台时先 `AttachThreadInput` 或后台窗口 `PostMessage`
  WM_(NC)LBUTTON…（各应用兼容性不同，需白名单策略）；DPI 感知
  Per-Monitor v2。现有 seat 的 pointer/key 编码通道可直接喂。
- **窗口音频（WASAPI loopback per-app）**：Windows 10 2004+ 的
  `IAgileObject`/`ActivateAudioInterfaceAsync` 无法按进程取流；实际方案是
  `IAudioClient::Initialize(CLSCTX_ALL, AUDIoclnt/stream)` 用
  `AudioSSE`/WASAPI loopback（设备级）+ 进程侧混音过滤不可行，故采用
  Windows.Graphics.Capture 配套的 `GraphicsCaptureSession` 不含音频——
  音频走 `Windows.Media.Capture.AppCaptureManager`（Win11 22621+，
  per-app 会话音频）或 `IAudioSessionManager2` 枚举
  `ISimpleAudioVolume`+SSE 环回。数据面回灌现有
  `audio_capture.rs` 的 PCM 帧槽（格式已是 [rate,ch,pcm]）。
- 窗口选择 UI：portal 弹窗的等价物（游戏内 quad 列表选 targets JSON）。

## 6. 已知风险与未验证边界（本机无 Windows，全部标注为"待真机/CI 验证"）

1. **真机运行零验证**：cargo check（独立 crate + msvc 目标）与 Linux 纯逻辑
   单测通过 ≠ 运行时正确。最终门是 GitHub Actions 的 windows native job
   （见 §2.5——该 job 目前不存在，需要与依赖链 cfg 化一起补）。
2. **DPI 缩放**：`GetWindowRect` 给物理像素需进程 DPI-aware（Per-Monitor v2），
   Minecraft/GLFW 进程通常是 system-aware 或 unaware——列表尺寸可能与
   `item.Size()`（始终物理）不一致；捕获内容本身用 item/pool 尺寸，显示不受
   影响，列表数值仅影响 UI 展示。混 DPI 多屏的 monitor 捕获未实测。
3. **HDR**：池固定 BGRA8（SDR）。HDR 显示器上 DWM 给的是已 tone-map 的 SDR
   帧（预期行为），HDR10 直通不在本期范围。
4. **最小化窗口**：列表阶段过滤掉（IsIconic）；捕获中途最小化后 DWM 停发帧
   → 游戏内画面定格最后内容。portal 语义一致（Linux 下最小化窗同样黑/旧帧）。
   如要"最小化继续捕获"需 `GraphicsCaptureItem` 无法覆盖，得换 PrintWindow
   降级路线，Phase 2 评估。
5. **隐私边框（黄色）**：`SetIsBorderRequired(false)` 在系统策略强制的机器上
   失败——已按"仅降级（保留边框）"处理并日志；Win10 <2004 该接口不存在，
   同样落到日志降级路径（未真机验证两种形态）。
6. **Win10/11 版本矩阵**：1903 以下 `IsSupported` 直接不可用；
   CreateForWindow/CreateForMonitor 在 1903/1909/21H2/Win11 22621/23H2 的
   行为差异（尤其 Size() 早期版本、cloaked 判定）未逐版本实测；
   Server SKU/无 GPU（WARP 回退 D3D11）路径未测。
7. **UIPI/完整性级别**：捕获以更高完整性运行的窗口（管理员程序）会得到
   E_ACCESSDENIED——start 返回错误，不崩，但需要文档级认知。
8. `item.Size()` 个别驱动/版本可能返回 (0,0)：已加 Win32 几何回退 +
   zero-size 显式报错。
9. **resize 风暴**：拖拽改尺寸时 `pool.Recreate` 的调用频率未做防抖
   （每帧尺寸不等即重建池+staging）；实测若成为 CPU 热点，改为 500ms
   防抖（Phase 2）。
10. start 同步回执窗口 10s：JVM 调用线程（渲染线程）在极端冷启动会阻塞
    ≤10s——portal 路径同样同步等弹窗（60s），属对齐行为；改异步需动 Java
    接线，留给 §2 接线时决策。

## 7. 验证记录（Linux 交叉环境）

- `cargo check --target x86_64-pc-windows-msvc`：**主 crate 在改动前后都止步于
  既有 Linux-only 依赖**（wayland-sys `std::os::unix`，见交付报告原文），
  该 target 的整库编译不属于本模块可交付范围（§2.5）。
  模块级验证：独立 scratch crate 以相同 windows 依赖/feature 集合 include 本
  文件后 `cargo check --target x86_64-pc-windows-msvc` 通过（结果原文见报告）。
- `cargo test --lib`（Linux）：基线 151/0/11 保持全绿，新增
  `capture_windows::tests::*`（过滤谓词、帧槽状态机、seq 去重、wire 编码、
  stop 幂等/join 预算、session_id 域）。
- Android/Linux/Server jar 构建：windows 依赖走
  `[target.'cfg(windows)'.dependencies]`，非 Windows target 完全不受影响。
