//! 内置拼音引擎（纯逻辑，零 Wayland 类型、零 IPC、零外部进程）。
//!
//! 词库为编译期生成的音节 trie（格式与解析见 super::dict），随 cdylib
//! `include_bytes!` 内嵌；运行时零加载：`Dict::parse` 只扫几百个音节表项并
//! 校验边界（µs~ms 级），查询直接落在内嵌字节流上。
//!
//! ## 键语义（与 fcitx5 pinyin 对齐的 MVP）
//! - 字母键：能延续有效拼音切分才消费（preedit=原始缓冲）；不能延续则
//!   **不消费**（raw 直通，杜绝吞键）。
//! - 歧义切分（nihao=ni+hao、xian=xi+an）：trie 按字母流枚举全部切分路径，
//!   候选按词条**词频降序**（词频来自 jieba 现代汉语语料计数）。
//! - 数字 1-9：选当前页第 n 个候选；空格：选高亮候选；
//!   左右：移动高亮（自动跟页）；PgUp/PgDn、-、=：翻页；
//!   退格：编辑缓冲；回车/无候选空格：原样提交缓冲 ASCII；
//!   Esc：取消组合；撇号：提交高亮候选（断句）。
//! - 裸 Shift press+release 切换 拼音/西文 模式（西文=全部直通，英文输入
//!   零干扰；持久到再次切换）。门面另有 `/` 前导命令行的直通豁免。
//! - Ctrl/Alt/Super 组合一律不消费（游戏快捷键零干扰），组合缓冲保留。
//!
//! 输出为 relay 的 ImeOp 批次（Preedit/Commit），由门面（super）经既有
//! relay→emit_ti_batch 出口交付应用——与 im2/宿主路径共用下游，引擎本身
//! 不触碰任何 wire 对象。

use super::dict::Dict;
use crate::ime::relay::ImeOp;

pub const PAGE_SIZE: usize = 9;
/// 单次收集的最大候选数（多页浏览上限，同时控制每键计算量）。
pub const MAX_COLLECT: usize = 128;
/// 每次收集的 trie 访问预算（防御性上限，防畸形输入阻塞 tick）。
const WALK_BUDGET: u32 = 200_000;
/// 组合缓冲长度上限（拼音音节串极长时无意义，防御游戏内长按住）。
const BUFFER_MAX: usize = 32;

/// 一个候选词条。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub word: String,
    pub freq: u32,
    /// true = 键完整覆盖当前缓冲；false = 缓冲前缀的延长候选。
    pub full: bool,
}

/// 单次按键的处理结果。
#[derive(Debug, Default)]
pub struct KeyResult {
    /// true = 引擎消费该键（门面不得再发 VK/raw）。
    pub consumed: bool,
    /// 需要原子交付给应用的 IME 操作批（非空时门面 flush+emit）。
    pub ops: Vec<ImeOp>,
}

impl KeyResult {
    fn pass() -> Self {
        KeyResult { consumed: false, ops: Vec::new() }
    }
    fn consumed() -> Self {
        KeyResult { consumed: true, ops: Vec::new() }
    }
}

// ── evdev 键码常量（xkb keycode = evdev + 8；门面传入 xkb 码）──
const EV_ESC: u32 = 1;
const EV_DIGIT1: u32 = 2; // digit n → EV_DIGIT1 + n - 1
const EV_DIGIT9: u32 = 10;
const EV_DIGIT0: u32 = 11;
const EV_MINUS: u32 = 12;
const EV_EQUAL: u32 = 13;
const EV_BACKSPACE: u32 = 14;
const EV_TAB: u32 = 15;
const EV_ENTER: u32 = 28;
const EV_APOSTROPHE: u32 = 40;
const EV_SHIFT_L: u32 = 42;
const EV_SHIFT_R: u32 = 54;
const EV_SPACE: u32 = 57;
const EV_PAGEUP: u32 = 104;
const EV_LEFT: u32 = 105;
const EV_RIGHT: u32 = 106;
const EV_PAGEDOWN: u32 = 109;

/// US 物理键盘字母表（evdev → ascii 小写）。布局假设：物理 US QWERTY；
/// 非 US 布局用户建议走 fcitx5 路径（xkb 全布局感知）——见交付报告局限节。
fn evdev_to_letter(key: u32) -> Option<u8> {
    let table: &[(u32, u8)] = &[
        (16, b'q'), (17, b'w'), (18, b'e'), (19, b'r'), (20, b't'), (21, b'y'),
        (22, b'u'), (23, b'i'), (24, b'o'), (25, b'p'),
        (30, b'a'), (31, b's'), (32, b'd'), (33, b'f'), (34, b'g'), (35, b'h'),
        (36, b'j'), (37, b'k'), (38, b'l'),
        (44, b'z'), (45, b'x'), (46, b'c'), (47, b'v'), (48, b'b'), (49, b'n'),
        (50, b'm'),
    ];
    table.iter().find(|(k, _)| *k == key).map(|(_, c)| *c)
}

/// xkb depressed mods 位：Shift=1, Control=4, Mod1(Alt)=8, Mod4(Super)=64。
const MOD_SHIFT: u32 = 1;
const MOD_CHORD: u32 = 4 | 8 | 64;

const PAGE_LABELS: [&str; PAGE_SIZE] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// 引擎状态。挂在 ImeState 上；Default = 拼音模式、空闲、无缓冲。
pub struct BuiltinEngine {
    dict: Option<Dict>,
    /// 拼音/西文切换（裸 Shift 翻转；西文 = 全部键直通）。
    english_mode: bool,
    /// 组合缓冲（仅 a-z，preedit 显示即原始串）。
    buffer: Vec<u8>,
    /// 本次缓冲重算得到的候选（freq 降序，≤ MAX_COLLECT）。
    candidates: Vec<Candidate>,
    /// 高亮（全局下标）与当前页。
    cursor: usize,
    page: usize,
    /// press 已被引擎消费的键（evdev 码）；release 同路消费，杜绝孤立 release。
    pressed: std::collections::HashSet<u32>,
    /// Shift 在场且期间无其他键消费 → release 时翻转模式。
    shift_standalone: bool,
    /// 快照版本：任何 Java 可见状态变化（缓冲/候选/页/高亮/模式）+1。
    version: u64,
    /// 已交付给轮询方的版本（IME 侧节流：仅变化时序列化）。
    delivered: u64,
}

impl Default for BuiltinEngine {
    fn default() -> Self {
        Self {
            dict: None,
            english_mode: false,
            buffer: Vec::new(),
            candidates: Vec::new(),
            cursor: 0,
            page: 0,
            pressed: std::collections::HashSet::new(),
            shift_standalone: false,
            version: 0,
            // 初始即视为「有变化」：首个轮询总拿到一次基线快照（idle 形态），
            // 之后仅状态真实变化时 Some。
            delivered: u64::MAX,
        }
    }
}

impl BuiltinEngine {
    /// 测试/定制入口：注入词典（fixture 小词表与真实内嵌词库共用同一格式）。
    pub fn with_dict(dict: Option<Dict>) -> Self {
        Self { dict, ..Default::default() }
    }

    pub fn set_dict(&mut self, dict: Option<Dict>) {
        self.dict = dict;
    }

    pub fn has_dict(&self) -> bool {
        self.dict.is_some()
    }

    pub fn english_mode(&self) -> bool {
        self.english_mode
    }

    pub fn composing(&self) -> bool {
        !self.buffer.is_empty()
    }

    pub fn buffer(&self) -> &str {
        std::str::from_utf8(&self.buffer).unwrap_or("")
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// 测试合同访问器：完整候选序列（含排序断言）。
    #[cfg(test)]
    pub(crate) fn candidates_for_test(&self) -> &[Candidate] {
        &self.candidates
    }

    pub fn dict_stats(&self) -> Option<(u32, u32, u32)> {
        self.dict.as_ref().map(|d| d.stats())
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    /// 全量复位（路由切换/失焦时用）：清缓冲与候选并推版本。
    /// 返回是否有在途组合（门面据此决定是否补发空 preedit）。
    pub fn cancel(&mut self) -> bool {
        let had = self.composing() || !self.candidates.is_empty();
        self.buffer.clear();
        self.candidates.clear();
        self.cursor = 0;
        self.page = 0;
        self.pressed.clear();
        self.shift_standalone = false;
        if had {
            self.bump();
        }
        had
    }

    /// 处理一次按键。`keycode` 为 xkb 码（evdev+8），与 im2 grab 转发同源。
    pub fn on_key(&mut self, keycode: u32, press: bool, mods_depressed: u32) -> KeyResult {
        let evdev = keycode.saturating_sub(8);
        if !press {
            return self.on_release(evdev);
        }
        self.pressed.insert(evdev);
        let r = self.on_press(evdev, mods_depressed);
        if !r.consumed {
            self.pressed.remove(&evdev);
        }
        r
    }

    fn on_release(&mut self, evdev: u32) -> KeyResult {
        if !self.pressed.remove(&evdev) {
            // 引擎没按住的键：release 直通（与 press 同路的不变式由
            // pressed 集合维护）。Shift 单独跟踪。
            self.shift_standalone = false;
            return KeyResult::pass();
        }
        if (evdev == EV_SHIFT_L || evdev == EV_SHIFT_R)
            && self.shift_standalone
            && !self.composing()
        {
            self.shift_standalone = false;
            self.english_mode = !self.english_mode;
            self.bump();
        }
        KeyResult::consumed()
    }

    fn on_press(&mut self, evdev: u32, mods: u32) -> KeyResult {
        // Ctrl/Alt/Super 组合：引擎永不裁决（游戏快捷键原样直通）。
        if mods & MOD_CHORD != 0 {
            self.shift_standalone = false;
            return KeyResult::pass();
        }
        if evdev == EV_SHIFT_L || evdev == EV_SHIFT_R {
            self.shift_standalone = true;
            return KeyResult::consumed();
        }
        // 组合中按了别的键 → Shift 不再是"裸按"。
        self.shift_standalone = false;
        if self.dict.is_none() {
            // 词库缺失（构建异常）：引擎不可用，全部直通。
            return KeyResult::pass();
        }
        // Esc 永远先作模式/组合出口处理（西文模式退出）。
        if evdev == EV_ESC && self.english_mode && !self.composing() {
            self.english_mode = false;
            self.bump();
            return KeyResult::consumed();
        }
        // 西文模式：其余全部直通。
        if self.english_mode {
            return KeyResult::pass();
        }
        if let Some(ch) = evdev_to_letter(evdev) {
            // Shift+字母 = 大写英文 → 直通（候选窗不收大写）。
            if mods & MOD_SHIFT != 0 {
                return KeyResult::pass();
            }
            return self.push_letter(ch);
        }
        match evdev {
            EV_DIGIT1..=EV_DIGIT9 => self.select_page_index((evdev - EV_DIGIT1) as usize),
            EV_DIGIT0 => self.select_page_index(9),
            EV_SPACE => {
                if self.candidates.is_empty() {
                    if self.composing() {
                        self.commit_buffer_raw()
                    } else {
                        KeyResult::pass()
                    }
                } else {
                    self.select_global(self.cursor)
                }
            }
            EV_ENTER => {
                if self.composing() {
                    self.commit_buffer_raw()
                } else {
                    KeyResult::pass()
                }
            }
            EV_BACKSPACE => {
                if self.composing() {
                    self.buffer.pop();
                    self.recompose();
                    self.preedit_batch()
                } else {
                    KeyResult::pass()
                }
            }
            EV_ESC => {
                if self.composing() {
                    let had = self.cancel();
                    KeyResult {
                        consumed: true,
                        ops: if had { vec![ImeOp::Preedit(String::new(), 0, 0)] } else { Vec::new() },
                    }
                } else {
                    KeyResult::pass()
                }
            }
            EV_APOSTROPHE => {
                if self.composing() {
                    self.select_global(self.cursor)
                } else {
                    KeyResult::pass()
                }
            }
            EV_TAB => {
                if self.composing() && !self.candidates.is_empty() {
                    self.select_global(self.cursor)
                } else {
                    KeyResult::pass()
                }
            }
            EV_PAGEUP | EV_MINUS => self.page_step(-1),
            EV_PAGEDOWN | EV_EQUAL => self.page_step(1),
            EV_LEFT => self.cursor_step(-1),
            EV_RIGHT => self.cursor_step(1),
            _ => KeyResult::pass(),
        }
    }

    fn push_letter(&mut self, ch: u8) -> KeyResult {
        if self.buffer.len() >= BUFFER_MAX {
            // 缓冲已满：不再吃字母（防异常输入），组合原样保留。
            return KeyResult::consumed();
        }
        self.buffer.push(ch);
        if self.recompose() {
            return self.preedit_batch();
        }
        // 非法延续：撤回该字母，不消费（raw 直通，组合状态不变）。
        self.buffer.pop();
        KeyResult::pass()
    }

    fn preedit_batch(&mut self) -> KeyResult {
        let pre = self.buffer().to_string();
        self.bump();
        KeyResult {
            consumed: true,
            ops: vec![ImeOp::Preedit(pre.clone(), pre.len() as i32, pre.len() as i32)],
        }
    }

    fn select_page_index(&mut self, idx: usize) -> KeyResult {
        let global = self.page * PAGE_SIZE + idx;
        if self.composing() && global < self.candidates.len() {
            self.select_global(global)
        } else {
            KeyResult::pass()
        }
    }

    fn select_global(&mut self, idx: usize) -> KeyResult {
        let Some(c) = self.candidates.get(idx).cloned() else {
            // 高亮越界防御：回退页首；再没有 → 原样提交缓冲。
            let start = self.page * PAGE_SIZE;
            if idx != 0 && start < self.candidates.len() {
                return self.select_global(start);
            }
            return self.commit_buffer_raw();
        };
        let ops = vec![ImeOp::Preedit(String::new(), 0, 0), ImeOp::CommitString(c.word)];
        self.buffer.clear();
        self.candidates.clear();
        self.cursor = 0;
        self.page = 0;
        self.bump();
        KeyResult { consumed: true, ops }
    }

    /// 回车/无候选空格/撇号兜底：原样提交缓冲（英文收尾语义）。
    fn commit_buffer_raw(&mut self) -> KeyResult {
        let raw = self.buffer().to_string();
        self.buffer.clear();
        self.candidates.clear();
        self.cursor = 0;
        self.page = 0;
        self.bump();
        KeyResult {
            consumed: true,
            ops: vec![ImeOp::Preedit(String::new(), 0, 0), ImeOp::CommitString(raw)],
        }
    }

    fn page_step(&mut self, delta: i32) -> KeyResult {
        let pages = self.page_count();
        if pages <= 1 || !self.composing() {
            return KeyResult::pass();
        }
        let mut p = self.page as i32 + delta;
        p = p.clamp(0, pages as i32 - 1);
        if p != self.page as i32 {
            self.page = p as usize;
            self.cursor = self.page * PAGE_SIZE;
            self.bump();
            // 候选页变化（preedit 不变）→ 无协议批次，快照通道自会推进。
        }
        KeyResult::consumed()
    }

    fn cursor_step(&mut self, delta: i32) -> KeyResult {
        if self.candidates.is_empty() || !self.composing() {
            return KeyResult::pass();
        }
        let last = self.candidates.len() - 1;
        let mut c = (self.cursor as i32 + delta).clamp(0, last as i32);
        if c < 0 {
            c = 0;
        }
        if c as usize != self.cursor {
            self.cursor = c as usize;
            self.page = self.cursor / PAGE_SIZE;
            self.bump();
        }
        KeyResult::consumed()
    }

    /// 候选面板点选（页内下标 0..8）：等价数字键，走同一提交批次语义。
    pub fn ui_select_page_index(&mut self, idx: usize) -> KeyResult {
        self.select_page_index(idx)
    }

    /// 候选面板翻页（±1）：等价 PgDn/PgUp。
    pub fn ui_page_step(&mut self, delta: i32) -> KeyResult {
        self.page_step(delta)
    }

    pub fn page_count(&self) -> usize {
        (self.candidates.len() + PAGE_SIZE - 1) / PAGE_SIZE
    }

    /// 当前页候选（word, label）。
    pub fn current_page(&self) -> Vec<(&str, &'static str)> {
        let start = self.page * PAGE_SIZE;
        self.candidates[start..]
            .iter()
            .take(PAGE_SIZE)
            .enumerate()
            .map(|(i, c)| (c.word.as_str(), PAGE_LABELS[i]))
            .collect()
    }

    /// 页内高亮下标（无候选时 0）。
    pub fn page_cursor(&self) -> usize {
        self.cursor % PAGE_SIZE
    }

    /// 快照 JSON（JNI 轮询出口；仅变化时返回 Some）。
    ///
    /// 合同（与 Java ImeBuiltinPanel.parse 对齐，单测 golden 锁形状）：
    /// `{"visible":bool,"preedit":"…","cursor":N,"page":N,"pages":N,
    ///   "page_size":9,"candidates":["…"],"labels":["1"…]}`
    pub fn snapshot_json(&mut self) -> Option<String> {
        if self.version == self.delivered {
            return None;
        }
        self.delivered = self.version;
        let visible = self.composing() && !self.candidates.is_empty();
        let page = self.current_page();
        let cands: Vec<String> = page.iter().map(|(w, _)| json_str(w)).collect();
        let labels: Vec<String> = page.iter().map(|(_, l)| json_str(l)).collect();
        Some(format!(
            "{{\"visible\":{},\"preedit\":{},\"cursor\":{},\"page\":{},\"pages\":{},\"page_size\":{},\"candidates\":[{}],\"labels\":[{}]}}",
            visible,
            json_str(self.buffer()),
            self.page_cursor(),
            self.page,
            self.page_count(),
            PAGE_SIZE,
            cands.join(","),
            labels.join(","),
        ))
    }

    /// 重算候选。返回 true = 缓冲存在合法切分前景（有候选或可继续拼）。
    fn recompose(&mut self) -> bool {
        let Some(dict) = self.dict.as_ref() else {
            self.candidates.clear();
            return false;
        };
        let buf = self.buffer.clone();
        self.candidates = collect(dict, &buf, MAX_COLLECT);
        let ok = !self.candidates.is_empty() || has_valid_prefix(dict, &buf);
        self.cursor = 0;
        self.page = 0;
        ok
    }
}

/// JSON 字符串字面量（候选=汉字词、缓冲=a-z；引号/反斜杠/控制符防御性转义）。
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 缓冲是否存在至少一条「全音节 + 可选尾音节前缀」的切分路径
/// （字母消费判定：无词条但可继续拼的前缀也算合法）。
fn has_valid_prefix(dict: &Dict, buf: &[u8]) -> bool {
    let mut budget = WALK_BUDGET;
    walk_prefix(dict, 0, buf, 0, &mut budget)
}

fn walk_prefix(dict: &Dict, node: u32, buf: &[u8], pos: usize, budget: &mut u32) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    if pos == buf.len() {
        return true;
    }
    let (fe, ec, _ft, _tc, _sm) = dict.node(node as usize);
    for (sid, child) in dict.edges_of(fe, ec) {
        let Some(syl) = dict.syllable(sid) else { continue };
        let rem = &buf[pos..];
        if rem.len() >= syl.len() && rem[..syl.len()] == *syl {
            if walk_prefix(dict, child, buf, pos + syl.len(), budget) {
                return true;
            }
        } else if syl.starts_with(rem) {
            return true;
        }
    }
    false
}

// ═══════════════ 候选收集（性能合同：每键 O(k log k + 剪枝后节点数)）═══════════════

/// 有界 top-k 延长候选池：最小堆按 freq 淘汰；去重留到最后（同词多路径
/// 罕见，堆槽浪费可忽略）。
struct ContHeap<'a> {
    k: usize,
    // (freq, word) 小顶：堆顶 = 当前门槛。
    heap: std::collections::BinaryHeap<std::cmp::Reverse<(u32, &'a str)>>,
    budget: u32,
}

impl<'a> ContHeap<'a> {
    fn new(k: usize) -> Self {
        Self { k, heap: std::collections::BinaryHeap::new(), budget: WALK_BUDGET }
    }
    fn threshold(&self) -> u32 {
        if self.heap.len() < self.k {
            0
        } else {
            self.heap.peek().map(|std::cmp::Reverse(f)| f.0).unwrap_or(0)
        }
    }
    fn offer(&mut self, freq: u32, word: &'a str) {
        if self.heap.len() < self.k {
            self.heap.push(std::cmp::Reverse((freq, word)));
        } else if let Some(&std::cmp::Reverse((min, _))) = self.heap.peek() {
            if freq > min {
                self.heap.pop();
                self.heap.push(std::cmp::Reverse((freq, word)));
            }
        }
    }
    fn into_words(self) -> Vec<(&'a str, u32)> {
        self.heap.into_iter().map(|std::cmp::Reverse((f, w))| (w, f)).collect()
    }
}

/// 收集候选：
/// 1. 全切分路径末端节点的终端词（完整覆盖，全部保留——数量天然很小）；
/// 2. 路径上每步「尾音节前缀」命中子树的高频延长词（有界堆 + sub_max 剪枝）。
/// 排序合同：完整覆盖段在前（freq 降序），延长段在后（freq 降序）。
pub(crate) fn collect(dict: &Dict, buf: &[u8], k: usize) -> Vec<Candidate> {
    use std::collections::HashMap;
    let mut full: HashMap<&str, (u32, bool)> = HashMap::new(); // word -> (max freq)
    let mut cont = ContHeap::new(k);
    walk_collect(dict, 0, buf, 0, &mut full, &mut cont);
    let mut out: Vec<Candidate> = Vec::with_capacity(full.len() + cont.heap.len());
    let mut cont_seen: std::collections::HashSet<&str> = Default::default();
    for (w, (freq, ..)) in full {
        out.push(Candidate { word: w.to_string(), freq, full: true });
    }
    for (w, freq) in cont.into_words() {
        if !cont_seen.insert(w) {
            continue;
        }
        if out.iter().any(|c| c.full && c.word == w) {
            continue; // 完整覆盖已有同词
        }
        out.push(Candidate { word: w.to_string(), freq, full: false });
    }
    out.sort_by(|a, b| b.full.cmp(&a.full).then_with(|| b.freq.cmp(&a.freq)).then_with(|| a.word.cmp(&b.word)));
    out
}

fn walk_collect<'a>(
    dict: &'a Dict,
    node: u32,
    buf: &[u8],
    pos: usize,
    full: &mut std::collections::HashMap<&'a str, (u32, bool)>,
    cont: &mut ContHeap<'a>,
) {
    if cont.budget == 0 {
        return;
    }
    cont.budget -= 1;
    let (fe, ec, ft, tc, _sm) = dict.node(node as usize);
    if pos == buf.len() {
        // 精确覆盖节点：终端词全部保留（同节点天然 freq 降序）。
        for (w, freq, _kind) in dict.terms_of(ft, tc) {
            let e = full.entry(w).or_insert((0, false));
            if freq > e.0 {
                e.0 = freq;
            }
        }
    }
    let rem = &buf[pos..];
    for (sid, child) in dict.edges_of(fe, ec) {
        let Some(syl) = dict.syllable(sid) else { continue };
        if rem.len() >= syl.len() && rem[..syl.len()] == *syl {
            walk_collect(dict, child, buf, pos + syl.len(), full, cont);
        } else if syl.starts_with(rem) {
            add_subtree(dict, child, cont);
        }
    }
}

/// 子树延长候选：迭代栈 + sub_max 堆剪枝（O(访问节点数)，与词库规模无关
/// 的有效上界——门槛一满，低频子树整棵跳过）。
fn add_subtree<'a>(dict: &'a Dict, root: u32, cont: &mut ContHeap<'a>) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if cont.budget == 0 {
            return;
        }
        cont.budget -= 1;
        let (fe, ec, _ft, _tc, sm) = dict.node(n as usize);
        if sm < cont.threshold() {
            continue;
        }
        let (_fe, _ec, ft, tc, _sm) = dict.node(n as usize);
        for (w, freq, _kind) in dict.terms_of(ft, tc) {
            cont.offer(freq, w);
        }
        for (_sid, child) in dict.edges_of(fe, ec) {
            stack.push(child);
        }
    }
}
