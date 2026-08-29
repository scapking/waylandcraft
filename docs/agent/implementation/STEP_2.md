# Step 2 — smithay im2 / ti3 manager 接入

## 结论：**失败回滚**

smithay 完整 im2 / ti3 manager 框架**无法**与 waylandcraft 现有自造
`input_method_v2.rs` / `text_input_v3.rs` 并存。Step 2 已回滚到 Step 1 状态，
代码修改全部撤销。**立即报告给用户决策**。

## 改动文件（已回滚）

| 文件 | 变更 |
|---|---|
| `native/src/im_smithay.rs` | 新增后清空为 tombstone 注释（保留文件名供诊断） |
| `native/src/lib.rs` | 加 im_smithay_manager / ti_smithay_manager 字段、调 `InputMethodManagerState::new` / `TextInputManagerState::new`、加 `delegate_input_method_manager!` / `delegate_text_input_manager!` — **全部撤销** |

最终代码状态：仅 Step 1 留下 `SeatState<WLCState>` 字段 + `SeatHandler` impl。
`im_smithay.rs` 留作占位文件（tomestone 注释记录失败原因）。

## 失败原因（技术硬证据）

`cargo check` 报 3 个 E0119 "conflicting implementations"：

```
error[E0119]: conflicting implementations of trait
  `GlobalDispatch<ZwpTextInputManagerV3, ()>` for type `WLCState`
error[E0119]: conflicting implementations of trait
  `Dispatch<ZwpTextInputManagerV3, ()>` for type `WLCState`
error[E0119]: conflicting implementations of trait
  `Dispatch<ZwpInputMethodManagerV2, ()>` for type `WLCState`
```

### 为什么冲突

smithay `delegate_input_method_manager!` 宏展开为：

```rust
impl Dispatch<ZwpInputMethodManagerV2, ()> for WLCState { ... }
// + GlobalDispatch<ZwpInputMethodManagerV2, ()> for WLCState
// + Dispatch<ZwpInputMethodV2, InputMethodUserData<Self>> for WLCState
// + Dispatch<ZwpInputMethodKeyboardGrabV2, InputMethodKeyboardUserData<Self>> for WLCState
// + Dispatch<ZwpInputPopupSurfaceV2, InputMethodPopupSurfaceUserData> for WLCState
```

现有 `ime/input_method_v2.rs:131-160` 已经实现：

```rust
impl GlobalDispatch<ZwpInputMethodManagerV2, ()> for WLCState { ... }
impl Dispatch<ZwpInputMethodManagerV2, ()> for WLCState { ... }
```

**两份完全相同 `(Type, UserData) = (ZwpInputMethodManagerV2, ())` 的 impl —
Rust trait 一致性规则禁止**，报 E0119。

### 为什么不能简单改名/换 data type 解决

smithay 内部硬编码 `Dispatch<ZwpInputMethodManagerV2, ()>`（见
`smithay/src/wayland/input_method/mod.rs:171` 处
`D: Dispatch<ZwpInputMethodManagerV2, ()>` bound）。要让它 work，WLCState 必须
实现 `Dispatch<ZwpInputMethodManagerV2, ()>`。这是**smithay 的 trait bound
要求**——用户无法绕开。

### 为什么不能"两个 global 共存"

wayland_server 用 (Resource Type, UserData Type) 元组区分 dispatch impl——
**相同的 (Type, ()) 是同一个 dispatch**。客户端 enum 全部 manager global 时，
firefox 拿到两个 ZwpInputMethodManagerV2 global（自造 + smithay），bind 哪个
都被同一个 dispatch impl 处理——无法区分。

但**两份同名 trait impl 在同一 State 上并存**就是 E0119，编译就过不去。

## 任务约束不可调和

| 任务要求 | 与 smithay 冲突点 |
|---|---|
| "用 `delegate_input_method_manager!` + `delegate_text_input_manager!`" | macro 生成 `Dispatch<...ManagerV..., ()> for WLCState` |
| "保留现有的 `input_method_v2.rs` / `text_input_v3.rs`" | 现有文件已实现 `Dispatch<...ManagerV..., ()> for WLCState` |

**两者物理上不可能并存**——同一 trait impl 在同一类型上只能有一个。

加上 Step 1 已经发现的"smithay Seat 接管 WlSeat dispatch 与 WLCSeatState 冲突"
问题，**smithay 完整 im2 + ti3 框架与 waylandcraft 现有架构完全不兼容**。

## Step 3 / Step 4 是否还要做

按任务要求"如果发现不兼容——立即报告，不要硬撑"。**Step 3 与 Step 4 都需要
Step 2 成功作为前置**（Step 3 是 smithay InputMethodKeyboardGrab::input 调
host_bridge.submit；Step 4 是测试）。Step 2 失败 → Step 3 / Step 4 **无意义
可做**——如果继续按"路径 B"（保留自造 ime + 不接 smithay）做 Step 3，
那是另一条路径，需要用户先决策。

## 给用户的选项

| 选项 | 工作量 | 风险 | 备注 |
|---|---|---|---|
| **A. 重构 ime/ 让 smithay 接管** | 大（1500-2500 行） | 高（破坏现有 IME 流程） | 任务原本禁止重构 ime/，但 smithay 框架强制 |
| **B. 放弃 smithay im2/ti3 manager，保持 v0.9.45** | 0（已现状） | 0（保持现状） | 但 v0.9.45 实机只 firefox 偶尔 work——不解决问题 |
| **C. 走最小可行修复路径**——保留自造 ime，**仅修 Step 3 的 host_bridge grab 路径** | 中（200-400 行） | 低（不破坏现有） | 实际解决 v0.9.45 的 firefox race 问题 |

具体来说 C 路径：
- 不接 smithay im2 / ti3 manager
- 但 host_bridge 在 firefox im2 grab 期间**只让 ime grab 拿走按键**，host_bridge
  不重复抢
- 修 v0.9.45 的 commit 0 commit 偶发、字母泄漏等问题

C 是最稳的路径，但需要确认用户授权。

## 当前代码状态（Step 2 回滚后）

```
$ cargo check
Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.14s
```

30 warnings（与 baseline 完全一致）。

```
$ cargo test --lib
test result: ok. 48 passed; 0 failed; 0 ignored
```

48/48 ✓。