# 赛题四 · 全文索引调优：瓶颈定位与优化方案

> ## ⚠️ 本文已被实测推翻，只作为过程记录保留
>
> 后续我搭起了 Linux 编译环境、编出了 seekdb、跑通了官方基准、并用 perf 采了三个阶段的样本。
> **结论：本文 §2 列出的五个"瓶颈"没有一个是真实热点。** 按它们改出来的补丁经 A/B 实测收益为零，已回滚。
>
> - 实测结果与真实热点：见 **[`RESULTS.md`](RESULTS.md)**
> - 原始数据：`measured/ab/`、`measured/profiles/`
>
> 具体错在哪（`RESULTS.md` §4 有完整对照）：
> 本文全在"分词器内部"找问题，但 perf 显示分词只占 `tokenize` 指标的约 35%
> （其余是 SQL 往返）、占 `build` 指标的个位数百分比。优化一个只占 5% 的部分，
> 整体自然看不出变化。**`perf` 应该跑在改代码之前，本文的顺序反了。**
>
> 另外本文开头那段"没有 Docker、评测脚本看不到"的背景也已作废 ——
> 评测脚本就在 `oceanbase/seekdb` 的 `2026-competition` 分支上，公开可访问。

---

> **原始状态声明（已过时，保留以便对照）**
>
> 本报告的每一条结论都来自**源码静态阅读**，**没有一行经过编译，也没有任何一个数字经过实测**。
>
> 原因：本机是 Windows 11，没有 Docker，`wsl.exe` 被安全策略列入程序黑名单；而 seekdb 是
> OceanBase 系的 C++ 内核，只能在 Linux 上构建。更关键的是，本题的评测脚本
> (`tools/benchmark/fts_large_bench*`、`.github/workflows/vldb.yml`) **不在**
> `oceanbase/seekdb` 主干里，而在赛事的私有 fork 中 —— 我看不到，也跑不了。
>
> 所以这份文档的定位是**待验证的优化方案**，不是成绩单。任何"预计提升 X%"的写法都是猜测，
> 我一律不写。

---

## 1. 为什么先把火力压在分词热路径

评分是三类等权：`综合提升率 =（构建 + 分词 + 查询）÷ 3`。

分词器是**唯一同时被两类基准调用**的组件：

```
分词基准  tokenize_ik_avg_ms / tokenize_beng_avg_ms   ← 直接调 TOKENIZE
索引构建  build_ik_*_sec / build_beng_en_sec          ← 建索引时对每篇文档全文分词
```

也就是说，优化分词热路径能同时拉动 **9 项指标里的 6 项、3 类里的 2 类**。同样的工作量
只改查询路径，只能拿到 4 项。这是排序的依据，不是偏好。

查询路径（`query_cn / beng / mixed / limit`）这一轮不覆盖 —— 它要看倒排迭代器与结果归并，
是另一套瓶颈，硬塞进来只会两头都做不深。

---

## 2. 五个瓶颈

按预期收益从高到低。每条都给到可核对的 `文件:行`。

### B1 · 每处理一个字符，付两次跨层调用

**位置**：`src/storage/fts/ik/ob_ik_processor.cpp:102-115`（`prepare_next_char`），
由 `step_next()`（同文件 `:117-137`）在**每次游标前进时**调用。

```cpp
int TokenizeContext::prepare_next_char()
{
  ObCharset::first_valid_char(coll_type_, fulltext_ + cursor_, fulltext_len_ - cursor_, next_char_len_);
  ObFTCharUtil::classify_first_char(coll_type_, fulltext_ + cursor_, next_char_len_, next_char_type_);
}
```

**机制**：

- `first_valid_char` 是 charset 层函数指针调用，内部要逐字节校验合法性；
- `classify_first_char`（`ob_ik_char_util.h:473`）先 `ObCharset::charset_type_by_coll` 查一次，
  再 `switch` 分派到 `do_classify<CS_TYPE>`，后者解码码点再做区间比较。

对一个纯 ASCII 的英文文档，每个字节都要走完这整条链：**一次函数指针调用 + 一次表查 +
一次 switch + 一次解码 + 若干比较**，而正确答案恒为 `char_len = 1`、类型来自 128 项定值表。
BENG 基准（英文标题+正文）整篇都吃这个成本，而 `build_beng_en_sec` 又要对全文分词一次。

**改法**：在 `prepare_next_char` 开头加 ASCII 快路径。

```cpp
if (OB_LIKELY(static_cast<uint8_t>(fulltext_[cursor_]) < 0x80)) {
  next_char_len_ = 1;
  next_char_type_ = ASCII_TYPE_TABLE[static_cast<uint8_t>(fulltext_[cursor_])];
} else {
  // 原逻辑不变
}
```

`ASCII_TYPE_TABLE` 用 `constexpr` 在编译期生成，取值必须与 `do_classify<CHARSET_UTF8MB4>`
在 `[0x00,0x7F]` 上逐值一致 —— 这一点必须用对拍单测钉死（见 §4.3），不能靠眼看。

**风险**：低。ASCII 区间语义简单、边界封闭，且有对拍兜底。
**收益来源**：分词基准的两项 + 构建基准的 `build_beng_en_sec`。

---

### B2 · 每个 token、每次前缀命中都做一次堆分配

**位置**：

- `src/storage/fts/ik/ob_ik_cjk_processor.cpp:70,77` —— `hits_.push_back(hit)`
- `src/storage/fts/ik/ob_ik_processor.cpp:168` —— `token_list_.add_token(token)`
- `src/oblib/lib/list/ob_list.h:279,301,373` —— `ObList::push_back` 里
  `allocator_.alloc(sizeof(node_t))`

**机制**：`hits_` 是 `ObList<ObDATrieHit, ObIAllocator>`，不是数组。每来一个可能成词的
前缀就往链表里 `push_back` 一个节点（**一次分配**），失配时 `erase(hit)`（**一次释放**）。
中文长句里这种"试探-回退"极其频繁。同理，`token_list_` / `result_list_` 每产出一个 token
就分配一个节点 —— 一篇 N 个 token 的文档，光骨架就是 N 次分配 + N 次释放。

这正是赛题里"临时对象创建和内存分配开销"那一条的落点；而且它同时作用在**分词基准和构建基准**上。

**改法**（两步，可分开做）：

1. `hits_` 换成定长栈上数组 + 计数。同一游标出发的前缀命中数受**最大词长**约束
   （IK 词典最长词条的汉字数），是个小的常数上界；上界取 8/16，溢出时回落到 `ObList`。
   这一步把最常见的分配直接归零。
2. `token_list_` / `result_list_` 换成支持 `reserve` 的连续容器，或在 `reuse()` 时保留
   已分配的 chunk、只重置水位（bump allocator 语义），让稳态下不再向系统要内存。

**风险**：中。`hits_` 的上界必须论证清楚 —— 取小了会漏词（**正确性事故**），
所以第 1 步要保留溢出回落路径，而不是硬截断。

**收益来源**：分词两项 + 构建三项（IK 建索引时整篇分词）。

---

### B3 · 英文归一化对每个 token 无条件分配 + 复制

**位置**：`src/share/text_analysis/ob_token_stream.cpp:296-301`

```cpp
ObString norm_alnum_token(norm_token_len, norm_token_ptr);
ObString norm_lower_token;
ObCharset::tolower(cs_, norm_alnum_token, norm_lower_token, norm_allocator_);
next_token.set_string(norm_lower_token);
```

**机制**：`ObCharset::tolower`（`src/oblib/lib/charset/ob_charset.cpp`）的实现是**无条件**的：

```cpp
if (1 == casemulti) {
  ob_write_string(allocator, src, dst);          // 先整段复制
  cset->casedn(cs_info, dst.ptr(), ..., dst.ptr(), ...);  // 再原地转小写
} else {
  buf = allocator.alloc(src.length() * casemulti); // 或者按倍率分配
  cset->casedn(...);
}
```

它没有"无变化就直接复用输入"的分支。英文正文里绝大多数 token 本来就是小写，
于是每个 token 白付：一次分配 + 一次全量复制 + 一次 casedn 扫描。

**改法**：转小写前先扫一遍判有没有需要改的字符，没有就把 `dst` 直接指向 `src`。

```cpp
bool needs_lower = false;
for (uint32_t i = 0; !needs_lower && i < norm_alnum_token.length(); ++i) {
  const uint8_t c = static_cast<uint8_t>(norm_alnum_token[i]);
  needs_lower = (c < 0x80) ? (c >= 'A' && c <= 'Z') : true;  // 非 ASCII 保守走慢路径
}
if (!needs_lower) {
  norm_lower_token = norm_alnum_token;   // 零拷贝
} else if (OB_FAIL(ObCharset::tolower(cs_, norm_alnum_token, norm_lower_token, norm_allocator_))) {
}
```

**风险**：低。要点只有一个：`norm_lower_token` 指向的是**输入文档的 buffer**，
必须确认该 buffer 的生命周期覆盖 token 的消费期。这一点本来就是成立的 ——
`ObTextWhitespaceTokenizer` 返回的 token 也是直接指向 `input_doc_->ptr_` 的偏移，
消费方从没拿到过独立副本。

**收益来源**：`tokenize_beng_avg_ms` + `build_beng_en_sec`（BENG 全走这条链）。

---

### B4 · 分组哈希表对重复 token 做两次查找

**位置**：`src/share/text_analysis/ob_token_stream.cpp:384-392`

```cpp
int hash_ret = grouping_map_.get_refactored(token_string, grouped_token_freq);  // 第一次
if (OB_HASH_NOT_EXIST == hash_ret) {
  ob_write_string(token_allocator_, token_string, copied_token_string);
  grouping_map_.set_refactored(copied_token_string, curr_token_freq);
} else if (OB_SUCCESS == hash_ret) {
  *grouping_map_.get(token_string) += curr_token_freq;   // 第二次，同样的哈希+比较
}
```

**机制**：`get_refactored` 已经定位到了桶和元素，却只把 value 拷出来；紧接着为了写回又
用 `get()` 把同样的哈希与相等比较再做一遍。文档里重复出现的词越多（真实语料恰恰如此），
这部分重复开销占比越高。

**改法**：一次 `get()` 拿指针，空则插入，非空则原地累加。

```cpp
int64_t *freq = grouping_map_.get(token_string);
if (nullptr == freq) {
  ObString copied;
  ob_write_string(token_allocator_, token_string, copied);
  grouping_map_.set_refactored(copied, curr_token_freq);
} else {
  *freq += curr_token_freq;
}
```

**风险**：低。语义等价，只是去掉了重复查找。
**前提**：确认 `ObHashMap::get()` 对不存在的 key 返回 `nullptr`（原代码 `:392` 已经在
用 `grouping_map_.get(...)` 取指针并解引用，说明该接口确实返回可写指针）。

**收益来源**：`tokenize_ik_avg_ms`（分组是 IK 的默认路径）。

---

### B5 · 标点判定按字节逐个调用 charset 函数

**位置**：`src/share/text_analysis/ob_token_stream.cpp:205-219`

```cpp
for (int64_t i = 0; i < token_len; ++i) {
  const char *character = token + i;
  if (ob_ispunct(cs_, *character) || ob_iscntrl(cs_, *character)) {
    special_mark_cnt++;
  }
}
```

**机制**：这里的循环单位是**字节**而不是**字符**。UTF-8 汉字是 3 字节，于是每个汉字被
检查 3 次，每次两次 charset 函数调用。而且一旦发现第一个"非标点"字节就可以短路返回，
现在却要数完整个 token 才知道 `special_mark_cnt == token_len` 是否成立。

**改法**：改成"找第一个非标点/非控制字符，找到即返回 `is_valid = true`"，

```cpp
bool is_valid = false;
for (int64_t i = 0; !is_valid && i < token_len; ++i) {
  const uint8_t c = static_cast<uint8_t>(token[i]);
  if (c >= 0x80) {
    is_valid = true;                          // 非 ASCII 字节不可能是 ASCII 标点
  } else if (!ASCII_PUNCT_CTRL_TABLE[c]) {
    is_valid = true;
  }
}
```

**风险**：低，但**必须核对语义等价** —— 原实现把"全部字节都是标点"判为无效；
改成"存在任一非标点字节即有效"是同一命题的逆否，逻辑上等价。注意 `c >= 0x80` 直接判有效
这一条要确认：`ob_ispunct` / `ob_iscntrl` 对 UTF-8 首字节与续字节的行为需要实测核对，
若 charset 表把某些高位字节也算作标点，这一条就会改变结果。**这是本组里唯一需要先做实验
再动手的改动。**

**收益来源**：分词两项。

---

## 3. 这一轮不碰的部分

- **查询路径**（`query_cn / query_beng / query_mixed / query_limit`，占 1/3 权重）：
  需要看倒排迭代器 `ObFTSIterator` 的访问模式、多词条结果归并与 `LIMIT` 提前终止。
  这是独立的一块，硬做会让两份分析都变浅。
- **索引构建的并行/流水线**：涉及任务划分与数据分发，改动面大、回归风险高，
  在没有编译环境的前提下不应触碰。

按当前范围，即使全部生效，理论天花板也到不了满分 —— `(构建 + 分词 + 查询) ÷ 3` 里
查询那一项是 0。这是必须说清楚的：**本方案的目标是拿到构建与分词两块的分，不是冲 100。**

---

## 4. 验证流程（在 Linux 上，约半小时）

### 4.1 建立基线

```bash
# 干净主干，先测出自己机器上的基线（不要用 CI 的固定基线做本地对比）
git checkout <baseline-commit>
./build.sh --init && ./build.sh      # 参考仓库 README 的构建方式
# 启动 seekdb 实例后：
SKIP_LOAD=0 bash tools/benchmark/fts_large_bench.sh
```

完整跑一遍（含重新导入 20000 篇）比较慢，建议先用小规模确认脚本能通，再上全量。

### 4.2 定位真实瓶颈（不要跳过这一步）

```bash
# 分词基准期间采样，看时间到底花在哪
perf record -F 999 -g -- <分词基准命令>
perf report --stdio | head -60
```

**这一步是前提**：本报告全部结论都是静态推断，`perf` 的输出才是判据。如果火焰图显示
热点不在 `prepare_next_char` / `ObList::push_back` / `casedn`，那就说明我判断错了，
应该按火焰图走，而不是按这份文档走。

### 4.3 正确性对拍（比性能更重要）

赛题明确写了"不能仅凭性能分数认定功能通过"。每一处改动都要配一条对拍：

| 改动 | 对拍方式 |
| --- | --- |
| B1 ASCII 快路径 | 单测遍历 `0x00..0x7F`，比较快路径类型与 `do_classify<CHARSET_UTF8MB4>` 的返回值，要求逐值相等 |
| B2 `hits_` 定长数组 | 同一语料，改动前后 `TOKENIZE` 输出逐 token 比对（含 offset 与顺序） |
| B3 tolower 零拷贝 | 构造含大写、全小写、含非 ASCII 三组 token，比对 `TOKENIZE` 结果；再跑一遍 `MATCH` 命中数 |
| B4 哈希单次查找 | 高频重复词文档（如 100 个 "the"），比对分组后的 `freq` 完全一致 |
| B5 标点短路 | 纯标点 token、混合 token、UTF-8 token 各若干，比对有效/无效判定 |

### 4.4 出分

```bash
python3 tools/benchmark/fts_large_bench_score.py <report-file>
```

对比时机器负载、编译方式、`SKIP_LOAD` 必须一致，否则数字没有意义。

---

## 5. 不确定性清单

按"最可能让我判断落空"的顺序：

1. **热点可能不在分词器**。20000 篇文档的构建基准里，I/O 与倒排落盘可能才是大头，
   分词只占一小段。若是这样，B1/B2 的实际收益远低于预期。**必须先用 `perf` 确认。**
2. **`ob_ispunct` 对高位字节的行为未知**，B5 有踩语义的风险（已在 §2.B5 标注）。
3. **`hits_` 的上界论证**依赖 IK 词典最大词长，我手上没有该词典的统计数据。
4. **编译期风险**：所有 patch 都未编译。`ObList` 换容器的改动涉及类型签名，
   很可能需要连带改头文件与调用点。
5. **查询类指标为 0**，综合提升率被摊薄到最多 2/3。

---

*本文档不含任何性能数字。原因见开头：没有在目标平台上编译和运行过，写出来的数字就是编的。*
