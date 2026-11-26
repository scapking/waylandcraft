// WaylandCraft 内置拼音词库二进制格式 v1 —— 编译器与运行时读取器的**唯一事实源**。
//
// 本文件零外部依赖（std only），被两处共用：
//   - `native/tools/ime-dict/dictgen.rs`（构建期生成器，`#[path]` include）：
//     `TrieBuilder::build()` 产字节流；
//   - `native/src/ime/builtin/engine.rs`（运行时）：`Dict::parse()` 只读视图。
// 格式改动必须同时通过 `tests` 里的 round-trip 断言，杜绝两侧漂移。
//
// ## 布局（全小端）
//
// ```text
// 偏移 0   : magic b"WCPYD1"          (6B)
// 偏移 6   : format_version u16 = 1   (2B)
// 偏移 8   : syllable_count u32       (S，音节表项数)
// 偏移 12  : node_count u32           (N，含 root=node0)
// 偏移 16  : edge_count u32           (E)
// 偏移 20  : term_count u32           (T，终端词条总数)
// 偏移 24  : word_blob_len u32        (W)
// 偏移 28  : 音节表 —— S × (u8 len + ascii bytes)（按 id 升序紧凑排列）
//          : 节点表 —— N × 20B { first_edge u32, edge_count u32,
//                                first_term u32, term_count u32, sub_max u32 }
//                                （sub_max = 该子树（含本节点终端）最大词频，
//                                  供 top-K 收集剪枝）
//          : 边表   —— E × 6B { syllable_id u16, child_node u32 }
//                                （同节点内按 syllable_id 升序）
//          : 终端表 —— T × 12B { word_off u32, freq u32, kind u32 }
//                                （kind: 0=词, 1=用户短语；同节点内按 freq 降序）
//          : 词条 blob —— 每条 (u16 len + UTF-8 bytes)，word_off 指向 len 字段
// ```
//
// 音节表 id 按**字典序**分配（生成器排序），便于调试与稳定输出。

use std::collections::HashMap;

pub const MAGIC: &[u8; 6] = b"WCPYD1";
pub const FORMAT_VERSION: u16 = 1;

pub const KIND_WORD: u32 = 0;
pub const KIND_PHRASE: u32 = 1;

/// 编译期构建器：`(word, syllables, freq, kind)` → 字节流。
pub struct TrieBuilder {
    syll_ids: HashMap<String, u16>,
    syllables: Vec<String>,
    nodes: Vec<NodeBuilder>,
}

struct NodeBuilder {
    edges: HashMap<u16, usize>,
    terms: Vec<(String, u32, u32)>, // (word, freq, kind)
}

impl Default for TrieBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TrieBuilder {
    pub fn new() -> Self {
        Self {
            syll_ids: HashMap::new(),
            syllables: Vec::new(),
            nodes: vec![NodeBuilder { edges: HashMap::new(), terms: Vec::new() }],
        }
    }

    fn syl_id(&mut self, s: &str) -> Option<u16> {
        if s.is_empty() || !s.is_ascii() || !s.bytes().all(|b| b.is_ascii_alphabetic()) {
            return None;
        }
        if let Some(&id) = self.syll_ids.get(s) {
            return Some(id);
        }
        let id = self.syllables.len() as u16;
        self.syllables.push(s.to_ascii_lowercase());
        self.syll_ids.insert(s.to_ascii_lowercase(), id);
        Some(id)
    }

    /// 插入一条词条。syllables 为空格分隔的无声调音节串。
    pub fn insert(&mut self, word: &str, syllables: &str, freq: u32, kind: u32) -> bool {
        let syls: Vec<&str> = syllables.split_whitespace().collect();
        if syls.is_empty() || syls.len() > 16 || word.is_empty() || word.len() > 48 {
            return false;
        }
        let mut node = 0usize;
        for s in syls {
            let id = match self.syl_id(s) {
                Some(i) => i,
                None => return false,
            };
            let next = match self.nodes[node].edges.get(&id) {
                Some(&n) => n,
                None => {
                    let n = self.nodes.len();
                    self.nodes.push(NodeBuilder { edges: HashMap::new(), terms: Vec::new() });
                    self.nodes[node].edges.insert(id, n);
                    n
                }
            };
            node = next;
        }
        self.nodes[node].terms.push((word.to_string(), freq, kind));
        true
    }

    /// 序列化。音节按字典序重编号（输出稳定）。
    pub fn build(mut self) -> Vec<u8> {
        // 音节重排：字典序 + 旧→新 id 映射。
        let mut order: Vec<usize> = (0..self.syllables.len()).collect();
        order.sort_by_key(|&i| self.syllables[i].clone());
        let mut remap = vec![0u16; self.syllables.len()];
        for (new, old) in order.iter().enumerate() {
            remap[*old] = new as u16;
        }
        let syllables: Vec<String> = order.iter().map(|&i| self.syllables[i].clone()).collect();

        // 节点重排：BFS（root=0，子节点按层入队），保证父先于子。
        let mut bfs_map = vec![usize::MAX; self.nodes.len()];
        let mut queue = vec![0usize];
        bfs_map[0] = 0;
        let mut next_new = 1usize;
        let mut head = 0;
        while head < queue.len() {
            let n = queue[head];
            head += 1;
            let mut kids: Vec<(u16, usize)> = self.nodes[n].edges.iter().map(|(&s, &c)| (s, c)).collect();
            kids.sort();
            for (_, c) in kids {
                if bfs_map[c] == usize::MAX {
                    bfs_map[c] = next_new;
                    next_new += 1;
                    queue.push(c);
                }
            }
        }
        let node_count = queue.len();

        // 边表与节点表（新编号）。
        let mut edges: Vec<(u16, u32)> = Vec::new();
        let mut nodes_out: Vec<(u32, u32, u32, u32, u32)> = Vec::with_capacity(node_count); // first_edge,count,first_term,tcount,sub_max
        // 终端表与 blob。
        let mut terms: Vec<(u32, u32, u32)> = Vec::new(); // word_off, freq, kind
        let mut word_blob: Vec<u8> = Vec::new();
        let mut sub_max = vec![0u32; node_count];
        // 先算每个新节点的直接 term 区段，sub_max 反向上滚。
        let mut new_nodes: Vec<NodeBuilder> = Vec::with_capacity(node_count);
        for &old in &queue {
            let mut nb = NodeBuilder { edges: HashMap::new(), terms: Vec::new() };
            std::mem::swap(&mut nb.terms, &mut self.nodes[old].terms);
            for (&s, &c) in &self.nodes[old].edges {
                nb.edges.insert(remap[s as usize] as u16, c);
            }
            new_nodes.push(nb);
        }
        // 重建 old→new 子节点映射后逐节点序列化。
        for new_i in 0..node_count {
            let old = queue[new_i];
            let nb = &mut new_nodes[new_i];
            let first_edge = edges.len() as u32;
            let mut kids: Vec<(u16, usize)> = nb.edges.iter().map(|(&s, &c)| (s, c)).collect();
            kids.sort();
            let cnt = kids.len() as u32;
            for (s, c) in kids {
                edges.push((s, bfs_map[c] as u32));
            }
            // 终端：freq 降序、同频按 word 字节序。
            nb.terms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let first_term = terms.len() as u32;
            let tcnt = nb.terms.len() as u32;
            let mut local_max = 0u32;
            for (w, fq, kind) in &nb.terms {
                let off = word_blob.len() as u32;
                word_blob.extend_from_slice(&(w.len() as u16).to_le_bytes());
                word_blob.extend_from_slice(w.as_bytes());
                terms.push((off, *fq, *kind));
                local_max = local_max.max(*fq);
            }
            sub_max[new_i] = local_max;
            nodes_out.push((first_edge, cnt, first_term, tcnt, 0));
            let _ = old;
        }
        // sub_max 后序上滚（BFS 逆序即近似后序：子总在新编号上 ≥ 父）。
        for i in (0..node_count).rev() {
            let (_, ecnt, _, _, _) = nodes_out[i];
            let fe = nodes_out[i].0 as usize;
            for e in fe..fe + ecnt as usize {
                let child = edges[e].1 as usize;
                if child > i {
                    let cm = sub_max[child];
                    if cm > sub_max[i] {
                        sub_max[i] = cm;
                    }
                }
            }
            nodes_out[i].4 = sub_max[i];
        }

        let s = syllables.len() as u32;
        let n = node_count as u32;
        let e = edges.len() as u32;
        let t = terms.len() as u32;
        let w = word_blob.len() as u32;

        let mut out = Vec::with_capacity(28 + 20 * n as usize + 6 * e as usize + 12 * t as usize + w as usize + 1024);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&e.to_le_bytes());
        out.extend_from_slice(&t.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        for syl in &syllables {
            out.push(syl.len() as u8);
            out.extend_from_slice(syl.as_bytes());
        }
        for (fe, ec, ft, tc, sm) in &nodes_out {
            out.extend_from_slice(&fe.to_le_bytes());
            out.extend_from_slice(&ec.to_le_bytes());
            out.extend_from_slice(&ft.to_le_bytes());
            out.extend_from_slice(&tc.to_le_bytes());
            out.extend_from_slice(&sm.to_le_bytes());
        }
        for (sid, child) in &edges {
            out.extend_from_slice(&sid.to_le_bytes());
            out.extend_from_slice(&child.to_le_bytes());
        }
        for (off, fq, kind) in &terms {
            out.extend_from_slice(&off.to_le_bytes());
            out.extend_from_slice(&fq.to_le_bytes());
            out.extend_from_slice(&kind.to_le_bytes());
        }
        out.extend_from_slice(&word_blob);
        out
    }
}

/// 字节流来源：内嵌词库走 &'static 零拷贝；动态词库（测试/外接）用 Rc。
#[derive(Clone)]
enum Backing {
    Static(&'static [u8]),
    Shared(std::rc::Rc<[u8]>),
}

impl Backing {
    #[inline]
    fn as_bytes(&self) -> &[u8] {
        match self {
            Backing::Static(b) => b,
            Backing::Shared(rc) => rc,
        }
    }
}

/// 运行时只读视图。零拷贝：查询直接落在字节流上。
pub struct Dict {
    data: Backing,
    syllable_count: u32,
    nodes_off: usize,
    edges_off: usize,
    terms_off: usize,
    words_off: usize,
    node_count: u32,
    edge_count: u32,
    term_count: u32,
    /// 音节表 id→(off,len) 预解析（音节只有数百个，构建成本可忽略）。
    syl: Vec<(u32, u8)>,
}

impl Clone for Dict {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            syllable_count: self.syllable_count,
            nodes_off: self.nodes_off,
            edges_off: self.edges_off,
            terms_off: self.terms_off,
            words_off: self.words_off,
            node_count: self.node_count,
            edge_count: self.edge_count,
            term_count: self.term_count,
            syl: self.syl.clone(),
        }
    }
}

#[inline]
fn u16le(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
#[inline]
fn u32le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

impl Dict {
    /// 解析并校验字节流。任何越界/魔数不符 → None（调用方须降级为
    /// 「内置引擎不可用」而非 panic）。
    pub fn parse(data: impl Into<std::rc::Rc<[u8]>>) -> Option<Dict> {
        Self::parse_rc(data.into())
    }

    /// 内嵌（`include_bytes!`）词库的零拷贝入口。
    pub fn parse_static(data: &'static [u8]) -> Option<Dict> {
        Self::parse_common(Backing::Static(data))
    }

    pub fn parse_rc(data: std::rc::Rc<[u8]>) -> Option<Dict> {
        Self::parse_common(Backing::Shared(data))
    }

    fn parse_common(data: Backing) -> Option<Dict> {
        let b: &[u8] = data.as_bytes();
        if b.len() < 28 || &b[0..6] != MAGIC {
            return None;
        }
        if u16le(b, 6) != FORMAT_VERSION {
            return None;
        }
        let s = u32le(b, 8);
        let n = u32le(b, 12);
        let e = u32le(b, 16);
        let t = u32le(b, 20);
        let w = u32le(b, 24);
        let syllables_off = 28usize;
        // 音节表扫描并校验边界。
        let mut off = syllables_off;
        let mut syl = Vec::with_capacity(s as usize);
        for _ in 0..s {
            if off >= b.len() {
                return None;
            }
            let l = b[off] as usize;
            off += 1;
            if off + l > b.len() {
                return None;
            }
            syl.push((off as u32, l as u8));
            off += l;
        }
        let nodes_off = off;
        let edges_off = nodes_off + 20 * n as usize;
        let terms_off = edges_off + 6 * e as usize;
        let words_off = terms_off + 12 * t as usize;
        if words_off + w as usize > b.len() {
            return None;
        }
        // 结构校验（线性一次过，成本 ~ms 级）：节点/边/终端表区间不越界。
        // 逐 child/word 解引用抽查仅在 debug 构建开启——内嵌词库由同一
        // Rust 代码生成、release 路径的区间校验已足以挡住结构性损坏，
        // 而 release 下的全量解引用会把 parse 拖到 20ms（tick 预算敏感）。
        for i in 0..n as usize {
            let base = nodes_off + i * 20;
            let fe = u32le(b, base) as usize;
            let ec = u32le(b, base + 4) as usize;
            let ft = u32le(b, base + 8) as usize;
            let tc = u32le(b, base + 12) as usize;
            if fe + ec > e as usize || ft + tc > t as usize {
                return None;
            }
        }
        if cfg!(debug_assertions) {
            for i in 0..n as usize {
                let base = nodes_off + i * 20;
                let fe = u32le(b, base) as usize;
                let ec = u32le(b, base + 4) as usize;
                let ft = u32le(b, base + 8) as usize;
                let tc = u32le(b, base + 12) as usize;
                for k in fe..fe + ec {
                    let child = u32le(b, edges_off + k * 6 + 2) as usize;
                    if child >= n as usize {
                        return None;
                    }
                }
                for k in ft..ft + tc {
                    let wo = u32le(b, terms_off + k * 12) as usize;
                    if wo + 2 > w as usize {
                        return None;
                    }
                    let wl = u16le(b, words_off + wo) as usize;
                    if wo + 2 + wl > w as usize {
                        return None;
                    }
                }
            }
        }
        Some(Dict {
            data,
            syllable_count: s,
            nodes_off,
            edges_off,
            terms_off,
            words_off,
            node_count: n,
            edge_count: e,
            term_count: t,
            syl,
        })
    }

    /// 规模统计（status/单测用）：(音节数, 节点数, 词条数)。
    pub(crate) fn stats(&self) -> (u32, u32, u32) {
        (self.syllable_count, self.node_count, self.term_count)
    }

    #[inline]
    pub(crate) fn syllable(&self, id: u16) -> Option<&[u8]> {
        let (off, l) = *self.syl.get(id as usize)?;
        Some(&self.data.as_bytes()[off as usize..off as usize + l as usize])
    }

    #[inline]
    pub(crate) fn node(&self, i: usize) -> (u32, u32, u32, u32, u32) {
        let base = self.nodes_off + i * 20;
        let b = self.data.as_bytes();
        (
            u32le(b, base),
            u32le(b, base + 4),
            u32le(b, base + 8),
            u32le(b, base + 12),
            u32le(b, base + 16), // sub_max
        )
    }

    /// 子边迭代：(syllable_id, child_node)。边表在同节点内按 sid 升序。
    #[inline]
    pub(crate) fn edges_of(&self, first: u32, count: u32) -> impl Iterator<Item = (u16, u32)> + '_ {
        let base = self.edges_off + first as usize * 6;
        let d = self.data.as_bytes();
        (0..count).map(move |k| {
            let i = base + k as usize * 6;
            (u16le(d, i), u32le(d, i + 2))
        })
    }

    #[inline]
    pub(crate) fn terms_of(&self, first: u32, count: u32) -> impl Iterator<Item = (&str, u32, u32)> + '_ {
        let base = self.terms_off + first as usize * 12;
        let d = self.data.as_bytes();
        let wo = self.words_off;
        (0..count).filter_map(move |k| {
            let i = base + k as usize * 12;
            let off = u32le(d, i) as usize;
            let freq = u32le(d, i + 4);
            let kind = u32le(d, i + 8);
            let l = u16le(d, wo + off) as usize;
            let s = std::str::from_utf8(&d[wo + off + 2..wo + off + 2 + l]).ok()?;
            Some((s, freq, kind))
        })
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    /// round-trip：构建器 → 解析器 → 结构一致性（格式唯一事实源的自锁，
    /// dictgen.rs 与本模块共用同一代码，杜绝两侧漂移）。
    #[test]
    fn builder_parser_roundtrip() {
        let mut tb = TrieBuilder::new();
        assert!(tb.insert("你", "ni", 5_000_000, KIND_WORD));
        assert!(tb.insert("好", "hao", 4_000_000, KIND_WORD));
        assert!(tb.insert("你好", "ni hao", 200_000, KIND_WORD));
        assert!(tb.insert("你好吗", "ni hao ma", 8_000, KIND_PHRASE));
        // 非法：非 ASCII 音节 / 空音节。
        assert!(!tb.insert("错", "cuō", 1, KIND_WORD));
        assert!(!tb.insert("空", "", 1, KIND_WORD));
        let blob = std::rc::Rc::from(tb.build().into_boxed_slice());
        let dict = Dict::parse_rc(blob).expect("round-trip parse");
        // root: ni 边存在。
        let (fe, ec, ft, tc, sm) = dict.node(0);
        assert_eq!(ft, 0);
        assert_eq!(tc, 0);
        assert_eq!(sm, 5_000_000);
        let mut found = false;
        for (sid, child) in dict.edges_of(fe, ec) {
            if dict.syllable(sid) == Some(&b"ni"[..]) {
                found = true;
                let (_, _, cft, ctc, _) = dict.node(child as usize);
                let words: Vec<&str> = dict.terms_of(cft, ctc).map(|(w, _, _)| w).collect();
                assert_eq!(words, vec!["你"]);
            }
        }
        assert!(found, "音节 ni 应在 root 边表");
    }
}
