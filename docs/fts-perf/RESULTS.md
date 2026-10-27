# 赛题四 · seekdb 全文索引调优：实测结论

> 这份文档取代了 `BOTTLENECK.md`。那份报告是基于源码静态阅读的推断，**本轮的 perf 实测推翻了它的主要结论**。
> 下面每个数字都来自本机真实编译、真实基准、真实采样，原始数据在 `measured/` 下。

---

## 0. 一句话结论

**我把完整链路跑通了（编译 → 官方基准 → perf 采样），拿到了三个赛道的真实热点分布。
结论是：这个赛题在此环境下的可优化空间远小于题面预期，靠分词热路径拿不到分。**

| 我做了什么 | 结果 |
|---|---|
| 在 Linux 容器里编译 seekdb 源码 | ✅ 成功，13 分钟，产出 1.33GB 二进制 |
| 复现官方基准 `fts_large_bench.sh` | ✅ 成功，命中数与官方基线逐项一致 |
| 改分词热路径（4 处）并 A/B 实测 | ❌ **实测收益为零，已回滚 3 处** |
| 用 perf 定位真实热点 | ✅ 三个赛道全部采到，见 §3 |
| 测算收益天花板 | ⚠️ 见 §5，远低于 50% 满分线 |

---

## 1. 环境与方法

### 1.1 编译

```bash
# 依赖初始化（7.4GB，走代理）
./build.sh init
# 配置（build.sh release 只做 configure，不做编译）
./build.sh release
# 编译
cmake --build build_release -j8
```

产物：`build_release/src/observer/seekdb`，1,332,349,352 字节，`RelWithDebInfo -O2`。
工具链：devtools gcc 12.3.0 + Rust 1.98.1（项目 `rust/rust-toolchain.toml` 钉死版本）。

### 1.2 跑基准

```bash
./seekdb --nodaemon --base-dir=/work/inst --data-dir=/work/inst/store \
         --redo-dir=/work/inst/store/redo --port=2881 \
         --parameter cpu_count=8 --parameter memory_limit=3G
```

客户端用 `deps/3rd/u01/obclient/bin/obclient`，通过 `MYSQL` 环境变量喂给官方脚本。

**校验通过**：自编译版本的四项查询命中数是 `8001 / 11000 / 7332 / 20`，
与官方 `fts_large_bench_baseline.json` 里的 `query_*_hits` **完全一致**。数据链路是对的。

### 1.3 为什么要 A/B 交替

第一轮我先跑 before 再跑 after，得到 tokenize_ik `0.1621 → 0.1455`（看起来 -10%）。
但同时 `select1`（纯 `SELECT 1`，**我的改动根本影响不到它**）从 `0.0749` 变成 `0.0872`，涨了 16%。

说明机器本身在漂移。于是改成 **before/after 交替跑 3 轮**，取中位数。
`select1` 的漂移量（-2.15%）就是本次测量的噪声底噪，任何小于这个数的"改善"都不算数。

---

## 2. 实测数据

### 2.1 A/B 交替，3 轮中位数

| 指标 | before | after（4 处改动） | 变化 |
|---|---|---|---|
| `select1_avg_ms`（对照组） | 0.0929 | 0.0949 | −2.15% ← **噪声地板** |
| `build_ik_all_sec` | 7.581 | 7.678 | −1.28% |
| `build_ik_content_sec` | 5.461 | 5.361 | +1.83% |
| `build_beng_en_sec` | 3.052 | 3.136 | −2.75% |
| `tokenize_ik_avg_ms` | 0.1434 | 0.1432 | +0.14% |
| `tokenize_beng_avg_ms` | 0.1049 | 0.1010 | +3.72% |
| `query_cn_avg_ms` | 1.3348 | 1.3293 | +0.41% |
| `query_beng_avg_ms` | 1.9640 | 2.0007 | −1.87% |
| `query_mixed_avg_ms` | 1.5249 | 1.5105 | +0.94% |
| `query_limit_avg_ms` | 1.7851 | 1.7931 | −0.45% |

**读法**：所有变化都在 ±3% 以内，且方向不一致（有正有负）。
`tokenize_beng` 的 +3.72% 是唯一超过噪声的，但它对应的改动（P3 零拷贝）在 `tokenize_ik` 上是 +0.14%，
不足以支撑结论；更可能是那三轮 `tokenize_beng` 里有一轮跑快了（`0.1004` vs 其余 `0.1049/0.1053`）。

**结论：这组改动没有可测量的收益。**

### 2.2 单轮前后对比（供参考，不采信）

单跑时 `build_ik_content` 出现过 `5.456 → 6.360`（+16.6%），
与同批 `select1` 的 +16% 漂移完全吻合 —— 这正是不能只看一对数据的理由。

---

## 3. perf 实测热点

采样方式：`perf record -F 499 -g -p <observer_pid>`，分三个阶段单独抓。

### 3.1 tokenize 阶段（30000 次 `SELECT tokenize(text,'ik')`，1664 样本）

线程分布：

```
73.08%  ReqWorker      （服务器工作线程）
22.90%  sql-nio-2      [kernel]  ← 全部是 sendto → tcp_sendmsg → tcp_push
```

`sql-nio-2` 那 22.9% 的内核栈完整是：

```
entry_SYSCALL_64 → do_syscall_64 → __x64_sys_sendto → inet_sendmsg
→ tcp_sendmsg → tcp_push → ip_queue_xmit → ip_finish_output
```

**就是 SQL 结果回传给客户端。** 这一块服务器端代码动不了。

ReqWorker 内部的调用链（占该线程的百分比）：

```
ObThWorker::worker                            62.86%
└─ ObMPQuery::process_single_stmt             58.29%
   └─ ObQueryDriver::response_query_result    47.78%
      └─ ObResultSet::get_next_row            41.95%
         └─ ObExprTokenize::eval_tokenize     41.47%
            ├─ ObExprTokenize::parse_param     5.77%   ← 每次重新解析/序列化参数
            │  └─ ObIJsonBase::print          5.35%   ← 把 JSON 重新打印成字符串
            └─ ObExprTokenize::tokenize_fulltext  32.81%
               └─ ObFTParseHelper::segment    29.57%
                  └─ ObIKFTParser::get_next_token  19.29%
                     └─ ObIKCJKProcessor::do_process  10.82%
                        ├─ ObFTDATReader::match_with_hit     6.19%
                        ├─ ObFTRangeDict::match              4.39%  ← 全部在 find_first_char_range
                        └─ ObAddWord::process_word           4.69%
```

**关键观察**：我改的 `ObFTCharUtil::do_classify` **连前 20 名都没进**（在整轮采样里只占 0.30%）。
它下面调的 UTF-8 解码器 `ob_mb_wc_utf8mb4` 在 tokenize 阶段也没进前 10。
这就是 P1（把 4 次解码合并成 1 次）实测无效的原因 —— **被优化的那段代码本来就不是热点**。

### 3.2 build 阶段（`ALTER TABLE ... ADD FULLTEXT INDEX ... WITH PARSER ik`，3712 样本）

纯 self 排名，**最高 2.64%，没有一个函数超过 3%**：

```
2.64%  ObOperator::get_next_batch
2.16%  ob_strnncollsp_utf8mb4          ← 排序时的 collation 比较
2.13%  __memset_avx2_unaligned_erms
2.05%  ObIArrayWrap<...>::at
1.83%  __memmove_avx_unaligned_erms
1.80%  ob_mb_wc_utf8mb4
1.80%  ObSortOpImpl::Compare::operator()
1.59%  ObNullSafeDatumStrCmp<45,...>::cmp
1.51%  __memcmp_avx2_movbe
```

调用树显示 64% 走并行执行框架（`ObPxPool` → `ObPxTaskProcess`），
主体是 `ObPxMultiPartSSTableInsertOp::write_ordered_slice_by_row`（39.33%，写 SSTable）
和排序比较（`ObSortOpImpl::Compare` + `ob_strnncollsp_utf8mb4` + `memcmp` ≈ 7%）。

**分词在这里几乎看不见。** 建索引的耗时花在存储引擎的写与排序上，不在分词器里。

### 3.3 query 阶段（1500 次 `MATCH ... AGAINST`，1225 样本）

```
4.82%  ObSRDaaTIterImpl::filter_on_demand
4.65%  ObIArrayWrap<ObHashSet<...>>::at
4.24%  __memmove_avx_unaligned_erms
4.08%  ObStringDiffDecoder::batch_decode
3.76%  ObBitVectorImpl<unsigned long>::set
3.51%  calc_bool_expr_for_double_type
3.35%  ObBitVectorImpl<unsigned long>::at
2.94%  ObSRDaaTIterImpl::fill_merge_heap
2.78%  ObExprBM25::eval_batch_bm25_relevance_expr
2.45%  ObSRDaaTIterImpl::collect_dims_by_id
2.37%  ObSimpleRowsMerger<...>::push
2.20%  ObSRDaaTIterImpl::project_results
```

全部集中在 DAAT 倒排迭代器的内层循环（`filter_on_demand` / `fill_merge_heap` /
`collect_dims_by_id` / `merge_heap` 堆操作），合计约 30%。
这些是逐条微优化（循环外提、去边界检查），单项收益个位数。

---

## 4. 为什么静态分析的结论是错的

`BOTTLENECK.md` 定位了 5 个"瓶颈"（链表逐节点分配、`tolower` 无条件复制、
标点判定按字节遍历、分组哈希双查找、字符分类两次跨层调用）。实测结果：

| 静态推断 | 实测 |
|---|---|
| 链表分配是热点 | `ObList` 相关函数未进任何阶段前 20 |
| `tolower` 无条件复制是热点 | `ObBasicEnglishNormalizer` 未进榜；改掉后 `tokenize_beng` 变化在噪声内 |
| 标点判定按字节遍历是热点 | `filter_special_marks` 未进榜；改掉后无变化 |
| 分组哈希双查找是热点 | `build_grouping_map` 未进榜；改掉后无变化 |
| 字符分类重复解码是热点 | `do_classify` 仅 0.30%；改掉后被淹没 |

**共同错误**：这五处都在"分词器内部"找问题，而实测显示分词只占
`tokenize` 指标的约 35%（其余是 SQL 往返）、占 `build` 指标的个位数百分比。
**优化一个只占 5% 的部分，整体看不出变化是必然的。**

教训很直接：`perf` 应该跑在改代码之前。我这次顺序反了，代价是白编译了四轮。

---

## 5. 收益天花板测算

要拿满分需要 `(build + tokenize + query) ÷ 3 ≥ 50%`。

### tokenize 类（2 项指标）

`tokenize_ik_avg_ms = 0.1434`，其中 `select1_avg_ms = 0.0929` 是纯 SQL 往返。

```
服务器端实际处理 = 0.1434 − 0.0929 = 0.0505 ms（占 35%）
即使服务器端优化到 0，指标只降 35%
```

CI 基线上这个比例更差：`0.76478 − 0.21032 = 0.5545`，服务器端占 72%（CI 机器更慢，
往返开销也更大）。所以同一份改动在 CI 上的相对收益会比本地更明显，
但**上限仍是 35%~72%，且不可能优化到 0**。

### build 类（3 项指标）

分词占比个位数。要动的是 SSTable 写入、排序比较、PX 框架 —— 存储引擎核心。
这类改动的工作量以周计，风险等级完全不同。

### query 类（4 项指标）

DAAT 迭代器内层循环占 30%，逐项微优化合计可能拿到 5%~10%。

### 综合

乐观估计（tokenize 拿到 20%、build 拿到 5%、query 拿到 10%）：

```
(0.20 + 0.05 + 0.10) ÷ 3 = 11.7%  →  23 分
```

这需要改存储引擎和 DAAT 迭代器，工作量与风险都远超本次会话能覆盖的范围。

---

## 6. 保留下来的改动

只保留了一处，`src/storage/fts/ik/ob_ik_char_util.h` 的 `do_classify`：

```cpp
// 原来：is_alpha / is_arabic / is_chinese / is_other_cjk 各自解码一次 UTF-8
// 现在：解码一次，四个分类器共用同一个 ob_wc_t
ob_wc_t unicode = 0;
if (OB_FAIL(decode_unicode<CS_TYPE>(input, char_len, unicode))) {
} else if (ObUnicodeBlockUtils::is_alpha(unicode)) {
  type = CharType::ENGLISH_LETTER;
} else if (ObUnicodeBlockUtils::is_arabic(unicode)) {
  ...
```

保留理由：
- **语义等价**，可逐行论证：`decode_unicode` 是 `(input, char_len)` 的纯函数，
  原来四次调用必然返回同一码点和同一状态；四个分类器本身只是对 `ob_wc_t` 做区间比较。
- **零风险**：不改变任何输出，不引入新分配，不动生命周期。
- **理论上正确**：确实把每个字符的 UTF-8 解码从 4 次降到 1 次。

**但我不声称它带来了收益** —— A/B 实测变化 `+0.14%`，在噪声内。

回滚的三处（`ob_token_stream.cpp` 的提前退出、零拷贝 `tolower`、单次哈希探测）：
零拷贝那处依赖"上游 token 指针指向文档缓冲"这一未经验证的假设，
在实测收益为零的情况下，没有理由保留这个风险。

---

## 7. 复现

```bash
# 1. 依赖 + 编译（Linux，Ubuntu 24.04，约 7.4GB 依赖 + 13 分钟编译）
./build.sh init && ./build.sh release && cmake --build build_release -j8

# 2. 起实例
build_release/src/observer/seekdb --nodaemon --base-dir=/tmp/inst \
  --data-dir=/tmp/inst/store --redo-dir=/tmp/inst/store/redo \
  --port=2881 --parameter cpu_count=8 --parameter memory_limit=3G

# 3. 跑官方基准
MYSQL="obclient -h127.0.0.1 -P2881 -uroot -N -s --default-character-set=utf8mb4" \
LABEL=before OUTPUT=/tmp/report-before.txt \
  bash tools/benchmark/fts_large_bench.sh

# 4. 分阶段 profile（需要 --privileged 容器，否则 perf_event_paranoid=2 挡住）
perf record -F 499 -g -p $(pgrep -f 'seekdb .*--nodaemon') -o perf-tokenize.data
```

原始材料：
- `measured/ab/` —— 6 份 A/B 报告（before/after 各 3 轮）
- `measured/reports/` —— 单轮前后对比
- `measured/profiles/perf-{tokenize,build,query}.txt` —— perf flat + 调用树
- `official/baseline-official-image.txt` —— 官方镜像跑的首次基线
- `official/ci-baseline.json` —— 官方 CI 基线参数与分数公式

---

## 8. 如果继续做，应该往哪走

按收益/风险排序：

1. **`ObExprTokenize::parse_param`（5.77%）** —— 每次 `tokenize()` 都把 parser 属性
   JSON 重新解析、rebuild、再序列化回去。这些输入在同一个表达式里是常量。
   在 expr 级别缓存可以省掉这部分，改动局限在 `ob_expr_tokenize.cpp`。**低风险，收益约 5%。**

2. **`ObFTParseHelper` + `token_map` 复用** —— 每次调用都在栈上新建 helper、
   新建 hash map。赛题文本里那句"解析器实例复用、内存池化数据结构"就是指这里。
   需要把 helper 挂到 eval ctx 上，改动中等。

3. **DAAT 迭代器微优化** —— `ObIArrayWrap::at` 的边界检查在 DAAT 内层热循环里，
   `ObBitVector::set/at` 未内联。循环外提 + 内联，收益 5%~10%，但涉及存储层热点路径，
   需要严格的功能对拍。

前两项做完大概能到 10%~15% 的 tokenize 改善，对应总分个位数。要突破 50%，
必须动排序/写入路径，那是另一个量级的工作。
