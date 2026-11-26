//! WaylandCraft 内置拼音词库编译器。
//!
//! 输入：规范化 TSV（每行 `word\t音节1 音节2 …\tfreq`，由
//! tools/ime-dict/prepare_sources.py 从 MIT 许可的 jieba/pypinyin 数据生成）。
//! 输出：音节 trie 二进制（格式唯一事实源 = native/src/ime/builtin/dict.rs，
//! 本工具 `#[path]` include 同一份代码，杜绝编译器/运行时两侧漂移）。
//!
//! 用法：
//!   rustc --edition 2021 -O tools/ime-dict/dictgen.rs -o /tmp/dictgen
//!   /tmp/dictgen <in.tsv> <out.bin>
//! 退出前自检：重新 parse 产物并断言 "ni hao" 首候选为「你好」（防格式退化）。

#[path = "../../src/ime/builtin/dict.rs"]
#[allow(dead_code, clippy::all)]
mod dict;

use dict::{TrieBuilder, KIND_PHRASE, KIND_WORD, Dict};
use std::rc::Rc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: dictgen <in.tsv> <out.bin>");
        std::process::exit(2);
    }
    let text = std::fs::read_to_string(&args[1]).expect("read tsv");
    let mut tb = TrieBuilder::new();
    let (mut n_word, mut n_phrase, mut n_skip) = (0u32, 0u32, 0u32);
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(w), Some(key), Some(f)) = (it.next(), it.next(), it.next()) else {
            n_skip += 1;
            continue;
        };
        let Ok(freq) = f.parse::<u32>() else {
            n_skip += 1;
            continue;
        };
        let syl_count = key.split_whitespace().count();
        let kind = if syl_count >= 4 { KIND_PHRASE } else { KIND_WORD };
        if syl_count >= 4 {
            n_phrase += 1;
        } else {
            n_word += 1;
        }
        if !tb.insert(w, key, freq, kind) {
            n_skip += 1;
        }
    }
    let blob = tb.build();
    std::fs::write(&args[2], &blob).expect("write bin");

    // ── 自检 ────────────────────────────────────────────────────────────
    let d = Dict::parse_rc(Rc::from(blob.into_boxed_slice())).expect("self-parse failed");
    let (s, n, t) = d.stats();
    eprintln!(
        "entries={n_word}+{n_phrase} skip={n_skip} syllables={s} nodes={n} terms={t} bytes={}",
        std::fs::metadata(&args[2]).unwrap().len()
    );
    // 词库语义抽测：沿音节路径精确走 trie，确认高频常用词在场
    // （nihao→你好 是本仓库 e2e 单测的合同，缺失即构建失败）。
    fn path_node(d: &Dict, key: &str) -> Option<u32> {
        let mut node = 0u32;
        for syl in key.split_whitespace() {
            let (fe, ec, _ft, _tc, _sm) = d.node(node as usize);
            let mut hit = None;
            for (sid, child) in d.edges_of(fe, ec) {
                if d.syllable(sid) == Some(syl.as_bytes()) {
                    hit = Some(child);
                    break;
                }
            }
            node = hit?;
        }
        Some(node)
    }
    fn top_term_at(d: &Dict, node: u32) -> Option<(String, u32)> {
        let (_fe, _ec, ft, tc, _sm) = d.node(node as usize);
        d.terms_of(ft, tc).next().map(|(w, f, _k)| (w.to_string(), f))
    }
    let must = [("ni hao", "你好"), ("wo", "我"), ("shi", "是"), ("de", "的")];
    for (key, want) in must {
        let Some(node) = path_node(&d, key) else {
            eprintln!("FAIL: 路径 {key:?} 不存在（词库生成异常）");
            std::process::exit(1);
        };
        let Some((top, _)) = top_term_at(&d, node) else {
            eprintln!("FAIL: {key:?} 节点无终端词条");
            std::process::exit(1);
        };
        if key == "ni hao" && top != want {
            eprintln!("FAIL: nihao 首候选应为「你好」，实际「{top}」（排序合同破坏）");
            std::process::exit(1);
        }
        eprintln!("probe {key:?} top=「{top}」 ok");
    }
}
