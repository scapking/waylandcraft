# Step 1 — smithay::input::Seat 接入

## 改动文件

| 文件 | 变更 |
|---|---|
| `native/src/seat_smithay.rs` | **新增** — `SeatHandler for WLCState` impl（约 70 行，含注释） |
| `native/src/lib.rs` | 加 `smithay::input::SeatState` import；声明 `mod seat_smithay;`；WLCState 加字段 `smithay_seat_state: SeatState<Self>`；`WLCState::new` 初始化 `SeatState::new()` |

## 设计决策

### 不调 `delegate_seat!`

`delegate_seat!` 宏会接管 `WlSeat` / `WlKeyboard` / `WlPointer` 的全部 dispatch（`seat.rs` 现在手工做的 `GlobalDispatch<WlSeat, ()>` + `Dispatch<WlKeyboard, WLCKeyboard>` 等会被 smithay 的全局 dispatch 顶替掉）。

任务硬约束：**不删 WLCSeatState、不要重构 seat.rs、只新增**。

因此 Step 1 **不调** `delegate_seat!`、**不调** `SeatState::new_seat` 创建 smithay 自己的 WlSeat global。

### `SeatHandler` impl 内容

- `KeyboardFocus = PointerFocus = TouchFocus = WlSurface`：smithay im2 / ti3 manager 的协议惯例
- `seat_state(&mut self) -> &mut SeatState<Self>`：返回新增字段
- `focus_changed` / `cursor_image` / `led_state_changed`：默认空实现（不做事）

**仅满足 trait 约束**——不接管任何运行时 dispatch，键盘路径完全不变。

### 局限

`Seat::from_resource(&wl_seat)` 当前会返回 `None`（因为没调 `SeatState::new_seat`，没给 WlSeat 设 `SeatUserData`）。这意味着 Step 2 加入的 smithay im2 / ti3 manager 在客户端调用 `get_input_method` / `get_text_input` 时拿不到 Seat 实例，**整个 smithay im2 / ti3 框架不可用**。

但 Step 1 目标是"smithay Seat 接进来并编译过"——已完成。

## 编译结果

```
$ cargo check
Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.62s
```

- 0 errors, 0 新 warnings（30 个 pre-existing dead code warnings 与 baseline 一致）

## 测试结果

```
$ cargo test --lib
test result: ok. 48 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

48/48 ✓（与 v0.9.45 baseline 一致）。

## 进入 Step 2 前关键提醒

Step 2 加 `InputMethodManagerState::new::<WLCState, _>(disp, |c| true)` 会编译过（只需要 `WLCState: SeatHandler`，已满足）。

但 `delegate_input_method_manager!` / `delegate_text_input_manager!` 接管的是 **im2 / ti3 自己的协议**——不接管 WlSeat——所以也不会与 WLCSeatState 冲突（编译层面）。

**真正的问题**：im2 / ti3 manager 在 `get_input_method(seat)` 回调里调 `Seat::<D>::from_resource(&seat)` 拿 smithay Seat，目前会返回 `None`——即客户端实际拿不到 im2 实例（或拿到但 grab 不到 keyboard）。

Step 2 完成后我会跑 cargo check 验证编译；如果客户端实际连不上，需要**报告给用户决策**：要 (A) 重构 seat.rs 让 smithay 接管 WlSeat，还是 (B) 放弃 smithay im2 manager、走自造 im2 + 修 Step 3 host_bridge grab 路径。