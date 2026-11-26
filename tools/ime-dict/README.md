# 内置拼音词库生成管线

产物：`native/assets/ime_builtin_pinyin.bin`（提交入库，运行时 `include_bytes!` 内嵌）。
格式唯一事实源：`native/src/ime/builtin/dict.rs`（编译器与读取器共用同一份代码，
dictgen 通过 `#[path]` include，杜绝两侧漂移）。

## 数据来源与许可

| 来源 | 版本 | 许可 | 用途 |
|---|---|---|---|
| [jieba](https://github.com/fxsjy/jieba) `jieba/dict.txt` | master (2026-11) | MIT | 词表 + 词频（现代汉语语料计数） |
| [pypinyin](https://github.com/mozillazg/pypinyin) `pypinyin/pinyin_dict.json` | 0.55.0 (PyPI) | MIT | 单字读音（多读音全保留） |
| [pypinyin](https://github.com/mozillazg/pypinyin) `pypinyin/phrases_dict.json` | 0.55.0 (PyPI) | MIT | 词语逐字读音（多音字消歧） |

三个来源均为 MIT 许可的开源数据；产物为按词频聚合后的编译格式，来源出处
在生成脚本头部与本文件留痕。

## 复现步骤

```sh
# 1. 拉取源数据（需代理时前缀 proxychains4）
curl -sL -o dict.txt https://raw.githubusercontent.com/fxsjy/jieba/master/jieba/dict.txt
pip download pypinyin==0.55.0 --no-deps -d pypkg && unzip -q pypkg/pypinyin-0.55.0-py2.py3-none-any.whl -d pypkg/x

# 2. 合并规范化（word<TAB>音节序列<TAB>freq；无声调 ASCII，ü→v）
python3 -I tools/ime-dict/prepare_sources.py dict.txt \
    pypkg/x/pypinyin/pinyin_dict.json pypkg/x/pypinyin/phrases_dict.json \
    words.tsv --top 70000

# 3. 编译为 trie 二进制（含自检：ni hao 首候选必须是「你好」）
rustc --edition 2021 -O native/tools/ime-dict/dictgen.rs -o /tmp/dictgen
/tmp/dictgen words.tsv native/assets/ime_builtin_pinyin.bin
```

当前产物规模：113723 词条 / 420 音节 / 71657 节点 / 4.14MB。
