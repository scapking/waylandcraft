# WaylandCraft IME 重构总任务

## 项目目标

waylandcraft（Java Fabric mod + Rust 嵌套 Wayland 合成器）的输入法子系统
需要完整重写。当前 v0.9.45 状态：firefox 文本框能偶尔显示 commit 汉字，
但有 race、闪、字母泄漏等问题。架构与 smithay 框架偏离，无法用现成库。

## 核心目标

1. **嵌套应用能用中文**（firefox 是最低要求）
2. **架构正确**——基于现成库/标准协议
3. **可验证**——单元/集成测试 + 实机日志

## 进度跟踪

| 阶段 | 状态 | 文件 |
|---|---|---|
| 0. 总状态 | ✅ | status/MASTER.md（本文件） |
| 1. 架构分析 | ⏳ 进行中 | architecture/ |
| 2. 技术调研 | ⏳ 进行中 | research/ |
| 3. 代码审计 | ⏸ 待启动 | implementation/audit.md |
| 4. 实现 | ⏸ 待决策 | implementation/ |
| 5. 测试 | ⏸ 待启动 | testing/ |
| 6. 最终审查 | ⏸ 待启动 | status/final-review.md |

## 当前代码状态

- HEAD: `3be28a6` (v0.9.45)
- 工作区: 干净（只有 untracked types.rs）
- 编译: 通过
- 测试: 48/48 ✓

## 所有 Agent 必读

1. **项目根**: `/run/csi/mount-root/nas/4079184d856ecc166ed19d4887083405/workspaces/default/waylandcraft/`
2. **Native 代码**: `native/src/`
3. **Java 代码**: `src/main/java/`
4. **历史对话**: 之前的 chat 上下文（README + memory/ 目录）
5. **所有结论必须落盘**到 `docs/agent/` 对应目录

## 不要做

- 不要再"自主完成"——必须**多 Agent 协调**
- 不要再"找开源方案然后自己做"——如果开源是"完整项目"就用
- 不要再"小修小补"——做根本性架构改动
- 不要为了"快速通过测试"而妥协架构
