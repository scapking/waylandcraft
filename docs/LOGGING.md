# 日志分级（verboseLogging）

单一开关控制"详细日志"：

- **verbose=true（开发档）**：全量日志——逐键、逐事件、周期采样行照常写。
- **verbose=false（发布档）**：只保留本文件"保留清单"里的行；细节行在
  **写入点前短路**——不格式化、不进缓冲、不写 stderr（组字热路径每键
  省 2-6 次字符串构造；判定是一次 relaxed 原子读）。

开关不改变缓冲日志（sync15）的既有合同：`waylandcraft-ime.log` 仍是 BufWriter 缓冲 +
相邻重复合并 + **重要行（WARN/ERROR/IME_STATE）即时 flush** + 10s 心跳兜底。
门控只决定"这行进不进写入器"，落盘时机合同不受影响——失败行不会因
分级变得更难看见。`status` error ring（`note_error`）是独立通道，
完全不受本开关影响。

## 开法（优先级 高 → 低）

1. **系统属性 `-Dwc.verbose=true|false`**：存在即覆盖一切（launcher /
   `JAVA_TOOL_OPTIONS` / gradle runClient jvmArgs），不必改 settings.json。
   解析规则：仅 `true|false`（忽略大小写与首尾空白），脏值视为未设置。
2. **命令**：`/wl settings set verboseLogging true|false`
   （严格布尔校验，非法值拒收并报错；即时生效——responder 同时下发
   Java 生效档 `WcLogging` 与 Rust `setVerboseLoggingNative`，档位切换
   本身会无条件写一行"档位切换"锚点便于跨层对时——早于
   `waylandcraft-ime.log` 安装时进 stderr/latest.log）。
   持久化到 `waylandcraft/settings.json`。`/wl settings list` 可查当前值。
3. **默认 = `FabricLoader.isDevelopmentEnvironment()`**：gradle runClient
   等开发环境默认开，发布 jar 默认关。

实现位置：Java 侧生效档 `dev.evvie.waylandcraft.utils.WcLogging`；
Rust 侧全局 `bridge::VERBOSE_LOG: AtomicBool`（**默认 true**——native
单测/bench/无 Java 下发环境保持全开，既有日志断言行为不漂移）。
服务端 jar 无 native：`setVerboseLogging` 走 `nativeAvailable` 防护静默
跳过（setModVersion 同纪律），Java 侧分级照常生效。

另有第三条独立通道：`WindowShareManager` 的 `[ADAPTIVE]` 行按任务要求
降为 **log4j debug 级**（不走 verbose 开关）——发布档 latest.log 默认
不可见；需要看控制环时把本 mod logger 配到 DEBUG。

## 分级表（逐子系统）

### 永远保留（发布档也写）

| 类别 | 行/位置 |
| --- | --- |
| 启动摘要 | bridge 版本/platform 探测、`host_bridge: backend selected` / TRANSIENT / 不可用结论、`[dbus-ibus/fcitx5] probing... → input context READY`、`[ime] globals 已注册`、`[ime][builtin] 内嵌拼音词库就绪`、`[audio] session owner thread started` 与 stage 1/4–4/4 摘要、`[kb] setKbLogFile` / `setImeLogFile` / `setAudioLogFile` 路径行、Java `WindowShareManager initialized` |
| WARN / ERROR / 失败证据 | 所有含 WARN/ERROR 的行（缓冲日志同时**即时 flush**）、`FATAL`、ProcessKeyEvent 失败、命令失败（不影响 host_bridge）、`ti3 batch DROPPED`、`host_bridge flush NOT applied`、`keyboard_focus: surface NOT ALIVE`、`[kb] WARN 无 focus`、`keyboard_key DROPPED kb_active=false`、apply_ti3_outcome `hb NOT ready` / `host_bridge=None`、audio 启动/会话失败/FALLBACK/join 超时、`[ime][source]/[ime][nested] 忽略非法值`、档位切换行 |
| error ring（独立通道） | `status::note_error` 全部条目（serial mismatch、im2 掉线、内置词库失败、audio 降级……）——不受门控 |
| IME 里程碑 | nested ⑥ 决策全部行（spawn/成功/失败/无二进制/bind → 主路径激活/超时 Failed/回退/残留杀进程/配置与探测摘要）、`[im2] 输入法客户端 bind / 断开 → 回退`、`IME_STATE: A -> B`（即时 flush）、`[source] 路由切换`、`/wl ime source` 切档行、ti3 实例 created / destroyed-active、popup 角色登记/解除、`[ime][overlay] ScreenRenderMixin 注入生效`（首次） |
| 共享 start/stop 结果 | 服务端 relay 的 oversized **拒绝** WARN、执行器 rejected WARN、慢观众行；`[SERVER] forwarded image` 已是 debug 级维持现状；Java WindowShareManager `Started/Stopped sharing`、audio sharing enabled/failed、audio follow 切换/停止、`Cleared all share states`、config 设置/清除结果、`Sharing enabled/disabled` |
| 键盘焦点迁移 | `[kb-debug] keyboard_focus` 的 enter/leave/lost/NOT ALIVE 汇总行（真实焦点切换才打；幂等重聚焦静默） |

### verbose 门控（发布档写入点跳过）

| 子系统 | 门控行 |
| --- | --- |
| seat.rs 逐键 | `[kb-debug] keyboard_key: key=... mods(...) -> SENT/DROPPED(no focus)`（每键 1 行；同块内 focus 描述串与 xkb mods 序列化读数随门一并短路） |
| bridge.rs 按键路由 | `[ime] bridge submit_key ...`、`[ime] ⑧ raw forward ...`（逐键；⑧ 路由分布计数不受影响） |
| bridge.rs 轮询细节 | `updateSurfaceData ... not yet associated`、`updateSurfaceTrees ... not alive`（每帧良性竞态跳行）、`candidateNav`（已删通路的每调用 stderr 行） |
| ime/mod.rs 逐事件 | set_focus 的每帧 `FocusIn（set_focus 路径）`/`FocusIn 跳过（路由门）` 两行（Java 每帧 focusSurface，60fps 洪水源）、`apply_ti3_outcome Enabled/State/Disabled` 细节行与 `host_bridge 提交跳过`、`[relay] im2 commit(serial) 无内容`、`host_bridge flush applied -> ti3 batch` |
| ti3 逐事件 | `[ti3] commit_instance / Enable / Disable / SetSurroundingText（含用户文本摘录）/ SetCursorRectangle` |
| relay 逐事件 | `[relay] set_app_enabled ...`（会话边界行；宏观轨迹仍由保留的 IME_STATE 承载） |
| popup 细节 | `[popup][gate] 内容在场交付`（200ms 节流观测网）、`attach(NULL)` 帧清空/hold 行（非 verbose 连节流簿记一起短路） |
| virtual_keyboard 逐事件 | `[vk] 引擎直通 key=...`、`[vk] modifiers（忽略...）`（sync15 点名的恒定刷屏行；vk 客户端创建行保留） |
| host_bridge 逐键/每信号 | `[dbus-ibus/fcitx5] submit Key keysym=0 拒绝`、`FocusIn/FocusOut -> ibus-daemon`、`SetCursorLocationRelative`、`SetSurroundingText`、`ProcessKeyEvent ... -> consumed=`（成功行）、`handle_signal: CommitText text=...`（含上屏文本，隐私面一并门控） |
| audio poll 周期 | `[audio] watchdog: round ...`（500ms tick）、`capture ongoing: N events`（process 回调每 2000 事件）、`pid节点:` / `candidate app stream:` / `candidate sink:` 拓扑逐节点 dump（首包 `FIRST capture` 里程碑保留） |
| status 采样 | metrics 10s 窗口聚合报告行（心跳 flush 本身不受影响，里程碑行可见延迟合同不变） |
| Java [ime][overlay] | 状态五元组行（1s 节流；**首次注入证明行保留**；WARN 统一通道行保留） |
| Java [cursor] | 焦点丢失补发 `(0,0,0,0)` 行、EditBox 位置变化行（含输入框文本摘录，字符串裁剪随门短路） |
| Java [ime][world] | `worldVisible=true/false` 轨迹行 |
| Java 服务端 relay | `[SERVER] relay stats (1s 聚合)` 行（计数推进不受影响） |

### 即时 flush（既有缓冲日志合同，与分级正交）

- `waylandcraft-ime.log`：内容含 `WARN`/`ERROR` 或以 `IME_STATE` 开头的
  行写入后立即冲刷（崩溃取证）；其余行由 10s 心跳（metrics_tick 驱动）
  兜底。
- `waylandcraft-kb.log` / `waylandcraft-audio.log`：逐行 flush 设计不变
  （被门控的行直接不产生，不存在"留行不落盘"的中间态）。

## 测试

- Rust：`bridge::verbose_gate_tests`（注入内存 sink，断言 VERBOSE 开关
  决定带门宏/内联门细节行写与跳、里程碑与档位切换行任何档位都写、
  全局默认档为 true、开关往返恢复）。
- Java：`VerboseLoggingSettingsTest`（settings.json Gson 往返、旧文件缺键
  时的默认兼容、`-Dwc.verbose` 解析、生效档合成优先级、纯 JVM 兜底档）。

## 已知边界

- 档位读取是 Relaxed：切换瞬间个别行可能按旧档写，无正确性依赖；
  业务逻辑禁止读 `VERBOSE_LOG` 做判定。
- `set_verbose_logging` 的下发发生在 settings responder 注册时（启动）
  与每次变更时；Rust 侧在此之前按默认 true 全开（启动早期日志不丢）。
- `-Dwc.verbose` 在场时 settings 值仍持久化但不生效（CLI 回执会标注）。
- 服务端（无 native）只有 Java 侧分级；`[SERVER] relay stats` 的开关
  由 `WcLogging` 默认档（发布环境=false）决定。

## v1.4.2 收口（1.4.1 实测日志暴露的三个漏网点）

- `KeyboardHandlerMixin` 两处逐键 `[kb-debug]`（mixin onPress / onKeyPress 返回）
  漏网在 INFO 级——P2-13 本应"逐键→debug"。现严格 `WcLogging.verbose()` 门；
  发布档保留一条"首次按键 = 注入已生效"里程碑行（伴生类 `KbFirstPressHolder`，
  mixin 类不能加字段）。
- verbose 档下 spawn 环境注入 `WAYLAND_DEBUG=1`（bash 通用 env 与 flatpak
  `--env=` 两路，`process.rs`）：客户端侧协议报文落
  `/tmp/waylandcraft-app{,-flatpak}.log`，定位"toplevel 已创建但永不 map"
  的等待点。发布档零注入（报文量极大）。
- `/wl status` 逐窗口诊断行（`win 0x… alive/… mapped/… pid/… title`）
  仅 verbose 输出，计数行保持发布档可见。
- 以上为日志/观测形态变化，无测试断言改动；门控语义由既有
  `bridge::verbose_gate_tests` 与 `VerboseLoggingSettingsTest` 覆盖。
