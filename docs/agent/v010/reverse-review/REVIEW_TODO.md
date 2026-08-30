# V010_REVERSE_REVIEW — 独立反向审查

## 目标
找出 v0.9.46 + Step 1 dead code 引入的所有架构错误。

## 阅读清单
- [x] ANALYSIS.md
- [x] FINAL.md (v0.9.45)
- [x] lib.rs
- [x] bridge.rs (keyboard_input 关键路径)
- [ ] seat.rs (1671 行自造)
- [ ] ime/mod.rs
- [ ] ime/input_method_v2.rs
- [ ] ime/text_input_v3.rs
- [ ] ime/relay.rs
- [ ] ime/ime_event.rs
- [ ] host_bridge/mod.rs
- [ ] host_bridge/dbus_ibus.rs
- [ ] host_bridge/dbus_fcitx5.rs
- [ ] seat_smithay.rs (Step 1 dead code)
- [ ] im_smithay.rs (Step 2 tombstone)
- [ ] ime/types.rs
- [ ] ime/tests.rs
- [ ] host_bridge/tests.rs
- [ ] v010 docs 如果存在

## 审查维度（按 checklist）
- [ ] 1. 隐藏耦合
- [ ] 2. 单点故障
- [ ] 3. 锁 / 死锁
- [ ] 4. race condition
- [ ] 5. 数据一致性
- [ ] 6. 错误恢复
- [ ] 7. 生命周期
- [ ] 8. 内存泄漏
- [ ] 9. API 一致性
- [ ] 10. 错误抽象
- [ ] 11. 过度抽象
- [ ] 12. 不必要复杂度
- [ ] 13. 第三方依赖风险
- [ ] 14. 部署问题
- [ ] 15. 可观测性
- [ ] 16. 运维问题

## 输出
写到 `FINDINGS.md`：
- 每条 finding 必须有 file:line
- ≥ 1 Critical
- 包含代码 patch（diff）

## 当前进度
- 启动：第一步读取所有源文件