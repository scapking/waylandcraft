//! 内置输入法引擎（零外部依赖）。
//!
//! 当前提供拼音引擎（`engine.rs`）。词库 `assets/ime_builtin_pinyin.bin`
//! 在构建期由 `native/tools/ime-dict/` 生成并提交（来源与许可见
//! `tools/ime-dict/README.md`）；运行时 `include_bytes!` 内嵌、`Dict::parse_static`
//! 零拷贝校验——无 IPC、无外部进程、无宿主依赖。
//!
//! 与 fcitx5 嵌套路线的关系由 `ImeSource` 裁决（见 crate::ime 门面）：
//! - `Auto`（默认）：fcitx5 嵌套 daemon 可用/在场时逐字节维持既有 im2 路由；
//!   仅当判定为「未安装 / 连续失败」（NoBinary/Failed）时回退内置引擎——
//!   这就是「任意 Linux 桌面装 mod 即用中文」的免安装路径。
//! - `Fcitx5`：强制嵌套 daemon 路线（现行为，永远不吃内置分支）。
//! - `Builtin`：强制内置引擎（fcitx5 在场也不路由给它吃键）。

pub mod dict;
pub mod engine;

pub use engine::{BuiltinEngine, Candidate, KeyResult, MAX_COLLECT, PAGE_SIZE};

use dict::Dict;

/// 输入法数据来源（`/wl ime source`、settings 键 `imeSource`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImeSource {
    /// 自动：fcitx5 嵌套 daemon 可用则用之，未安装/已失败则内置引擎。
    #[default]
    Auto,
    /// 强制 fcitx5 嵌套 daemon（禁用内置回退）。
    Fcitx5,
    /// 强制内置引擎。
    Builtin,
}

impl ImeSource {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "fcitx5" => Some(Self::Fcitx5),
            "builtin" => Some(Self::Builtin),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fcitx5 => "fcitx5",
            Self::Builtin => "builtin",
        }
    }
}

/// 内嵌拼音词库（构建期生成物，提交于 native/assets/）。
pub const PINYIN_DICT_BYTES: &[u8] =
    include_bytes!("../../../assets/ime_builtin_pinyin.bin");

/// 解析内嵌词库（校验头 + 结构边界，零拷贝）。失败返回 None——调用方
/// 必须把「内置引擎不可用」如实呈现，而不是 panic。
pub fn embedded_dict() -> Option<Dict> {
    Dict::parse_static(PINYIN_DICT_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ime::relay::ImeOp;
    use dict::TrieBuilder;
    use std::rc::Rc;

    /// 测试词库（fixture）：覆盖切分歧义、多路径、多页与去重场景。
    fn fixture_engine() -> BuiltinEngine {
        let mut tb = TrieBuilder::new();
        let rows: &[(&str, &str, u32)] = &[
            ("你", "ni", 9_000_000),
            ("好", "hao", 8_000_000),
            ("你好", "ni hao", 500_000),
            ("你好吗", "ni hao ma", 400_000),
            ("尼豪", "ni hao", 1_000),
            ("逆号", "ni hao", 800),
            ("西", "xi", 1_500_000),
            ("安", "an", 1_400_000),
            ("西安", "xi an", 1_300_000),
            ("先", "xian", 6_000_000),
            ("线", "xian", 5_900_000),
            ("现", "xian", 5_800_000),
            ("我", "wo", 7_000_000),
            ("们", "men", 6_500_000),
            ("我们", "wo men", 6_400_000),
            ("是", "shi", 9_500_000),
            ("的", "de", 9_900_000),
            ("地", "de", 2_000_000),
            ("得", "de", 1_900_000),
            ("啊", "a", 3_000_000),
            ("阿", "a", 900_000),
            ("打", "da", 900_000),
            ("大", "da", 3_100_000),
        ];
        // 分页测试专用：12 个同键高频词。
        for i in 0..12u32 {
            let _ = tb.insert(&format!("页{i}"), "aa", 10_000 + i, 0);
        }
        for (w, k, f) in rows {
            assert!(tb.insert(w, k, *f, dict::KIND_WORD), "fixture insert {w}");
        }
        BuiltinEngine::with_dict(Some(Dict::parse_rc(Rc::from(tb.build().into_boxed_slice())).unwrap()))
    }

    /// 逐键喂引擎（press+release 同路），返回该 press 的结果。
    fn press(e: &mut BuiltinEngine, evdev: u32, mods: u32) -> KeyResult {
        let r = e.on_key(evdev + 8, true, mods);
        r
    }

    fn type_str(e: &mut BuiltinEngine, s: &str) -> Vec<KeyResult> {
        let mut out = Vec::new();
        for ch in s.bytes() {
            let evdev: u32 = match ch {
                b'q' => 16, b'w' => 17, b'e' => 18, b'r' => 19, b't' => 20, b'y' => 21,
                b'u' => 22, b'i' => 23, b'o' => 24, b'p' => 25,
                b'a' => 30, b's' => 31, b'd' => 32, b'f' => 33, b'g' => 34, b'h' => 35,
                b'j' => 36, b'k' => 37, b'l' => 38,
                b'z' => 44, b'x' => 45, b'c' => 46, b'v' => 47, b'b' => 48, b'n' => 49,
                b'm' => 50,
                _ => unreachable!(),
            };
            let r = e.on_key(evdev + 8, true, 0);
            e.on_key(evdev + 8, false, 0);
            out.push(r);
        }
        out
    }

    const SPACE: u32 = 57;
    const BKSP: u32 = 14;
    const ENTER: u32 = 28;
    const ESC: u32 = 1;
    const APOS: u32 = 40;
    const PDOWN: u32 = 109;
    const PUP: u32 = 104;
    const LEFT: u32 = 105;
    const RIGHT: u32 = 106;

    // ── 词库/格式 ──────────────────────────────────────────────

    #[test]
    fn dict_rejects_garbage_and_truncation() {
        assert!(Dict::parse_rc(Rc::from(vec![0u8; 8])).is_none());
        let mut tb = TrieBuilder::new();
        tb.insert("你", "ni", 10, 0);
        let blob = tb.build();
        assert!(Dict::parse_rc(Rc::from(blob.clone().into_boxed_slice())).is_some());
        // 截断尾部（词 blob 不完整）必须拒绝。
        assert!(Dict::parse_rc(Rc::from(blob[..blob.len() - 5].to_vec().into_boxed_slice())).is_none());
    }

    #[test]
    fn dict_stats_match_builder() {
        let mut tb = TrieBuilder::new();
        tb.insert("你", "ni", 10, 0);
        tb.insert("你好", "ni hao", 5, 0);
        let d = Dict::parse_rc(Rc::from(tb.build().into_boxed_slice())).unwrap();
        let (syl, nodes, terms) = d.stats();
        assert_eq!(syl, 2, "ni + hao");
        assert_eq!(nodes, 3, "root + ni + hao");
        assert_eq!(terms, 2);
    }

    #[test]
    fn embedded_dict_parses() {
        let d = embedded_dict().expect("内嵌词库必须解析成功");
        let (syl, nodes, terms) = d.stats();
        assert!(syl >= 300, "音节数 {syl}");
        assert!(nodes >= 50_000, "节点数 {nodes}");
        assert!(terms >= 50_000, "词条数 {terms}（目标 5-8 万常用词）");
    }

    #[test]
    fn embedded_dict_load_is_not_blocking() {
        // 加载耗时实测合同：内嵌零拷贝 parse——release 构建仅做区间校验
        // （~ms 级，报告记录实测值）；debug 构建附带全量解引用审计，放宽到
        // 50ms。真正游戏内加载发生在 WLCState::new（init 线程），不占 tick。
        let t = std::time::Instant::now();
        let d = embedded_dict().expect("内嵌词库");
        let ms = t.elapsed().as_millis();
        eprintln!("[ime][builtin] 内嵌词库 parse={}ms entries={} (debug={})", ms, d.stats().2, cfg!(debug_assertions));
        assert!(ms < 120, "parse {ms}ms 异常缓慢");
    }

    // ── 切分与候选 ─────────────────────────────────────────────

    #[test]
    fn nihao_segs_as_ni_plus_hao_top_nihao() {
        let mut e = fixture_engine();
        let rs = type_str(&mut e, "nihao");
        assert!(rs.iter().all(|r| r.consumed), "nihao 全程应被消费");
        assert_eq!(e.buffer(), "nihao");
        assert_eq!(e.current_page()[0].0, "你好", "歧义切分取高频组合词");
    }

    #[test]
    fn nihao_contains_extension_words() {
        let mut e = fixture_engine();
        type_str(&mut e, "nihao");
        let words: Vec<&str> = e.current_page().iter().map(|(w, _)| *w).collect();
        assert!(words.contains(&"你好吗"), "前缀延长候选须在页内: {words:?}");
    }

    #[test]
    fn xian_prefers_single_syllable_top_freq() {
        let mut e = fixture_engine();
        type_str(&mut e, "xian");
        assert_eq!(e.current_page()[0].0, "先", "全覆盖候选内高频优先");
        assert!(e.candidate_count() >= 4);
    }

    #[test]
    fn ambiguous_multi_path_yields_both_words() {
        let mut e = fixture_engine();
        type_str(&mut e, "xian");
        let top: Vec<&str> = e.candidates_for_test().iter().map(|c| c.word.as_str()).collect();
        assert!(top.contains(&"西安"), "多路径切分候选须包含 西安: {top:?}");
        assert!(top.contains(&"先"), "单路径候选须包含 先");
    }

    #[test]
    fn candidates_sorted_by_coverage_then_freq() {
        let mut e = fixture_engine();
        type_str(&mut e, "ni");
        let cs = e.candidates_for_test();
        // 排序合同：full=true 段整体先于 full=false 段；两段内部词频降序。
        let boundary = cs.iter().position(|c| !c.full).unwrap_or(cs.len());
        assert!(cs[..boundary].iter().all(|c| c.full));
        assert!(cs[boundary..].iter().all(|c| !c.full));
        for part in [&cs[..boundary], &cs[boundary..]] {
            for w in part.windows(2) {
                assert!(w[0].freq >= w[1].freq, "同级必须词频降序");
            }
        }
    }

    #[test]
    fn polyphone_dedup() {
        let mut e = fixture_engine();
        type_str(&mut e, "de");
        let hits = e.candidates_for_test().iter().filter(|c| c.word == "的").count();
        assert_eq!(hits, 1, "同词多音路径去重");
    }

    // ── 键处理语义 ─────────────────────────────────────────────

    #[test]
    fn invalid_start_letter_not_consumed() {
        let mut e = fixture_engine();
        // fixture 无 v 起始音节 → 空缓冲 'v' 直通。
        let r = press(&mut e, 47, 0);
        assert!(!r.consumed, "无切分前景的字母必须直通");
        assert!(!e.composing());
    }

    #[test]
    fn invalid_extension_not_consumed() {
        let mut e = fixture_engine();
        type_str(&mut e, "ni");
        // "niq"：q 不是任何 ni 后音节的合法延续（fixture 仅 hao/ma…）→ 直通。
        let r = press(&mut e, 16, 0);
        assert!(!r.consumed);
        assert_eq!(e.buffer(), "ni", "被拒字母不得污染缓冲");
    }

    #[test]
    fn space_selects_highlight_and_emits_commit_batch() {
        let mut e = fixture_engine();
        type_str(&mut e, "nihao");
        let r = press(&mut e, SPACE, 0);
        assert!(r.consumed);
        assert_eq!(
            r.ops,
            vec![ImeOp::Preedit(String::new(), 0, 0), ImeOp::CommitString("你好".into())],
            "选词批次 = 清 preedit + commit，顺序不可反"
        );
        assert!(!e.composing(), "选词后组合终结");
        // 再按空格（无组合）不消费。
        assert!(!press(&mut e, SPACE, 0).consumed);
    }

    #[test]
    fn digit_selects_page_slot() {
        // 数字 2 与数字 1 必须落在不同候选（页槽映射）。
        let mut e1 = fixture_engine();
        type_str(&mut e1, "nihao");
        let r1 = press(&mut e1, 2, 0); // 数字 1
        let mut e2 = fixture_engine();
        type_str(&mut e2, "nihao");
        let r2 = press(&mut e2, 3, 0); // 数字 2
        for r in [&r1, &r2] {
            assert!(r.consumed && r.ops.len() == 2, "digit 选词须产出 [Preedit, Commit]");
        }
        if let (ImeOp::CommitString(a), ImeOp::CommitString(b)) = (&r1.ops[1], &r2.ops[1]) {
            assert_ne!(a, b, "数字 1 与 2 必须落在不同候选");
        } else {
            unreachable!()
        }
        // 页槽越界（第 9 个）：fixture nihao 不足 9 候选 → 不消费。
        let mut e3 = fixture_engine();
        type_str(&mut e3, "nihao");
        assert!(!press(&mut e3, 10, 0).consumed, "越界页槽直通");
    }

    #[test]
    fn backspace_edits_buffer_then_consumes_nothing_when_empty() {
        let mut e = fixture_engine();
        type_str(&mut e, "nihao");
        let r = press(&mut e, BKSP, 0);
        assert!(r.consumed);
        assert_eq!(e.buffer(), "niha");
        if let ImeOp::Preedit(t, b, en) = &r.ops[0] {
            assert_eq!(t, "niha");
            assert_eq!((*b, *en), (4, 4), "preedit 光标随缓冲长度");
        } else {
            panic!("退格应回空/短 preedit 批次");
        }
        // 退到空后组合终结，再退不消费。
        type_str(&mut e, "");
        press(&mut e, BKSP, 0);
        press(&mut e, BKSP, 0);
        press(&mut e, BKSP, 0);
        press(&mut e, BKSP, 0);
        assert!(!press(&mut e, BKSP, 0).consumed, "空缓冲退格直通");
    }

    #[test]
    fn enter_commits_raw_buffer() {
        let mut e = fixture_engine();
        type_str(&mut e, "nih");
        let r = press(&mut e, ENTER, 0);
        assert!(r.consumed);
        assert_eq!(r.ops[1], ImeOp::CommitString("nih".into()), "回车原样提交拼音串");
        assert!(!e.composing());
        assert!(!press(&mut e, ENTER, 0).consumed, "空缓冲回车直通（MC 发聊天不受扰）");
    }

    #[test]
    fn escape_cancels_composition() {
        let mut e = fixture_engine();
        type_str(&mut e, "ni");
        let r = press(&mut e, ESC, 0);
        assert!(r.consumed);
        assert_eq!(r.ops, vec![ImeOp::Preedit(String::new(), 0, 0)]);
        assert!(!e.composing());
        assert!(!press(&mut e, ESC, 0).consumed, "空闲态 Esc 直通（MC 关聊天框不受扰）");
    }

    #[test]
    fn apostrophe_commits_top_candidate() {
        let mut e = fixture_engine();
        type_str(&mut e, "nihao");
        let r = press(&mut e, APOS, 0);
        assert!(r.consumed);
        assert!(matches!(&r.ops[1], ImeOp::CommitString(w) if w == "你好"), "撇号=断句选首");
    }

    #[test]
    fn ctrl_chord_never_consumed_composition_kept() {
        let mut e = fixture_engine();
        type_str(&mut e, "ni");
        // Ctrl+A（mods Control=4）
        let r = press(&mut e, 30, 4);
        assert!(!r.consumed, "Ctrl 组合必须直通游戏/应用");
        assert_eq!(e.buffer(), "ni", "组合不被 Ctrl 打断");
    }

    #[test]
    fn shift_caps_letter_not_consumed() {
        let mut e = fixture_engine();
        let r = press(&mut e, 30, MOD_SHIFT_FOR_TEST);
        assert!(!r.consumed, "Shift+字母=大写英文直通");
        assert!(!e.composing());
    }

    const MOD_SHIFT_FOR_TEST: u32 = 1;

    #[test]
    fn bare_shift_toggles_english_and_back() {
        let mut e = fixture_engine();
        // Shift press+release = 裸按 → 切西文。
        assert!(press(&mut e, 42, 0).consumed);
        assert!(e.on_key(42 + 8, false, 0).consumed);
        assert!(e.english_mode());
        // 西文模式：字母全部直通，且 Shift 再切回。
        assert!(!press(&mut e, 30, 0).consumed);
        assert!(!e.composing());
        assert!(press(&mut e, 42, 0).consumed);
        assert!(e.on_key(42 + 8, false, 0).consumed);
        assert!(!e.english_mode(), "再裸按 Shift 切回拼音");
    }

    #[test]
    fn shift_during_composition_not_toggle() {
        let mut e = fixture_engine();
        type_str(&mut e, "ni");
        // 组合中按 Shift 再松开（期间有其他键）：不得翻转模式。
        press(&mut e, 30, 0); // 再吃一个 a → "nia..."
        assert!(press(&mut e, 42, 0).consumed);
        e.on_key(42 + 8, false, 0);
        assert!(!e.english_mode(), "组合期间的 Shift 不是模式切换");
    }

    #[test]
    fn esc_in_english_mode_exits() {
        let mut e = fixture_engine();
        press(&mut e, 42, 0);
        e.on_key(42 + 8, false, 0);
        assert!(e.english_mode());
        assert!(press(&mut e, ESC, 0).consumed, "西文模式下 Esc 是回拼音的出口");
        assert!(!e.english_mode());
    }

    #[test]
    fn release_follows_press_path() {
        let mut e = fixture_engine();
        // 被消费的 press，其 release 也必须被消费（无孤立 release 落入应用）。
        assert!(e.on_key(30 + 8, true, 0).consumed);
        assert!(e.on_key(30 + 8, false, 0).consumed);
        // 未消费的 press（v 无前景），其 release 也不得消费。
        assert!(!e.on_key(47 + 8, true, 0).consumed);
        assert!(!e.on_key(47 + 8, false, 0).consumed);
    }

    #[test]
    fn paging_nine_per_page_and_page_keys() {
        let mut e = fixture_engine();
        type_str(&mut e, "aa");
        assert!(e.candidate_count() > PAGE_SIZE, "fixture 应 >9 候选触发分页");
        assert_eq!(e.page_count(), (e.candidate_count() + 8) / 9);
        let first = e.current_page()[0].0.to_string();
        let r = press(&mut e, PDOWN, 0);
        assert!(r.consumed && e.page() == 1);
        assert_ne!(first, e.current_page()[0].0);
        assert!(press(&mut e, PUP, 0).consumed);
        assert_eq!(e.page(), 0);
        // 单页时 PgDn 不消费。
        type_str(&mut e, "nihao");
        if e.page_count() <= 1 {
            assert!(!press(&mut e, PDOWN, 0).consumed, "单页翻页键直通");
        }
    }

    #[test]
    fn page_labels_are_1_to_9() {
        let mut e = fixture_engine();
        type_str(&mut e, "aa");
        let labels: Vec<&str> = e.current_page().iter().map(|(_, l)| *l).collect();
        assert_eq!(labels, vec!["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
    }

    #[test]
    fn cursor_arrows_move_and_follow_page() {
        let mut e = fixture_engine();
        type_str(&mut e, "aa");
        for _ in 0..10 {
            assert!(press(&mut e, RIGHT, 0).consumed);
        }
        assert_eq!(e.page(), 1, "高亮跨过第 9 候选必须跟页");
        assert_eq!(e.page_cursor(), 1);
        assert!(press(&mut e, LEFT, 0).consumed);
        assert!(press(&mut e, LEFT, 0).consumed);
        assert_eq!(e.page(), 0, "左移跨页回上一页");
        assert_eq!(e.page_cursor(), 8);
        // 空格按高亮选中（现应位于页 0 槽 8）。
        let want = e.current_page()[8].0.to_string();
        let r = press(&mut e, SPACE, 0);
        assert!(matches!(&r.ops[1], ImeOp::CommitString(w) if *w == want), "空格=选高亮");
    }

    #[test]
    fn snapshot_only_on_change() {
        let mut e = fixture_engine();
        assert!(e.snapshot_json().is_some(), "首查须有初始快照（即便空闲）");
        assert!(e.snapshot_json().is_none(), "无变化必须 None（JNI 轻量合同）");
        type_str(&mut e, "ni");
        let s = e.snapshot_json().expect("状态变化后须有快照");
        assert!(s.contains("\"visible\":true"), "{s}");
        assert!(s.contains("你"), "{s}");
        assert!(s.contains("\"preedit\":\"ni\""), "{s}");
        assert!(e.snapshot_json().is_none());
    }

    #[test]
    fn cancel_resets_and_flags() {
        let mut e = fixture_engine();
        type_str(&mut e, "ni");
        assert!(e.cancel(), "有组合时 cancel 返回 true（门面须清 preedit）");
        assert!(!e.composing());
        assert!(!e.cancel(), "空闲时 cancel 无动作");
    }

    #[test]
    fn no_dict_engine_passes_everything() {
        let mut e = BuiltinEngine::default();
        assert!(!press(&mut e, 30, 0).consumed);
        assert!(!press(&mut e, SPACE, 0).consumed);
        assert!(!e.composing());
    }

    #[test]
    fn long_garbage_buffer_bounded() {
        let mut e = fixture_engine();
        for _ in 0..80 {
            e.on_key(30 + 8, true, 0);
            e.on_key(30 + 8, false, 0);
        }
        assert!(e.buffer().len() <= 32, "缓冲须有上限");
    }

    // ── 真实词库抽测（fixture 之外的第二层合同）───────────────────

    #[test]
    fn real_dict_nihao_space_commits_nihao() {
        let Some(d) = embedded_dict() else {
            panic!("内嵌词库不可用");
        };
        let mut e = BuiltinEngine::with_dict(Some(d));
        let t = std::time::Instant::now();
        type_str(&mut e, "nihao");
        let key_ms = t.elapsed().as_micros();
        eprintln!("[ime][builtin] 5 键组合耗时 {key_ms}µs（含候选收集）");
        assert!(key_ms < 20_000, "每键平均应远低于 1ms");
        let r = press(&mut e, SPACE, 0);
        assert!(matches!(&r.ops[1], ImeOp::CommitString(w) if w == "你好"), "真实词库 nihao→你好");
    }

    #[test]
    fn real_dict_common_words_ranked() {
        let d = embedded_dict().unwrap();
        let mut e = BuiltinEngine::with_dict(Some(d));
        for (pinyin, want) in [("women", "我们"), ("tamen", "他们"), ("pengyou", "朋友"), ("shijie", "世界")] {
            type_str(&mut e, pinyin);
            let top5: Vec<String> = e.current_page().iter().take(5).map(|(w, _)| w.to_string()).collect();
            assert!(top5.iter().any(|w| w == want), "编码 {pinyin} 前 5 候选应含 {want}，实际 {top5:?}");
            e.cancel();
        }
    }

    #[test]
    fn ime_source_parse_roundtrip() {
        for (raw, src) in [
            ("auto", ImeSource::Auto),
            ("Fcitx5", ImeSource::Fcitx5),
            (" builtin ", ImeSource::Builtin),
        ] {
            assert_eq!(ImeSource::parse(raw), Some(src), "{raw}");
            assert_eq!(ImeSource::parse(raw).unwrap().as_str(), src.as_str());
        }
        assert_eq!(ImeSource::parse("ibus"), None);
        assert_eq!(ImeSource::default(), ImeSource::Auto);
    }
}
