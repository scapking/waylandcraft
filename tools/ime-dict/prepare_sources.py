#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""WaylandCraft 内置拼音引擎词库预处理。

数据来源（许可证见仓库 tools/ime-dict/README.md 与本脚本头部注释）：
  1. jieba dict.txt            —— MIT (https://github.com/fxsjy/jieba)
     提供：词表 + 词频（现代汉语语料计数）。
  2. pypinyin pinyin_dict.json —— MIT (https://github.com/mozillazg/pypinyin)
     提供：单字 → 读音（多读音逗号分隔，全部保留）。
  3. pypinyin phrases_dict.json—— MIT (https://github.com/mozillazg/pypinyin)
     提供：词语 → 逐字读音（多音字消歧优先于按字组合）。

输出：规范化 TSV（word \t 音节1 音节2 ... \t freq），供 dictgen.rs 编译为
内置二进制词典。音节为无声调 ASCII（ü→v）。

用法：
  python3 prepare_sources.py <jieba_dict.txt> <pinyin_dict.json> <phrases_dict.json> <out.tsv> [--top 70000]
"""
import json
import sys
import unicodedata

CJK = (
    range(0x3400, 0x4DC0),   # 扩展 A
    range(0x4E00, 0xA000),   # 基本区
    range(0xF900, 0xFAFF),   # 兼容表意
)


def is_cjk(ch: str) -> bool:
    o = ord(ch)
    return any(o in r for r in CJK)


def strip_tone(syl: str) -> str:
    """去声调：ü 族 → v，其余 NFD 分解去 combining。"""
    out = []
    for ch in syl:
        if ch in 'üǖǘǚǜ':
            out.append('v')
        else:
            d = unicodedata.normalize('NFD', ch)
            base = ''.join(c for c in d if not unicodedata.combining(c))
            out.append(base)
    return ''.join(out).lower()


def valid_syl(s: str) -> bool:
    return bool(s) and s.isascii() and s.isalpha()


def main(argv):
    jieba_path, pinyin_json, phrases_json, out_path = argv[0:4]
    top = 70000
    if '--top' in argv:
        top = int(argv[argv.index('--top') + 1])

    # ── 1. 单字读音表（首读音 + 全部读音） ────────────────────────────
    char_all = {}   # ch -> [syl, ...] 无声调去重
    with open(pinyin_json, encoding='utf-8') as f:
        pdict = json.load(f)
    # 注意：pinyin_dict.json 的键是十进制码点字符串（"12295"），非汉字本身。
    for key, val in pdict.items():
        try:
            ch = chr(int(key))
        except ValueError:
            continue
        if len(ch) != 1 or not is_cjk(ch):
            continue
        syls = []
        for s in str(val).split(','):
            t = strip_tone(s.strip())
            if valid_syl(t) and t not in syls:
                syls.append(t)
        if syls:
            char_all[ch] = syls

    # ── 2. 词语精确读音 ──────────────────────────────────────────────
    phrase_syls = {}  # word -> "a b c"
    with open(phrases_json, encoding='utf-8') as f:
        pdict = json.load(f)
    for word, groups in pdict.items():
        if not all(is_cjk(c) for c in word) or not (1 <= len(word) <= 8):
            continue
        syls = []
        ok = True
        for g in groups:
            if isinstance(g, list):
                g = g[0] if g else ''
            t = strip_tone(str(g))
            if not valid_syl(t):
                ok = False
                break
            syls.append(t)
        if ok and len(syls) == len(word):
            phrase_syls[word] = ' '.join(syls)

    # ── 3. jieba 词频表 ──────────────────────────────────────────────
    words = {}  # word -> freq
    with open(jieba_path, encoding='utf-8') as f:
        for line in f:
            parts = line.split()
            if len(parts) < 2:
                continue
            w, fs = parts[0], parts[1]
            if not (1 <= len(w) <= 8) or not all(is_cjk(c) for c in w):
                continue
            try:
                fq = int(fs)
            except ValueError:
                continue
            if fq > words.get(w, 0):
                words[w] = fq

    freqs = {}
    # 单字：全部读音各自成条（多音字），沿用 jieba 频率（缺失时保底小频率，
    # 保证候选可见——pypinyin 覆盖的生僻字给低频率）。
    for ch, syls in char_all.items():
        base = words.get(ch, 50)
        for s in syls:
            key = (ch, s)
            freqs[key] = max(freqs.get(key, 0), base)

    ranked = sorted(words.items(), key=lambda kv: -kv[1])[:top]
    for w, fq in ranked:
        exact = phrase_syls.get(w)
        if exact is not None:
            freqs[(w, exact)] = max(freqs.get((w, exact), 0), fq)
        else:
            per = [char_all.get(c) for c in w]
            if any(p is None for p in per):
                continue
            # 组合式：首读音必入；长度 ≤2 时追加每字次读音（常见词兜底），
            # 长度 ≥3 只用首读音（组合爆炸无益，phrases_dict 已覆盖常见词）。
            combos = [' '.join(p[0] for p in per)]
            if len(w) == 2:
                if len(per[0]) > 1:
                    combos.append(' '.join([per[0][1], per[1][0]]))
                if len(per[1]) > 1:
                    combos.append(' '.join([per[0][0], per[1][1]]))
            for c in combos[:3]:
                freqs[(w, c)] = max(freqs.get((w, c), 0), fq)

    # ── 4. phrases_dict 独有词补充（jieba 未收录） ───────────────────
    extra = 0
    for w, key in phrase_syls.items():
        if w in words:
            continue
        if len(key.split()) > 5:
            continue
        freqs[(w, key)] = 300
        extra += 1

    n = 0
    with open(out_path, 'w', encoding='utf-8') as f:
        for (w, key), fq in freqs.items():
            f.write(f"{w}\t{key}\t{fq}\n")
            n += 1
    print(f"chars={len(char_all)} phrases={len(phrase_syls)} jieba_cjk_words={len(words)} "
          f"extra_phrases={extra} entries={n}", file=sys.stderr)


if __name__ == '__main__':
    main(sys.argv[1:])
