// 注入 native 构建标识（git 短 hash），启动日志可一眼确认 native 版本，
// 避免"版本号新、native 旧"的构建/分发错位问题。
fn main() {
    // 优先级：CI 显式注入 > git rev-parse > unknown。
    // env 变化会触发重跑，target 缓存不影响正确性。
    let hash = std::env::var("WAYLANDCRAFT_GIT_HASH")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=WAYLANDCRAFT_GIT_HASH={hash}");
    println!("cargo:rerun-if-env-changed=WAYLANDCRAFT_GIT_HASH");
    println!("cargo:rerun-if-changed=src/system_ime.rs");
}
