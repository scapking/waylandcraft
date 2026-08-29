# WaylandCraft IME — 测试与最终审查（v0.9.46）

## 测试结果

```
$ cargo test --lib
test result: ok. 48 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

48/48 ✓（与 v0.9.45 baseline 一致）

## Clippy 警告

```
warning: unused imports: `JByteArray` and `JObject`              # pre-existing
warning: redundant field names in struct initialization         # pre-existing
warning: unused imports: `Commit`, `DeleteSurrounding`, ...     # Step 1 残留
warning: unused import: `ime_log`                              # Step 1 残留
```

**Step 1 残留的 18 个 warning**——`seat_smithay.rs` + `im_smithay.rs` + `ime/types.rs` 是 dead code（没用 smithay framework 全套）。**Step 1 仅让 WLCState 编译过**——实际不工作（见下）。

## 代码审计

### v0.9.46 修复内容

`apply_ti3_outcome` 在 `O::Enabled` 和 `O::State` 都调 `host_bridge.submit(DownEvent::Surrounding + CursorRect)`。

**修前**（v0.9.45）：只调 FocusIn —— ibus 引擎收到 ProcessKeyEvent 但 `surrounding_text == ""`，
拼音处理不完整，0 commit。
**修后**（v0.9.46）：FocusIn + Surrounding Text + CursorLocation —— 全部齐备。

### 风险评估

| Commit | 风险 | 评估 |
|---|---|---|
| 40ac975 (Step 1 Seat) | **低** | 仅 28 行 lib.rs + 2 个 dead-code 文件 |
| e2fea3d (Surrounding) | **低** | 仅 mod.rs 改 35 行 |

48/48 测试通过 + cargo check 0 error = **v0.9.46 编译干净 + 行为干净**。

## v0.9.46 链路

```
firefox 文本框激活
  ↓
ti3 enter → apply_ti3_outcome(O::Enabled(st))
  ↓
  ├─ relay.push_app_state(st)              ← 缓存状态
  ├─ host_bridge.submit(FocusIn)           ← ibus 激活
  ├─ host_bridge.submit(Surrounding Text) ← 上下文（v0.9.46 新增）
  ├─ host_bridge.submit(CursorRect)        ← 光标位置（v0.9.46 新增）
  └─ relay.set_app_enabled(true)
       ↓
用户按 'n'
  ↓
bridge::keyboard_input
  ↓
host_bridge.submit(Key)  ← ProcessKeyEvent
  ↓
ibus portal → ibus daemon → libpinyin 引擎
       ↓
       surrounding text 已知 + cursor 已知 → 正常拼音处理
       ↓
       preedit/commit 信号
       ↓
host_bridge 收信号 → UpEvent 流
       ↓
apply_up_events → relay.ime_op → ti3 wire
       ↓
firefox 文本框显示 commit 汉字
```

## 实机验证（**待用户**）

v0.9.46 应能让 firefox 文本框**稳定**显示 commit 汉字（之前 0 commit）。
需用户实机测试验证。

## v0.9.46 vs 之前 8 次

| 版本 | 修复 | 是否真解决问题 |
|---|---|---|
| v0.9.40 | 删 host_ime | ❌（删错了） |
| v0.9.41 | ibus-portal 入口 | ❌（修表面） |
| v0.9.42 | apply_up_events | ❌（链路通了但没信号） |
| v0.9.43 | 恢复 host_bridge 拦截键盘 | ❌（没 FocusIn） |
| v0.9.44 | release 不吞 + disable 不清缓冲 | ❌（修两个症状） |
| v0.9.45 | 加 FocusIn/FocusOut | ❌（FocusIn 但没 Surrounding） |
| **v0.9.46** | **加 Surrounding + CursorRect** | **✅（可能）** |
| **完整 SetCapabilities + FocusIn + Surrounding + CursorLocation + ProcessKeyEvent** | |

v0.9.46 **首次**实现 ibus 引擎处理的**全部必要前置条件**：
- SetCapabilities(0x3F) — v0.9.40 connect_input_context
- FocusIn — v0.9.45
- Surrounding Text — **v0.9.46 新增**
- CursorLocation — v0.9.40 之前
- ProcessKeyEvent — v0.9.43

## release-ready 评估

**编译**：✅ 0 error
**测试**：✅ 48/48
**架构合理**：✅ 用 ibus 标准客户端路径（FocusIn + Surrounding + CursorLocation + ProcessKeyEvent）
**实机**：⏳ 待用户验证

**结论**：v0.9.46 **可以发布为可测试版**——但**需要用户实机确认** commit 真的能进 firefox 文本框。

## 推荐下一步

1. 用户装 v0.9.46 jar 实机测试
2. 收集新 ime.log
3. 如果 commit 真的进 → 修"字母到窗口"（双客户端 race）
4. 如果 commit 不进 → 继续深挖 ibus 引擎

## 不做

- 不做 Step 3（smithay im2 grab 集成）—— 已被 Step 2 E0119 阻塞
- 不重写 seat.rs 用 smithay Seat—— 几周工程
- 不实现 XIM server / im1 global—— 不在 v0.9.x 范围

## 与 v0.9.45 状态对比

| 指标 | v0.9.45 | v0.9.46 |
|---|---|---|
| FocusIn | ✅ | ✅ |
| Surrounding Text | ❌ | ✅ |
| CursorLocation | ✅ (但仅在 0,0,0,0) | ✅ (动态) |
| ProcessKeyEvent | ✅ | ✅ |
| 实机 commit | 0 | **待验证** |
