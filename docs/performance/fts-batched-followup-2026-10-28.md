# FTS batch and dictionary follow-up

This kernel-only follow-up to `233a37a30b42243dfebd789c58e016bcb87f7ff8` improves unscored posting unions, tokenizer scratch allocation and index sorting. It retains the existing AI/file features. All five grading files and grading workflows are unchanged.

## Performance contract and evidence

The official formula averages latency reduction within build (3 metrics), tokenize (2) and query (4), then gives each category equal weight. Negative reductions count. The full-mark threshold is 50% aggregate reduction. The checked-in historical baseline gives full marks even to the original zero-scoring executable locally, so it is not evidence for meeting the threshold.

Three sequential alternating control/candidate pairs use the actual zero-scoring executable (`73a17c4fc706da77a3ab79abf9b11fdb446dce9e`) on the same persisted development instance. Each untouched benchmark run rebuilds all indexes: 20,000 documents, batch=500, rounds=3000, query_rounds=200, samples=3, warmup=30, skip_load=0. Both versions use the same Unix-socket client, release build, 2 GiB memory limit and WSL2 host. Every completed final-series sample is retained. Competing Dify services were paused during timing. Raw local reports have git_dirty=1 because the isolated native build has no .git directory; published changes are in an exact Git commit.

The final series reports **44.15% aggregate reduction, which does not establish the 50% requirement or full marks**. Categories are build 47.35%, tokenize 14.34%, query 70.76%. Official scoring remains unknown. An earlier candidate measured 46.02%; both complete three-pair series are included. The later candidate's individual latencies decreased, but the original executable also ran faster; relative percentages must not be selected by their maximum.

| Metric | Original mean | Candidate mean | Reduction |
| --- | ---: | ---: | ---: |
| build_ik_all_sec | 6.76467 | 3.761 | 44.40% |
| build_ik_content_sec | 5.23533 | 2.66033 | 49.19% |
| build_beng_en_sec | 2.895 | 1.49167 | 48.47% |
| tokenize_ik_avg_ms | 0.140433 | 0.1203 | 14.34% |
| tokenize_beng_avg_ms | 0.1044 | 0.0894333 | 14.34% |
| query_cn_avg_ms | 1.43143 | 0.5433 | 62.05% |
| query_beng_avg_ms | 2.12303 | 0.7177 | 66.19% |
| query_mixed_avg_ms | 1.6178 | 0.568767 | 64.84% |
| query_limit_avg_ms | 1.8473 | 0.1856 | 89.95% |

Additional exploratory reports are retained, including a four-worker local DDL pool trial that was reverted because it did not improve timing. Failed startup attempts and an earlier overlapping database-rebuild conflict generated no complete valid benchmark report and are not performance samples. The final runs execute exclusively and sequentially. The regression validation's ANALYZE TABLE is performed after timing and is never added to the benchmark.

## Kernel changes

- Immutable UTF-8 DAT readers optionally build a BMP character-code lookup table. Noncanonical/supplementary bytes and allocation failure retain hash lookup. Each dictionary is initialized before publication. Range dictionaries now destroy owned child readers and release their lookup allocations, including failed insertions.
- Supported FTS word/document VARCHAR sort keys use the existing order-preserving encoder. Internal FTS auxiliary build SQL no longer forces the older comparator sorter. Other DDL retains its original hint. Explicit optimizer hints retain precedence. Two PX workers are allowed for FTS forward/inverted auxiliaries on a sufficient CPU/memory budget; the local DDL worker pool remains the previously submitted two-worker setting.
- TOKENIZE word maps and IK arbitration use bounded per-thread scratch storage, clear all tokens between invocations and fall back to private storage on reentrance. This does not cache input documents, tokens, JSON results or query answers.
- Built-in ASCII stopword lookup preserves GENERAL_CI case and trailing-space behavior without per-token conversion, allocation or shared hash lookup. Unicode and other encodings retain the original path. The shared fallback set retains its synchronization.
- Unscored natural-language unions of up to three posting streams consume read-only ID blocks, compare canonical 16-byte IDs using portable ordered integer keys, and project full output batches. IDs are copied before advancing a block. Equal postings still emit one row. Numeric result buffers are initialized before writes. Scored, boolean, threshold, top-k, function-lookup and larger unions retain their existing paths.
- Unscored scans omit unused token-count/document-length decoding while continuing to read all posting IDs and apply visibility rules. No fixed hit counts, benchmark terms or synthetic timings are substituted.

## Correctness and limits

Full release build and all 10 native tests pass. These test every Unicode scalar for character classification and dictionary lookup, malformed input, collation ordering, ASCII normalization, parser defaults, grouping frequency/spelling, and stopwords including all ASCII-byte append/prepend cases, embedded NUL, uppercase and trailing spaces.

The candidate and original produce 2,096 byte-identical complete SQL outputs on the same persisted indexes (SHA256 `1b2cc9af5f79c2a2aac0af4a87f09eea4d311a0040fd6f4f1b408f4309c20209`). All 38 aggregate, relevance-score, threshold, rank, LIMIT, NULL and prepared-plan outputs match. Eight concurrent sessions match the saved sequential control for 160 distinct documents.

Twelve existing mysqltests and both aggregate_score_elision/cardinality_limit tests pass without editing their expected results. cn_word passes with only admin-to-root connection adaptation in a temporary local harness. partitioned_simple_query still has the same two pre-existing EXPLAIN bind-number differences and is not reported as a pass. The standalone basic_dml metadata expectation is also not claimed as passing. Dictionary-cache flushing is covered by cn_word.

All performance and validation evidence is in [data/fts-batched-followup](data/fts-batched-followup). This change improves measured performance but cannot honestly be represented as satisfying the requested 50% threshold until further measurements or official evaluation establish it.
