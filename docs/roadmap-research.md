# waylandcraft 重构调研报告 (2026-09-09)

调研源: Exa 检索 + NLR-DevTeam/Fcitx5-Enhancer + reserveword/IMBlocker 源码克隆
状态: v1.2.26 CI 已全绿发布；本文档服务 roadmap 条目 2/3/5/6/7/8/9/10

---

## 9. 输入法（最高优先，多年未解的核心痛点）

### 生态现状（关键事实）

1. **MC 26.1 已原生支持 IME preedit**：Mojira MC-306468（26.1 Snapshot 10 修复
   preedit 候选窗不显示）证明 26.1 的 EditBox 链路已带 preedit 事件流。
   LibGui 16.0.1 已有 `WTextField.onPreeditUpdated` 回调（适配 26.1）。
   → 这意味着**不需要任何模拟层**：MC 自身 EditBox 已可接收 preedit 提交。
2. **Fcitx5-Enhancer**（NLR-DevTeam, MC 26.1+）：native `libfcitx5_detector.so`
   + Mixin（EditBoxMixin/KeyboardHandlerMixin/MinecraftMixin），解决 IME 激活时
   Tab/Enter 被游戏热键双重处理。注意：其 26.x 版禁用了原生 Wayland 通道
   （README 明说 native wayland 支持 disable，需回 1.20-1.21 分支看实现）。
   架构可借鉴：native 探针 + Mixin 拦截按键分流。
3. **IMBlocker**（reserveword）：纯 Mixin 焦点管理系统，IMManagerLinux/X11/Mac/
   Windows 多后端；原理是"等效焦点组件"识别 + 自动开关输入法 + 修候选框定位。
   是"输入法开关状态机"的最佳参考。
4. **GLFW/LWJGL 层**：bczhc/glfw 有 IME preedit 支持实验；glfw/glfw PR#2130
   跨平台 IME API（preedit/候选）。Minecraft <26.1 需要环境变量/GLFW hack，
   26.1 已内置 → 我们的目标版本（26.1.2）走 MC 原生 API 即可。

### 结论 / 推荐架构（对应"窗口属于 Minecraft 一部分"）

- waylandcraft 桌面窗口文本输入 = 把 Rust smithay 合成器内嵌窗口的键盘事件
  路由到 MC 原生输入流，**而不是**在合成器内再跑一套 text-input-v3 ↔ fcitx。
- 具体：native 侧负责把 fcitx5/ibus 的 preedit/commit 通过现有 dbus_ibus/
  dbus_fcitx5 bridge 收上来（已有实现基础），Java 侧把结果注入 MC 26.1
  EditBox 的原生 preedit API（仿 LibGui 16.0.1 onPreeditUpdated / MC-306468
  修复后的 EditBox 行为）。
- IMBlocker 式焦点状态机解决"游戏热键 vs IME 激活"冲突。
- 里程碑: M1 native 探针+事件桥（复用现有 dbus_*）; M2 MC 26.1 preedit 注入;
  M3 焦点状态机; M4 fcitx5+ibus 双后端实测通过。

## 2. Wayland / X11 / 未来协议扩展

- EVV1E 上游 + 两个 extended fork（PainDeMie64、Skycrafter-dev）均验证 Xwayland
  集成可行；本项目已内置 xwayland-satellite（bundled /tmp 解压）。
- 扩展性设计：现有 capture source (portal/x11/wayland) 抽象已是正确方向，
  继续把 window source 统一成接口层即可。无新调研需求。

## 3. 音频（当前只有视频可用）

- 用户方案 = 业界标准：jitter buffer。参考：
  - 直播低延迟：ingest buffer 2-4s + 服务端 LL-HLS/LL-DASH 3-5s；
  - WebRTC jitter buffer：重排序/定时/丢包隐藏，接收端核心。
- waylandcraft 落法：发送端 AudioCaptureManager 提前缓存 4-8s PCM 分块 +
  序号；接收端 AudioPlaybackManager 按 seq 入 jitter buffer，对齐后连续播放，
  丢包隐藏（重复/静音填充）。SharedWindowAudioPayload 已有 seq 字段 →
  基础设施已就位，缺的是 buffer 逻辑本身。

## 5. CLI 精简

- 现状：registerCommands 一棵 ~320 行命令树 + 2816 行文件，命令层级深、
  命名不齐（window x11/wayland + capture x11/wayland + x11* 三套并存 =
  windowSource*/capture*/x11* 功能重叠）。
- 已确认存在"注册了但从未实现"的历史债（本轮修了 12 个）→ 精简时先清重复。

## 6. 模板窗口移动体验

- 无外部参考需求，纯交互打磨（layoutMove 平滑/吸附/碰撞）。需实机反馈。

## 7. 快捷键统一

- KeyMapping 分散注册（keyOpenScreen/keyCaptureKeyboard/keyToggleCursor 等），
  需统一到单一 KeyMapping 注册表 + 冲突检测 + 设置页可见。

## 8. 高可用/扩展/并发/性能

- 抽象层按 2 的方向收敛；音频/视频链路独立线程池 + 背压；native 桥 JNI
  已有崩溃防御基础（v0.12.4 三层防御），继续补错误传播/重连。

## 10. 调研执行记录

- Exa 搜索 8 组查询（waylandcraft forks / MC IME / text-input-v3 / 低延迟音视频）
- clone: EVV1E/waylandcraft(参考) + Fcitx5-Enhancer + IMBlocker（源码在
  /tmp/fc5-enh /tmp/imblocker）
