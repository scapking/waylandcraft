//! 内部 IME 事件类型（host_bridge 用，协议无关）。
//!
//! 与 host_bridge ImeEvent 配合——但保留 ImeOp/TiCommand/UpEvent 等
//! 兼容 waylandcraft 已有 ime 路径的中间类型。

/// 预编辑文本更新。
#[derive(Debug, Clone, PartialEq)]
pub struct PreeditUpdate {
    pub text: String,
    pub cursor_begin: i32,
    pub cursor_end: i32,
}

impl PreeditUpdate {
    pub fn clear() -> Self {
        Self {
            text: String::new(),
            cursor_begin: 0,
            cursor_end: 0,
        }
    }
    pub fn set(text: impl Into<String>, cursor_begin: i32, cursor_end: i32) -> Self {
        Self {
            text: text.into(),
            cursor_begin,
            cursor_end,
        }
    }
}

/// Commit 文本。
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub text: String,
}

/// 删除 surrounding text。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteSurrounding {
    pub before_length: u32,
    pub after_length: u32,
}

/// LookupTable 候选窗。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LookupTable {
    pub candidates: Vec<String>,
    pub labels: Vec<String>,
    pub cursor_pos: u32,
    pub cursor_visible: bool,
    pub page_size: u32,
    pub orientation: u32,
    pub visible: bool,
}

/// 批次完成（原子应用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Done {
    pub batch_id: u32,
}
