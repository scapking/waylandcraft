//! 内部 IME 事件流（C 方案 Layer 2）。
//!
//! ## 设计原则
//!
//! 三个 IME 协议（XIM / im2 / im1）**都翻译成同一套事件**——业务逻辑（状态机、
//! 焦点路由、缓冲管理）只跟 ImeEvent 打交道，**完全不知道协议层细节**。
//!
//! **绝不**在事件流里暴露：
//! - 协议层概念（serial、done_count 等）——由各协议适配器维护
//! - IME 引擎语义（preedit 是 libpinyin 还是 fcitx5 发的）——透明转发
//! - 时间戳 / 来源标识——超出抽象层
//!
//! ## 方向
//!
//! 事件流是**双向**的：
//! - **下行**（应用 → IME）：`KeyEvent` / `SurroundingText` / `CursorRect` / `Activate` / `Deactivate`
//! - **上行**（IME → 应用）：`PreeditUpdate` / `Commit` / `LookupTable` / `Done`
//!
//! ## 与 HostEvent 的关系
//!
//! 旧的 `system_ime::HostEvent` 是**穿透入站事件**——已经删除。
//! 新的 `ImeEvent` 是**协议无关内部流**——只描述 IME 端点与应用端点之间的
//! 协议无关内容。

use crate::seat::KeyboardAction;

/// IME 焦点 / 状态变化（**下行**：应用告诉 IME "我准备好了" 或 "我走了"）。
///
/// 重命名为 `Focus` 以避免与 `crate::ime::ImeState`（Relay 状态机）冲突。
#[derive(Debug, Clone, PartialEq)]
pub enum FocusChange {
    /// 应用已有一个可输入 IME 文本的会话（text-input v3 已 enable，
    /// 或 X11 客户端已 XIMPreEditStart）。
    Activate,
    /// 应用离开输入上下文。
    Deactivate,
}

/// 按键事件（**下行**：应用把原始按键转给 IME）。
///
/// 不区分 press/release 之外的修饰态（modifiers 由应用在上层跟踪；
/// 这里只传 raw key，IME 自己解释）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyEvent {
    /// XKB keycode（= evdev + 8）。所有协议层都翻译到这一种表达。
    pub keycode: u32,
    /// 按下还是释放。
    pub action: KeyboardAction,
    /// 修饰键（XKB 风格：depressed, latched, locked, group）。
    pub mods: (u32, u32, u32, u32),
}

/// 文本状态同步（**下行**：应用告诉 IME 周围文本 + 光标位置）。
///
/// 这是 XIM `XIMPreEditDraw` / ti3 `set_surrounding_text` 的协议无关版本。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SurroundingText {
    /// 完整 surrounding text（不含 preedit；preedit 由 `PreeditUpdate` 单独传）。
    pub text: String,
    /// 光标在 surrounding text 中的位置（字符偏移）。
    pub cursor: u32,
    /// 选区锚点（光标到锚点之间的文本是当前选区；cursor == anchor 表示无选区）。
    pub anchor: u32,
}

/// 光标矩形（**下行**：候选窗定位用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CursorRect {
    /// x, y, width, height（屏幕坐标，由应用解释——X11 是 screen，
    /// Wayland 是 surface-local；host_bridge 负责协议转换）。
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 预编辑文本更新（**上行**：IME 告诉应用 "我现在有这个 preedit"）。
///
/// 旧 host_ime 用 (text, cursor_begin, cursor_end) 三元组；这里**只**
/// 描述最终结果（带 cursor 标记的字符串），协议层（XIM / im2 / im1）
/// 各自翻译成 wire 格式。
#[derive(Debug, Clone, PartialEq)]
pub struct PreeditUpdate {
    /// preedit 文本（可能为空 = 清空 preedit）
    pub text: String,
    /// preedit 内光标起点（字节偏移）；-1 = 末尾
    pub cursor_begin: i32,
    /// preedit 内光标终点（字节偏移）；-1 = 末尾
    pub cursor_end: i32,
}

impl PreeditUpdate {
    /// 清空 preedit 的便捷构造。
    pub fn clear() -> Self {
        Self {
            text: String::new(),
            cursor_begin: 0,
            cursor_end: 0,
        }
    }

    /// 设置 preedit 的便捷构造。
    pub fn set(text: impl Into<String>, cursor_begin: i32, cursor_end: i32) -> Self {
        Self {
            text: text.into(),
            cursor_begin,
            cursor_end,
        }
    }
}

/// 提交文本（**上行**：IME 把已确认的字符 commit 给应用）。
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub text: String,
}

/// 删除 surrounding text（**上行**：IME 告诉应用 "把这段周围文本删了"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteSurrounding {
    /// 从光标前删的字符数（正数 = 向后删，负数 = 向前删——但通常都是正数）
    pub before_length: u32,
    /// 从光标后删的字符数
    pub after_length: u32,
}

/// 候选窗数据（**上行**：ibus LookupTable / fcitx5 ClientSideUI 归一化）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LookupTable {
    /// 候选字符串（按页内顺序，跨页由 host_bridge 决定是否拆分）
    pub candidates: Vec<String>,
    /// 候选序号标签（ibus 可能为空——渲染侧按 page 补 "1.".."9.","0."）
    pub labels: Vec<String>,
    /// 高亮候选在【当前页内】的下标
    pub cursor_pos: u32,
    pub cursor_visible: bool,
    /// 每页候选数
    pub page_size: u32,
    /// 0=水平 1=垂直 2=系统
    pub orientation: u32,
    /// 是否显示候选窗
    pub visible: bool,
}

/// 批次完成（**上行**：所有 preedit/commit/delete 已发完，应用可以原子应用）。
///
/// 协议层语义：
/// - ti3 / im2: `done(serial)` 触发 app apply
/// - XIM: 没显式 done，但 preedit/commit 之间天然就是保序的
/// - im1: 同 ti3
///
/// ImeEvent 流里**有显式 Done 标记**——XIM 适配器自己生成"每次 commit 后
/// 立即 Done"的伪 Done。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Done {
    /// 调试用：标识这是哪个批次的 Done（协议层给，不严格保证唯一）
    pub batch_id: u32,
}

/// 下行事件（应用 → IME）。
#[derive(Debug, Clone, PartialEq)]
pub enum DownEvent {
    State(FocusChange),
    Key(KeyEvent),
    Surrounding(SurroundingText),
    CursorRect(CursorRect),
}

/// 上行事件（IME → 应用）。
#[derive(Debug, Clone, PartialEq)]
pub enum UpEvent {
    Preedit(PreeditUpdate),
    Commit(Commit),
    DeleteSurrounding(DeleteSurrounding),
    LookupTable(LookupTable),
    Done(Done),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preedit_clear_works() {
        let p = PreeditUpdate::clear();
        assert!(p.text.is_empty());
        assert_eq!(p.cursor_begin, 0);
        assert_eq!(p.cursor_end, 0);
    }

    #[test]
    fn preedit_set_works() {
        let p = PreeditUpdate::set("nihao", 0, 2);
        assert_eq!(p.text, "nihao");
        assert_eq!(p.cursor_begin, 0);
        assert_eq!(p.cursor_end, 2);
    }

    #[test]
    fn down_event_equality() {
        let a = DownEvent::State(FocusChange::Activate);
        let b = DownEvent::State(FocusChange::Activate);
        assert_eq!(a, b);
        let c = DownEvent::State(FocusChange::Deactivate);
        assert_ne!(a, c);
    }

    #[test]
    fn up_event_commit_works() {
        let e = UpEvent::Commit(Commit { text: "汉".into() });
        match e {
            UpEvent::Commit(c) => assert_eq!(c.text, "汉"),
            _ => panic!("wrong variant"),
        }
    }
}
