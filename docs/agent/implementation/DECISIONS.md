# WaylandCraft IME 重写 — 决策日志

## Step 0 — 计划

任务约束汇总：
- 不删 WLCSeatState（容易崩）
- 不删 input_method_v2.rs / text_input_v3.rs
- 每步 cargo check 编译过才进下一步
- 不重构 seat.rs，只新增
- 若 smithay 框架与 waylandcraft 架构根本不兼容 → 立即报告，不要硬撑

**关键发现**（Step 0 预分析）：

smithay im2 / ti3 manager **强制**依赖 smithay `SeatState<D>` + `delegate_seat!`——
而 `delegate_seat!` 接管 `WlSeat` / `WlKeyboard` / `WlPointer` 全部 dispatch。

WLCSeatState 已经在管理这些资源的 dispatch。两条线必然冲突。

## Step 1 决策（2026-08-29）

| 问题 | 决策 |
|---|---|
| Step 1 怎么做 | 加 `SeatState<WLCState>` 字段 + `SeatHandler` impl；**不**调 `delegate_seat!` |
| 为什么 | `delegate_seat!` 与 WLCSeatState dispatch 冲突；任务硬约束禁止重构 seat.rs |
| 后果 | 客户端在 im2 / ti3 manager 上 `get_input_method(seat)` 时 `Seat::from_resource` 返回 `None` |
| 进 Step 2 吗 | 进——任务是"先编译过" |

## Step 2 决策（2026-08-29）

**结论：Step 2 不可行——任务约束与 smithay 框架物理冲突。已回滚。**

| 事实 | 证据 |
|---|---|
| smithay `delegate_input_method_manager!` macro 生成 `Dispatch<ZwpInputMethodManagerV2, ()> for WLCState` | `smithay/src/wayland/input_method/mod.rs:259-285` 宏定义 |
| 现有 `ime/input_method_v2.rs:131,144` 已实现同一 trait impl | `grep "impl.*Dispatch<ZwpInputMethodManagerV2" input_method_v2.rs` |
| `cargo check` 报 E0119 conflicting implementations | 实测编译日志 |
| 任务同时要求"用 delegate 宏"+"保留现有文件" | 任务描述 |

**决策**：回滚 Step 2，仅保留 Step 1 增量（SeatState 字段 + SeatHandler impl）。
**不再继续 Step 3 / Step 4**——它们依赖 Step 2 成功。
**报告给用户决策**：A 重构 ime / B 保持现状 / C 走最小 host_bridge grab 修复路径。