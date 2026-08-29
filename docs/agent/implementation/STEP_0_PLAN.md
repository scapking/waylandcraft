# Step 0 — 计划与风险评估

## 任务理解

按用户给的 4 步流程，基于 smithay 完整 im2 + ti3 框架重写 waylandcraft IME 子系统。
硬约束：
- 不删 WLCSeatState（容易崩）
- 不删 input_method_v2.rs / text_input_v3.rs（删了必崩）
- 每步 cargo check 编译过才进下一步
- 不重构 seat.rs，只新增
- 若 smithay 框架与 waylandcraft 架构根本不兼容 → 立即报告，不要硬撑

## 架构预分析

### smithay im2/ti3 框架依赖链

```
InputMethodManagerState (smithay)
  └─ 拿 WlSeat via zwp_input_method_manager_v2.get_input_method(seat)
       └─ Seat::<D>::from_resource(&seat) ←── 必须 SeatUserData 已设
            └─ 由 SeatState<D>::new_seat + delegate_seat! 完成
                 └─ SeatState 替代 WlSeat 的整个 lifecycle
                      └─ WLCSeatState 现在管理的 WlSeat / WlKeyboard / WlPointer dispatch 全冲突
```

### 真实情况

smithay im2 / ti3 manager 必须靠 smithay SeatState 才能用，而 smithay SeatState
会通过 `delegate_seat!` 接管 WlSeat / WlKeyboard / WlPointer 的所有 dispatch——
**与 WLCSeatState 当前的自造 dispatch 直接冲突**。

四种可能路径：
| 方案 | 描述 | 任务约束兼容？ |
|---|---|---|
| A. 彻底替换 | WLCSeatState 退化为只存 xkb 状态 + modifiers | ✗ 禁止重构 |
| B. WLCSeatState 内嵌 smithay | WlSeat global 由 smithay SeatState 建，dispatch 走 smithay | ✗ 需要改 WLCSeatState 全部 dispatch |
| C. 双 WlSeat global | 自造 + smithay 各一个，firefox 连自造那条 | ✗ firefox 在自造 WlSeat 上做 im2 grab，到不了 smithay grab |
| D. 完全不接 smithay Seat | smithay im2/ti3 manager 没 Seat | ✗ smithay im2/ti3 不可用 |

### 决定

按任务"先做 Step 1，看 cargo check 是否过"。Step 1 任务说"不删 WLCSeatState，先新增
smithay::input::Seat + SeatHandler impl，bridge::keyboard_input 同时走 WLCSeatState（兼容）+
新增路径"。

我的 Step 1 实施计划：
1. **不**调 `delegate_seat!`（避免与 WLCSeatState 冲突）
2. **不**调 `SeatState::new_seat` 创建 smithay 自己的 WlSeat global
3. **只新增** `seat_state: SeatState<WLCState>` 字段（smithay im2/ti3 manager 用得到）
4. **新增** `SeatHandler` impl（WLCState 上）—— 但 **KeyboardFocus = WlSurface** 用现有焦点 surface
5. `bridge::keyboard_input` 仍走 WLCSeatState
6. **cargo check 预期能过**（因为新增字段不破坏现有 dispatch）

如果 Step 1 编译过 → 进 Step 2 加 im2/ti3 manager global。Step 2 必然引入
delegate_input_method_manager! 和 delegate_text_input_manager!，但这俩宏只接管
im2/ti3 相关的协议（不接管 WlSeat），所以**和 WLCSeatState 不冲突**。但 im2 / ti3
manager 的内部需要 WlSeat 上有 SeatUserData——这意味着没有 `delegate_seat!`，im2 manager
就**无法 work**（get_input_method 时 from_resource 拿不到 Seat）。

**关键发现**：smithay im2/ti3 框架**强制**依赖 smithay Seat。任务约束 "不删 WLCSeatState"
与 smithay 框架不可调和。

**Step 1 验证完后，我会**：
- 如果 Step 1 编译过：继续 Step 2，并在 Step 2 完成后做实机集成检查
- 如果 Step 2 暴露 smithay im2 不可 work（因为没 delegate_seat）：**立即报告给用户**

## 风险汇总

| 风险 | 概率 | 缓解 |
|---|---|---|
| Smithay SeatState 与 WLCSeatState dispatch 冲突 | 高（Step 1 故意避开） | Step 1 不动 WlSeat dispatch，只加字段 |
| Smithay im2/ti3 manager 没有可用 Seat | 高（Step 2 必然暴露） | 报告给用户决策 |
| 编译失败导致不得不改 WLCSeatState | 中 | 立即回滚 Step 1，仅保留 WLCSeatState |
| Java JNI 签名变化 | 低（本任务不动 Java） | 不动 Java |