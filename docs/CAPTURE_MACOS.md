# macOS 桌面/窗口捕获后端（ScreenCaptureKit）

本文档描述 `native/src/capture_macos.rs` 的 macOS 捕获后端：设计、线程与
生命周期模型、JNI 线协议，以及 **Java 侧接线清单**（native 已就绪，Java 分发
由维护者在 `WaylandCraftBridge` / `PipeWireCaptureManager` 同级接入，本分支
不改 Java 文件）。Linux portal 路径的对应实现见
`native/src/portal_capture.rs` 与 `src/main/java/dev/evvie/waylandcraft/capture/PipeWireCaptureManager.java`。

## 1. 选型结论（为什么是抓帧轮询而不是 SCStream 全速流）

| 方案 | 系统要求 | 结论 |
| --- | --- | --- |
| **SCScreenshotManager.captureImageWithFilter（已采用）** | macOS 14+ | 静态抓帧 API，一次调用一张 CGImage；以 12fps 轮询即可覆盖"桌面窗口当贴图"的需求。objc2 绑定齐备（`objc2-screen-capture-kit 0.3.2`），block 回调 + WaitGate（Mutex+Condvar，std Semaphore 未稳定故不用）同步桥接，无 delegate、无 runloop 依赖。 |
| SCStream 全速流 | macOS 12.3+ | 需实现 `SCStreamDelegate` / `SCStreamOutput` 协议（objc2 `define_class!` 手写）、CMSampleBuffer 解析、帧队列背压。在**无法真机验证**的条件下风险过高，且与现有 Java 拉帧模型（每 tick 取最新帧）收益重叠——贴图消费端本来就要重采样到纹理。后续若需要 >15fps，可在此模块加 `stream` 特性而不改线协议（seq 语义不变）。**帧率上限：约 12fps（`TARGET_FPS`）**。 |
| CGWindowListCreateImage | macOS ≤14（15 起对屏幕内容返回空/失效） | 不作 fallback。macOS 13 及以下统一报 `err:macos14_required`，引导升级而非降级出错误画面。 |

版本门用**运行时类探测**（`AnyClass::get("SCScreenshotManager")` + selector
存在性）而不是 `NSProcessInfo` 版本号比较——更可靠（类不存在时 msg_send 会
crash，探测后绝不调用）。

## 2. 架构与生命周期

```
Java (JNI 线程)                     mac-capture 线程
────────────────                    ─────────────────────────────
mac_capture_list        ──同步──▶   CGWindowListCopyWindowInfo + 过滤
mac_capture_start_window ─同步─▶    权限 preflight/request → SCShareableContent
                                    （block+WaitGate，≤10s）→ 建 SCContentFilter
                                    → spawn ─────────────────────▶ 循环：
                                      condvar 等 1000/12ms（stop 可打断）
                                      → captureImage…(block 回调内拷像素
                                         CGImage→ARGB8888→Vec)
                                      → 首帧后设 SCStreamConfiguration 长边 cap
                                         （1920，Retina 4K/5K 降采样）
                                      → slot.publish(seq++)
                                      连续 3 次失败 → slot.mark_dead → 退出
mac_capture_frame(since) ─轮询─▶    FrameSlot.poll：seq>since→新帧；
                                    无新帧且 dead→哨兵；否则空
mac_capture_stop        ─同步─▶     stop=true → condvar notify →
                                    join()（有界 ≤ ~1.1s）→ slot.clear
                                    （filter/config 都在 capture 线程 drop）
```

要点：

- **单会话**：`start_*` 会先 `stop()` 既有会话。
- **stop 确定性**（portal 僵尸线程教训）：停止标志 + condvar 唤醒 + `join()`；
  循环内所有等待都有超时，join 有界。ObjC 对象（filter/config）只在捕获线程
  持有并在线程退出时释放，跨线程只发生一次所有权转移（`SendRetained` 封装，
  安全性论证见该处注释）。
- **seq 语义**：`FrameSlot` 单槽最新帧覆盖 + 单调 seq。`take(since)` 只在
  seq 更大时返回，UI 可跳过无变化帧；会话死亡前抓到的最后一帧仍会被消费，
  下一轮才收到 dead 哨兵（先刷完画面再清理）。
- **panic=abort 纪律**：release profile 是 `panic = "abort"`，mac 区代码零
  `unwrap/expect`，block 回调内先算 `Result` 再落槽；锁中毒走 `into_inner`。

## 3. JNI API 与线协议（bridge.rs 已注册，全平台注册、非 mac 返回错误）

Java 声明模板（`WaylandCraftBridge`，与 portal 三件套并列）：

```java
private static native String[] macCaptureList(long instance);
private static native String[] macCaptureDisplays(long instance);
private static native byte[] macCaptureStartWindow(long instance, int windowId);
private static native byte[] macCaptureStartDisplay(long instance, int index);
private static native byte[] macCaptureFrame(long instance, long sinceSeq);
private static native void macCaptureStop(long instance);

public String[] macCaptureList() { return macCaptureList(this.instance); }
public String[] macCaptureDisplays() { return macCaptureDisplays(this.instance); }
public byte[] macCaptureStartWindow(int id) { return macCaptureStartWindow(this.instance, id); }
public byte[] macCaptureStartDisplay(int idx) { return macCaptureStartDisplay(this.instance, idx); }
public byte[] macCaptureFrame(long since) { return macCaptureFrame(this.instance, since); }
public void macCaptureStop() { macCaptureStop(this.instance); }
```

方法名与 `bind_java_type!` 生成的符号一致（JNI 标准 mangling，同
`portalCapture*` 的模式）。

### 3.1 列表

- `macCaptureList()` → `String[]`，行格式对齐 `getDesktopWindows`：
  `id|title|app|pid|hash|x|y|w|h`（hash 为 `mac{ID十六进制}`；title/app 中的
  `|` 已替换为 `/`）。
  **失败时为单行 `["err:<code>"]`**（权限未授不伪装成空列表）。
- `macCaptureDisplays()` → `String[]`，行 `index|id|WxH|builtin|main|name`；
  失败同样 `["err:<code>"]`。

### 3.2 启动

`macCaptureStartWindow(int)` / `macCaptureStartDisplay(int)` 返回 UTF-8
`byte[]` 文本：

- `"ok:{w}x{h}"` —— 目标点尺寸（首帧真实像素尺寸见帧头，Retina 下通常 2 倍）。
- `"err:<code>"`，已知 code：
  - `permission_denied` — 屏幕录制权限未授予（start 内部已弹系统引导，
    用户去 系统设置→隐私与安全性→屏幕录制 勾选后**重启游戏进程**才生效）；
  - `macos14_required` — 系统 < macOS 14；
  - `macos_capture_unsupported_platform` — 在非 macOS 上调用（Linux/Android
    走 portal；Windows 走各自后端）；
  - `window_not_found:{ID}` / `display_index_invalid:{i}` / `display_not_found`
    / `shareable_content_failed: …` / `shareable_content_timeout`
    / `spawn_failed: …` / `session_busy`。

start 可能阻塞至 ~10 秒（系统弹窗等待授权 + 内容枚举）。Java 侧沿用 portal
模式在命令线程调用即可，别放渲染关键路径。

### 3.3 拉帧

`macCaptureFrame(long sinceSeq)` → `byte[]`：

| 返回 | 含义 | Java 动作 |
| --- | --- | --- |
| 长度 0 | 无新帧（含无会话） | 跳过纹理上传 |
| `[1]`（单字节） | 会话已终止（目标关闭/连续失败） | 清纹理 + `macCaptureStop()` + 移除 toplevel |
| `[w u32][h u32][seq u64][像素]`（LE） | 新帧 | 上传后把 `seq` 存为下次调用的 `sinceSeq` |

像素布局：**ARGB8888，内存字节序 B,G,R,A**（小端 uint32 `0xAARRGGBB`）。

> ⚠️ **字节序警告（接线必读）**：portal 路径给的是 **RGBA** 字节序
> （`RemoteWindowRenderer.updateTextureRGBA` 按 `GL_RGBA/GL_UNSIGNED_BYTE`
> 上传）。本后端是 BGRA 内存序：**不能直接复用 `updateTextureRGBA` 的通道
> 解释**，否则红蓝互换。接线时二选一（推荐前者，零转换开销）：
> 1. 在 `RemoteWindowRenderer` 加 `updateTextureARGB(handle,w,h,bytes)`，
>    上传时 `glTexImage2D(..., GL_BGRA_EXT, GL_UNSIGNED_BYTE, ...)`
>    （LWJGL 的 `GL43.GL_EXT_BGRA`/`GL13.EXT_BGRA`，macOS Core Profile 支持
>    `GL_BGRA_EXT` 仅限纹理格式兼容时——若 GL 层拒绝，退方案 2）；
> 2. 或沿用 `updateTextureRGBA`，Java 侧逐像素 swap（每帧全帧拷贝，成本明显）。
> iOS/Android 的 GLES 侧若也走该管理器，注意分支只影响 mac 路径。

### 3.4 停止

`macCaptureStop()` 幂等、同步完成（内部 join 有界，最坏 ~1.1s，通常 <0.2s）。

## 4. Java 侧接线清单（本分支未改 Java；由维护者统一接入）

1. `WaylandCraftBridge.java`：上节 6 个 native 声明 + 包装（portal 三件套旁）。
2. 新建 `MacCaptureManager`（镜像 `capture/PipeWireCaptureManager.java`）：
   - `startCapture(windowId/displayIndex)` → 解析 `ok:`/`err:`；err 透出聊天栏
     （`permission_denied` 附带"重启游戏生效"提示，`macos14_required` 提示升级）。
   - `registerToplevel(title)`：复用 `SharedWindowDisplay` +
     `RemoteWindowRenderer`（handle 取 `0x4D00000000000000L | windowId` 之类
     与 portal `0x7000…` 段错开）。
   - `tick()`：`macCaptureFrame(lastSeq)`；空→跳过；`[1]`→`stopCapture()`；
     否则解析头 + **按 §3.3 的 ARGB 通道上传** + `lastSeq = seq`。
   - `stopCapture()`：`macCaptureStop()` + `destroyTexture` + 移除 display。
3. 平台分发（`OS.IS_MAC`）：
   - `WaylandCraft#update()` 的 captureManager.tick → mac 上指 MacCaptureManager；
   - `WaylandCraftCommand` 窗口列表：mac 上 `macCaptureList()` 替代
     `X11WindowLister.getDesktopWindows()`（后者依赖 X11，mac 必失败）；
     `/wc` 选择 UI 增加显示器条目（`macCaptureDisplays`）。
4. 音频（`shared/AudioCaptureManager`）：mac 上保持禁用
   （audio_capture_stub 会返回 `Err("audio capture is only supported on
   Linux/Android")`，Java 应捕获并提示"macOS 暂无共享声音"）。

## 5. 权限与部署

- 首次 start/list 触发 TCC 弹窗（系统设置 → 隐私与安全性 → 屏幕录制）。
  授权后 **进程需重启** 才有画面（macOS 已知行为；native 侧无法绕过）。
  未授权期间 `macCaptureList` 返回 `err:permission_denied`，绝不假成功。
- Minecraft（LWJGL/GLFW 非沙箱进程）无需 entitlement；若未来进 App Store
  沙箱化，需 `com.apple.security.screen-capture` entitlement（macOS 11+ 语义
  按 Apple 文档另行核对）。
- **分发缺口（如实声明）**：当前 CI 把 macos 当 viewer-only 平台
  （`.github/workflows/build.yml` matrix `viewer: true`，`build.gradle`
  `viewerPlatforms` 含 macos），**没有 macos native 编译/打包 job**。本后端
  要真正进入 macOS jar 需要：CI 新增 macos runner 的 `cargo build`（原生
  aarch64/x86_64 各一，或交叉+签名）、`build.gradle` 从 viewerPlatforms 摘掉
  macos、`verify_jar.py` 更新——均在本任务范围外，由维护者排期。

## 5.5 Apple 交叉 check 覆盖面（如实声明）

- **模块级隔离门已通过**：`capture_macos.rs` 以 edition 2024 独立 crate
  （依赖与本仓库 macos target 段一致）分别在
  `--target aarch64-apple-darwin` 与 `--target x86_64-apple-darwin` 下
  `DOCS_RS=1 cargo check -j1` 全新编译 **0 错误**（14 条 warning 全部是绑定
  0.3.2 把 `CGImageGetWidth`/`CFDictionaryGetValue` 等 C 风格函数改名为方法
  的 deprecated 提示，属后续打磨项，两种形态当前都存在）。
- **整库** `cargo check --target *-apple-darwin` **当前过不去，阻塞点与本模块
  无关**（实测报错：`error: failed to run custom build command for
  gettext-sys v0.26.0`，链路 `waylandcraft → freedesktop-desktop-entry 0.8.1 →
  gettext-rs → gettext-sys`，其 build.rs 依赖 pkg-config 的 Linux 桌面环境；
  合入的主分支还引入了 libspa-sys 等 C 面）。Linux 依赖链按 target cfg 化是
  独立后续工程；pipewire 已先行门控进
  `[target.'cfg(any(target_os="linux",target_os="android"))'.dependencies]`。
- `DOCS_RS=1` 的语义：`objc2-exception-helper` 的 build.rs 用宿主 cc 编译
  try_catch.m（无 include 的纯 ObjC，本机无 clang）；该 env 使其早退——与
  cargo check 不做链接的语义一致，不影响类型/签名检查有效性。真机 CI 原生
  编译时不设该 env，正常参与链接。
- 实现注记：跨线程等待原语用自写 `WaitGate`（Mutex+Condvar，一次性 signal，
  先 signal 后 wait 不丢事件）——`std::sync::Semaphore` 在 stable rustc
  （含 1.100）尚未稳定，禁止使用。

## 6. 已知风险 / 未验证项（本机 Linux，无法运行 macOS）

1. **真机未验证**：抓帧通路（TCC 弹窗、SCScreenshotManager 回调、像素格式）
   全部依赖 macOS 真机行为；Linux 侧验证仅覆盖类型/绑定正确性
   （`cargo check --target aarch64-apple-darwin` / `x86_64-apple-darwin`）与
   纯逻辑单测。
2. **Retina**：输出为像素尺寸（通常点尺寸×2）；长边 1920 cap 由 SCK 缩放，
   缩放质量与 `pointPixelScale` 未真机确认。Java 纹理尺寸以帧头为准即可。
3. **版本矩阵**：14.x 为设计目标；15.x（Tahoe 前）理论上仅 SCK 行为微调，
   26（Tahoe）未测；13 及以下显式拒绝。CGWindowList 枚举在 15 仍可用
   （仅 `CGWindowListCreateImage` 失效），但 kCGWindowName 依赖屏幕录制权限。
4. **权限撤销**：会话中途在系统设置撤销权限 → SCK 回调报错 → 连续 3 次后
   dead 哨兵透出。未真机验证撤销瞬间的行为。
5. **多显示器**：displayID ↔ SCDisplay 匹配假设 `CGGetActiveDisplayList` 与
   SCK 列表一致；热插拔后 index 语义按"调用时刻快照"（Java 需重新拉列表）。
6. **objc2 绑定升级**：`SendRetained` 的 unsafe impl 与 block 回调参数
   （`*mut CGImage` 裸指针 vs `Retained`）在绑定大版本升级时需重审；
   `captureImageWithFilter_configuration_completionHandler` 的 method family
   声明为 none（不接管返回值所有权），像素在 block 内拷出，符合当前生成形态。
7. **12fps 上限**：对静态窗口足够；高帧率内容（视频/游戏画面套窗口）会有
   明显卡顿感，属已声明的 MVP 限制。
8. **内存**：1920 cap 下 8.3MB/帧；`take` 克隆 + JNI byte[] 再拷一份，
   tick 峰值 ~17MB 临时分配（portal 同量级，接受）。

## 7. Linux/Android 零破坏说明

- `capture_macos.rs` 的 FFI 区是 `#[cfg(target_os = "macos")]`，纯逻辑区无
  外部依赖；Linux 构建里它只是一组带单测的纯函数。
- `portal_capture` / `audio_capture` 在 lib.rs 中按 target_os 二选一
  （stub 文件仅存在于非 linux/android，Linux 行为逐字节不变）。
- bridge.rs 新增 6 个注册项与函数：非 mac 调用返回
  `err:macos_capture_unsupported_platform`，不触碰任何既有路径。
- Cargo 新依赖全部位于 `[target.'cfg(target_os = "macos")'.dependencies]`。
