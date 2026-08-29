# WaylandCraft IME 重写 — FINAL 总报告

## 任务完成度

| Step | 状态 | 详情 |
|---|---|---|
| Step 1: smithay::input::Seat 接入 | ✅ **完成** | 新增 `SeatState<WLCState>` 字段 + `SeatHandler` impl |
| Step 2: smithay im2 + ti3 manager | ❌ **回滚（不可行）** | 详见 STEP_2.md |
| Step 3: smithay im2 grab → host_bridge | ⛔ **未做**（依赖 Step 2） | |
| Step 4: 测试 | ⏸ **部分**（48/48 现有测试通过） | |

## Step 1 详情

**改动**：
- `native/src/seat_smithay.rs` — 新增（70 行）：`SeatHandler for WLCState` impl
- `native/src/lib.rs` — `WLCState` 加 `smithay_seat_state: SeatState<Self>` 字段 + 初始化

**约束遵守**：
- ✅ 不删 WLCSeatState（1671 行原封不动）
- ✅ 不重构 seat.rs
- ✅ 不动 bridge::keyboard_input
- ✅ 不删 ime/ 任何文件
- ✅ cargo check 编译过（30 warnings，与 baseline 一致）
- ✅ cargo test --lib 48/48 通过

**局限**（已知）：
- 没调 `delegate_seat!`、没调 `SeatState::new_seat` —— 客户端连不上 smithay Seat
- 这是任务硬约束"不重构 seat.rs"的直接后果

## Step 2 详情

**目标**：加 `InputMethodManagerState` + `TextInputManagerState` +
`InputMethodHandler` impl + `delegate_input_method_manager!` +
`delegate_text_input_manager!`。

**结果**：`cargo check` 报 3 个 E0119 conflicting implementations。

**冲突点**：

```
smithay delegate_input_method_manager! 写:
  Dispatch<ZwpInputMethodManagerV2, ()> for WLCState

现有 input_method_v2.rs:131,144 已经写:
  Dispatch<ZwpInputMethodManagerV2, ()> for WLCState
```

**这是 smithay 框架的 trait bound 硬约束**——smithay 内部
`D: Dispatch<ZwpInputMethodManagerV2, ()>` 必须满足；同一 (Type, Data) 重复
impl 在同一 State 上是 Rust trait coherence 违规（E0119）。

**任务约束冲突**：

| 任务要求 | 与 smithay 冲突 |
|---|---|
| "用 `delegate_input_method_manager!`" | macro 生成冲突的 Dispatch impl |
| "保留现有 `input_method_v2.rs`" | 已有冲突的 Dispatch impl |

**两个要求物理上不可能并存**。

## 为什么 Step 1 没爆而 Step 2 爆

- Step 1 只加 `SeatState<D>` 字段 + impl `SeatHandler` trait。
  - `SeatHandler` 的方法是 `seat_state()` / `focus_changed()` 等——不与 WLCSeatState 的 Dispatch impl 重叠
- Step 2 调 `delegate_input_method_manager!`——macro 生成的 `Dispatch<ZwpInputMethodManagerV2, ()> for WLCState` 与 `ime/input_method_v2.rs:131` 完全相同，E0119

**关键差异**：Step 1 trait `SeatHandler` 没有任何同名 Dispatch 实现的位置——是不同 trait；Step 2 delegate 宏的产物与现有 Dispatch 同一 trait 同一签名。

## 给用户的下一步选项

| 选项 | 工作量 | 风险 | 是否真解决问题 |
|---|---|---|---|
| **A. 允许重构 ime/input_method_v2.rs**——把现有 Dispatch 委托给 smithay | 大（1500-2500 行） | 高（破坏现有 IME 流程） | 是——smithay 接管后可接 im2/ti3 grab |
| **B. 保持现状（v0.9.45）** | 0 | 0 | ❌ 不解决"firefox 偶尔 work" |
| **C. 走最小修复路径**——保留自造 ime，**只修 v0.9.45 实机的 host_bridge grab race + commit 0 commit 问题** | 中（200-400 行） | 低（不破坏现有结构） | 部分——firefox 应能稳定 work |

**我推荐 C**：保持现有 ime/，修 v0.9.45 已知 race。C 不依赖 smithay 框架，
不破坏现有 48/48 测试，目标是把"偶尔 work"变成"稳定 work"。

如果用户选 A，需要明确放弃"不重构 ime"约束，工作量约 1500-2500 行——是个
真正的重写工程。

## 代码现状（2026-08-29 HEAD）

- `git status`：未提交（Step 1 改动在 lib.rs / seat_smithay.rs / im_smithay.rs / docs/）
- `cargo check`：0 errors, 30 warnings（与 v0.9.45 baseline 完全一致）
- `cargo test --lib`：48/48 passed
- 新增文件：
  - `native/src/seat_smithay.rs`（Step 1 增量）
  - `native/src/im_smithay.rs`（Step 2 失败 tombstone）
  - `docs/agent/implementation/{STEP_0_PLAN,STEP_1,STEP_2,DECISIONS,FINAL}.md`

## 后续决策待用户

我**不**自动继续 Step 3 / Step 4——按任务硬约束"如果发现不兼容——立即报告，
不要硬撑"。请用户从 A / B / C 中选一个。

- 选 A：需要你明确放弃"不删 ime"约束——我会接着做 Step 2 重构（删 input_method_v2.rs 自造 dispatch，把 WLCState 上的 Dispatch impl 改成 delegate 给 smithay）
- 选 B：保留 HEAD 不动
- 选 C：保留 v0.9.45 ime 架构，仅做 host_bridge grab race 修复

我**强烈推荐 C**。