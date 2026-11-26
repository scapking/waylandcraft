# 应用生命周期：单实例应用与会话收养

适用版本：1.4.2+（fix/app-launcher-lifecycle）。对应缺陷：`/wl launch code`
后进程在、`mapped=0`、窗口永远不出现（1.4.1 用户实测）。

## 1. 问题：单实例应用 × 跨会话孤儿

VS Code、Firefox、Discord、微信这类应用是**单实例**的：再次启动时，新进程
只把"请打开窗口"的请求经 IPC 交接给**已存活的同名进程**，自己立即退出。

WaylandCraft 每局游戏是一个独立的嵌套 Wayland 会话（合成器 socket，
`wayland-N`）。应用在会话里被 `/wl launch` 启动后：

- 它的应用主进程绑定在这个 socket 上显示窗口；
- 游戏退出后，socket 随 JVM 消亡，**但应用进程不会跟着死**——spawn 采用
  double-fork + setsid，进程被 init 收养，旧版本 mod 从不记录它的 PID。

于是下一局游戏里再 launch 同一应用：CLI 把请求交接给上一局的孤儿实例，
孤儿实例的 Wayland 连接早已绑死（不会重连新会话），窗口永远出不来。
`/tmp/waylandcraft-app-*.log` 里两次会话混写同一个文件的现场即此。

## 2. 机制：收养登记 + 会话标记

**收养（adoption）**：native spawn 时通过 pipe 把孙进程（exec 后即应用主
进程）的 PID 与其 setsid 进程组（PGID）登记进游戏进程内的收养表；同时向
应用注入环境标记

```
WAYLANDCRAFT_SESSION = <游戏进程PID>-<wayland-N>
```

（bash 导出、进程环境、flatpak `--env=` 三口径注入，flatpak 沙箱内同样带上。）
一个 JVM 一个标记——新旧会话即使 socket 名撞车（旧 socket 文件被删除后
`new_auto` 会复用编号），标记也不会相同。

**断开即收尸**：DISCONNECT 事件与进程退出（shutdown hook）都会对收养表中
全部进程树执行 SIGTERM → 限时宽限 → SIGKILL。孤儿实例从此不存在。

## 3. 启动前的陈旧实例检测

`/wl launch <app>` 先扫描运行中的同名进程（exe 名/命令行词形 + appId），
读 `/proc/<pid>/environ` 与对其 `WAYLAND_DISPLAY` socket 做非阻塞 connect
探测，由纯函数 `AppLifecycleLogic.classify` 给出四态：

| 判定 | 条件 | 处置 |
|---|---|---|
| OWN_SESSION | 标记等于本会话 | 不动（单实例复用是正常行为，`/wl close` 管理） |
| FOREIGN_ALIVE | socket 探测可连通 | **绝不触碰**——宿主桌面实例或另一局在跑的游戏 |
| STALE_CONFIRMED | socket 已死（文件消失/拒绝连接）且非宿主 display；标记异主确证，或无标记但名字符合 `wayland-N` 惯例（覆盖旧版本遗留孤儿） | 提示并终止进程树，然后正常 launch |
| NOT_PROVABLE | 无 WAYLAND_DISPLAY / 探测不确定 / 指向宿主 display 却异常 | 不动 |

红线：**无法确证的绝不误杀**。宿主桌面的同名进程永远命中 CONNECTED →
FOREIGN_ALIVE；判定层与采集层分离，classify 是零 IO 纯函数（单测直打）。

## 4. 交接失败诊断

launch 成功后挂 10 秒观察（200 tick，与 give 回查同节奏）：到期时
toplevel 数没有增加、且本次 spawn 的 CLI 进程已退出 → 判定为疑似交接给了
会话外的陈旧实例：聊天给出一条可操作提示（重跑 `/wl launch`、`/wl list apps`
查看收养、`/wl status` 看错误环），同时写入 status error ring。

## 5. 命令语义变化

- `/wl close <handle>`：不再 `pkill -f <appID>`。按收养表精确归属
  （PID → PGID 同组 → 登记 appId 兜底，覆盖 flatpak/satellite 来路），
  终止整棵进程组树（TERM→限时→KILL）。未收养的窗口（宿主/capture 来路）
  默认拒绝并提示。
- `/wl close <handle> force`：显式兜底档，回退旧 pkill 模式杀（可能波及
  宿主同名进程，知情使用）。
- `/wl list apps`：尾部列出本会话收养进程（`appId#pid`）。
- `/wl status`：新增 `adopted: N` 计数——`adopted>0` 而 `toplevels=0`
  即可定位"进程活着但窗口交给了会话外实例"。
- 断开/退出游戏：本会话 spawn 的应用进程树全部主动终止。

## 6. 验证

- `./gradlew compileJava test`（JDK 25）：224/0——其中
  `src/test/java/dev/evvie/waylandcraft/appsession/` 24 例：
  AppLifecycleLogicTest 20 例（四态判定全分支、含 1.4.1 实测场景还原、
  六路 NOT_PROVABLE 红线、environ/socketPath/stat/线格式解析、交接裁决、
  token 匹配防误配）+ AppSocketProbeTest 4 例（**真实文件系统**三态：
  不存在→MISSING、活监听→CONNECTED、关闭后 socket 文件残留→REFUSED，
  即 1.4.1 缺陷的合成器侧形态；并端到端接 classify 组合断言）。
- `cargo test -j1 --lib`（native/Cargo.toml）：259/0/13——process.rs 新增
  标记公式/env 注入/flatpak 选项位序/存活探测/kill 守卫/未登记 PID 拒绝。
- 真机端到端（需要 Linux /proc 与 fork，默认 ignore）：

```sh
CARGO_TARGET_DIR=... RUSTFLAGS="-L .../linklibs" \
  cargo test -j1 --lib --manifest-path native/Cargo.toml \
  -- --ignored --test-threads=1 adopted_spawn
```

  验证 spawn → pipe 回读 PID → `/proc/<pid>/environ` 带会话标记 →
  setsid PGID 一致 → force kill 树消亡，无残留进程。
- 游戏内全链（双会话场景）见"用户复测指引"（提交报告），要点：
  旧会话 launch code → 直接杀旧游戏进程 → 新会话 `/wl launch code`，
  预期出现"已清理 N 个残留实例"提示且窗口正常进入世界。

## 7. 已知边界

- 标记注入前（≤1.4.1）spawn 的遗留孤儿靠 socket 死亡 + `wayland-N` 惯例名
  识别；非 wc 命名且宿主会话同样死亡的极端场景会被保守放过（宁可漏杀）。
- 应用自行改组（子进程 setpgid 逃逸进程组）时，组杀不覆盖逃逸分支；
  Java 端陈旧清理走 descendants 树枚举可补齐。
- `/wl close` 的收养归属以合成器凭据 PID 为准；应用窗口若由**另一个进程**
  创建（罕见 wrapper 结构），需 force。
- flatpak/snap 沙箱会重整环境，标记以 `--env=` 与外层进程双保险；
  若某运行器彻底剥环境，退化为 socket 死亡判定层。
