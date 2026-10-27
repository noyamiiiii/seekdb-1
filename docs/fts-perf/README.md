# 赛题四 · seekdb 全文索引调优（实测记录）

本目录是这一轮工作的全部证据材料。**先说结论，免得误读**：

- 我把链路跑通了：容器内从源码编译 seekdb → 跑通官方基准 `tools/benchmark/fts_large_bench.sh`
  → 用 perf 抓到了 build / tokenize / query 三个赛道的真实热点。
- 我改了分词热路径，A/B 实测（3 轮交替取中位数）**没有可测量的收益**，4 处改动回滚了 3 处。
- 结论是：分词器不是瓶颈。`tokenize` 指标的 65% 是 SQL 往返，`build` 指标的分词占比是个位数，
  `query` 走的是 DAAT 倒排迭代器。要拿分得动存储引擎的写入/排序/迭代路径。

具体数字、调用链、收益天花板测算都在 [`RESULTS.md`](RESULTS.md)。

## 目录

| 路径 | 内容 |
|---|---|
| `RESULTS.md` | 主报告：环境、A/B 数据、perf 调用链、天花板测算、下一步方向 |
| `BOTTLENECK.md` | 早期基于静态阅读的推断报告。**已被 perf 实测推翻**，保留作为对照，文首有状态声明 |
| `p1-classify-once.patch` | 唯一保留的改动，已应用到本分支 `src/storage/fts/ik/ob_ik_char_util.h` |
| `apply_patches.py` | 补丁管理脚本（精确文本匹配 + 命中次数校验，改错了会失败而不是乱补） |
| `measured/ab/` | 6 份 A/B 报告，before/after 各 3 轮交替跑 |
| `measured/reports/` | 单轮前后对比（**存在机器漂移，不作为结论依据**） |
| `measured/profiles/` | perf flat + 调用树，三个赛道各一份 |
| `official/` | 官方镜像首次基线 + 官方 CI 基线 JSON 与计分公式 |

## 关于保留的那处改动

`do_classify` 原来对同一个字符最多做 4 次 UTF-8 解码（`is_alpha` / `is_arabic` /
`is_chinese` / `is_other_cjk` 各解一次），现在解码一次、四个区间判定共用同一个 `ob_wc_t`。

语义等价（`decode_unicode` 是 `(input, char_len)` 的纯函数），零风险，但 **A/B 实测 +0.14%，
在噪声内** —— 我不声称它带来了收益，保留它是因为它确实更正确、更清楚，
而且 `do_classify` 在 tokenize 阶段只占 0.30%，本来就不该指望它。
