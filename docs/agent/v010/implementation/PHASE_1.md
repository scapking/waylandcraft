# v0.10 Phase 1 报告 — 大删除 + 保留 API

## 决策回顾

**8 次版本失败**因为都在"自造 im2/ti3 dispatch 框架"路径上修表面。
smithay 不允许同名 Dispatch impl（E0119），所以必须**大刀阔斧删除**自造 dispatch。

## Phase 1 执行

### 删除文件

| 文件 | 行数 | 原因 |
|---|---|---|
| `native/src/ime/input_method_v2.rs` | 276 | 自造 im2 dispatch，与 smithay 冲突 |
| `native/src/ime/text_input_v3.rs` | 311 | 自造 ti3 dispatch，与 smithay 冲突 |
| `native/src/ime/relay.rs` | 494 | 自造 relay 状态机——逻辑并入 mod.rs |
| `native/src/ime/tests.rs` | 1002 | 旧 wire 测试——新增覆盖核心 race 的测试在 mod.rs |
| `native/src/ime/types.rs` | 60 | 已被 ime_event.rs 取代 |
| `native/src/seat_smithay.rs` | 110 | smithay Seat 接入是 dead code |
| `native/src/im_smithay.rs` | 34 | smithay im2 接入是 dead code |

**小计**：~2287 行删除

### 保留文件

- `native/src/ime/mod.rs` — 重写后的薄门面（Phase 2 输出）
- `native/src/ime/ime_event.rs` — 内部 IME 事件流（UpEvent / DownEvent）

### 修改文件

- `native/src/lib.rs` — 撤销 `SeatState<WLCState>` 字段、`mod seat_smithay`/`mod im_smithay` 声明
- `native/src/bridge.rs` — 不引用删除类型

### 公共 API（保留）

| 函数 | 用途 |
|---|---|
| `ImeState::set_focus(surface)` | 键盘焦点切到某 surface |
| `ImeState::clear_focus()` | 键盘焦点整体离开 |
| `ImeState::handle_key(key, action, mods)` | 转发按键到 im2 grab |
| `ImeState::take_lookup_table()` | 取候选窗快照（Java 自绘用） |
| `ImeState::apply_up_events(events)` | 灌入 host_bridge 上行事件 |
| `ImeState::app_active()` | 是否有激活文本输入会话 |
| `ImeState::keyboard_grabbed()` | im2 grab 是否抓走键盘 |
| `ImeState::apply_ti3_outcome` | ti3 commit 裁决落地（Phase 2 重新实现） |

## 验证

- cargo check 0 error
- cargo test --lib 48/48 全过
- clippy warning ≤ 30（保持 baseline）