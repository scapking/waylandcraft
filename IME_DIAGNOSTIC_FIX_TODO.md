# IME_DIAGNOSTIC — 修复 runImeDiagnosticNative NoSuchMethodError

## 现象
日志刷屏（每帧 2 次，surface tree 遍历时）：
```
[waylandcraft][bridge] updateSurfaceTrees: get_or_create_surface 失败:
  NoSuchMethod("Method dev.evvie.waylandcraft.bridge.WaylandCraftBridge.runImeDiagnosticNative(J)Ljava/lang/String; not found")
  （跳过此 surface）
```

## 根因
- Rust 端 `bind_java_type! native_methods` 块声明了
  `name = "runImeDiagnosticNative"`（v0.11.0+ 输入法诊断）
- **Java 端 `WaylandCraftBridge.java` 没有声明对应的 `native` 方法**
- jni-rs 0.22.4 的 `register_native_methods`（JNI RegisterNatives）对**整个
  native_methods 数组**做原子注册——任何一个 callback 在 Java 端找不到对应
  `native` 声明 → 整体失败 → `Err(NoSuchMethod("... runImeDiagnosticNative ..."))`
- 这个错误被 `API::get()` 返回，导致所有 `methods` 块生成的 wrapper
  （`get_or_create_surface` 等）第一次调用就抛错。错误文本里出现的是
  `runImeDiagnosticNative`（第一个注册失败的），不是真正调用的方法名。

## 修复
1. Java 端加 `private static native String runImeDiagnosticNative(long instance);`
2. Java 端加 `public static String runImeDiagnostic(long instance)` wrapper
3. Java 端加 `wl ime diagnostic` 命令接入

## 步骤
- [x] 定位：grep Rust 端 native_methods 块、Java 端 native 声明
- [x] 确认机制：jni-rs 0.22.4 `register_native_methods` 是原子操作
- [x] 在 WaylandCraftBridge.java 加 native 声明 + wrapper
- [x] 在 WaylandCraftCommand.java 注册 `wl ime diagnostic`
- [x] diff 检查干净（3 文件，+75 行）
- [ ] 下次构建验证 Java + Rust 编译都通过

## 改动摘要
- `WaylandCraftBridge.java`：
  - 加 `private static native String runImeDiagnosticNative(long instance)`（与 Rust `name = "runImeDiagnosticNative"` 对应）
  - 加 `public String runImeDiagnostic()` wrapper（处理 nativeAvailable/instance 检查 + UnsatisfiedLinkError 兜底）
- `WaylandCraftCommand.java`：
  - 注册 `wl ime diagnostic` 命令
  - 实现 `imeDiagnostic()` 调用 bridge.runImeDiagnostic()，把 JSON 输出到 chat