# WaylandCraft IME 重构总任务

## 项目目标

waylandcraft（Java Fabric mod + Rust 嵌套 Wayland 合成器）的输入法子系统
需要完整重写。当前 v0.9.46 状态：firefox 文本框**应该**能稳定显示 commit 汉字
（v0.9.46 加了 Surrounding Text + CursorRect 推给 host_bridge）。

## 进度

| 阶段 | 状态 | 文件 |
|---|---|---|
| 0. 总状态 | ✅ | status/MASTER.md（本文件） |
| 1. 架构分析 | ✅ | architecture/ANALYSIS.md |
| 2. 技术调研 | ⏸ TODO | research/SOLUTIONS.md（未落盘，Agent 跑了 60+ 分钟） |
| 3. 代码审计 | ✅ | implementation/DECISIONS.md + FINAL.md |
| 4. Step 1: smithay Seat 接入 | ✅ | implementation/STEP_1.md（commit 40ac975） |
| 5. Step 2: smithay im2 manager | ❌ E0119 | implementation/STEP_2.md（E0119 失败） |
| 6. 选项 C 修 race | ✅ | testing/REVIEW.md（commit e2fea3d v0.9.46） |
| 7. 实机测试 | ⏳ 待用户 | （用户测 v0.9.46） |
| 8. 最终审查 | ✅ | testing/REVIEW.md |

## 当前代码状态

- HEAD: `e2fea3d` (v0.9.46)
- main: `e2fea3d` 推 GitHub
- tag: `v0.9.46` 推 GitHub
- 编译: 0 error
- 测试: 48/48 ✓

## 关键决策

- **smithay 完整 im2 框架无法使用**（Step 2 E0119 失败）
- **修 v0.9.45 已知 race**（v0.9.46）—— 通过 ibus 客户端标准路径
- **保留现有 ime/ 架构**—— host_bridge 接管键盘
- **v0.9.46 实现 ibus 引擎全部必要前置条件**

## 实机验证（**需要用户**）

- 下载 v0.9.46 universal.jar
- 装到 mods
- 测试嵌套 firefox 中文输入
- 收集新 ime.log
- 如果 commit 真的进 → 修"字母到窗口"双客户端
- 如果 commit 不进 → 继续诊断

## 所有 Agent 必读

1. 项目根: `/run/csi/mount-root/nas/4079184d856ecc166ed19d4887083405/workspaces/default/waylandcraft/`
2. Native 代码: `native/src/`
3. Java 代码: `src/main/java/`
4. 重要文档:
   - docs/IME_ARCHITECTURE.md
   - docs/agent/architecture/ANALYSIS.md
   - docs/agent/implementation/FINAL.md
   - docs/agent/testing/REVIEW.md
5. 所有结论落盘: docs/agent/ 目录
