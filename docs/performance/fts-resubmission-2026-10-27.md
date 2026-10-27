# Full-text resubmission: remove repeated setup and unused relevance work

The previous submission `73a17c4fc706da77a3ab79abf9b11fdb446dce9e` was accepted by the submission platform but received a remote mean improvement of **−13.0411%** and zero raw performance points. This revision adds kernel changes and measured correctness checks; it does not resubmit the same code or change the benchmark.

## Scoring contract

The official task gives equal weight to build, tokenize and query categories. Within each category the per-metric latency reductions are averaged, then the three category means are averaged. A 50% aggregate latency reduction earns 100 raw performance points (the task is worth 50 contest points). Negative metric deltas remain in the mean. Formal scoring uses the remote fixed baseline.

The checked-in historical CI baseline reports very high local scores even for the previous binary. That did not predict the remote zero. Consequently the table below compares the actual previous binary with this revision on the same local instance. It is **not an official score or proof of full marks**.

## Changes and invariants

- The existing process-level dictionary hub owns one immutable view and pinned backing cache handles for each of the three built-in IK dictionaries. Acquire/release publication and one initialization mutex protect concurrent first use. Parser contexts and document state remain private. The views and handles are freed at global full-text shutdown; this trades bounded dictionary-cache residency for fewer per-document cache lookups, allocations and sorts.
- Short ASCII English tokens use an inline normalization buffer; long and non-ASCII tokens retain charset conversion. The tokenizer avoids charset dispatch for one-byte UTF8 ASCII. Consumers still copy tokens before advancing.
- Word grouping uses one `set_or_update` lookup and preserves the prior first-occurrence count, subsequent frequencies and **last spelling** for collation-equal words.
- Omitted parser properties use the existing typed defaults instead of formatting default JSON and immediately parsing it again. Explicit properties still pass through the original validation/canonicalization path.
- Primary natural-language index scans used only as truth predicates beneath scalar aggregates can omit BM25 estimation and per-row relevance calculation. Score-valued aggregates, nonzero/parameterized thresholds, Boolean mode, field boosts, function lookups, index merges, row-producing queries and LIMIT/top-k ranking retain the scored path. The score-free path initializes posting masks, hidden lookup projections, rescan state and DAAT/TAAT state; a long-query TAAT union retains duplicate-hit semantics and spill support.

## Controlled measurements

Ubuntu 24.04 / WSL2, release target, the same Unix-socket client, memory_limit=2G, Dify containers paused throughout these measurements. Unmodified complete benchmark: ROWS=20000, BATCH=500, ROUNDS=3000, QUERY_ROUNDS=200, SAMPLES=3, WARMUP=30, SKIP_LOAD=0. Each column is a complete load/index/tokenize/query run; the comparison averages both before runs and both final runs. No outlier is removed.

| Metric | Before 1 | Before 2 | Final 1 | Final 2 | Mean reduction |
| --- | ---: | ---: | ---: | ---: | ---: |
| build_ik_all_sec | 5.458 | 5.35 | 5.245 | 6.946 | -12.80% |
| build_ik_content_sec | 4.532 | 4.227 | 4.027 | 4.33 | 4.59% |
| build_beng_en_sec | 2.426 | 2.424 | 2.423 | 2.427 | 0.00% |
| tokenize_ik_avg_ms | 0.1314 | 0.1204 | 0.1176 | 0.1196 | 5.80% |
| tokenize_beng_avg_ms | 0.0946 | 0.084 | 0.0915 | 0.0885 | -0.78% |
| query_cn_avg_ms | 1.2238 | 1.1638 | 0.9415 | 0.885 | 23.50% |
| query_beng_avg_ms | 1.7939 | 1.8286 | 1.3416 | 1.3289 | 26.28% |
| query_mixed_avg_ms | 1.4105 | 1.4176 | 1.0719 | 1.0121 | 26.31% |
| query_limit_avg_ms | 1.6354 | 1.528 | 1.5152 | 1.487 | 5.10% |

| Category | Mean reduction |
| --- | ---: |
| build | -2.74% |
| tokenize | 2.51% |
| query | 20.30% |
| Equal-weight aggregate | 6.69% |

Build and small TOKENIZE measurements vary across runs. These results **do not establish the 50% reduction required for full marks**. Remote evaluation remains necessary; the fixed baseline, test inputs, timing logic, score code and CI configuration are unmodified. Raw reports, variances and comparison math are in [data/fts-resubmission](data/fts-resubmission).

## Correctness evidence

- Full release build succeeds; six native tests pass, including all 1,112,064 Unicode scalar values, inline-buffer boundaries 255/256/257/4096, all five parser default configurations, grouped frequencies and collation-equivalent final spelling.
- On the **same persisted 20,000-row index**, the saved previous binary and the final binary produce 2,096 byte-identical SQL outputs, including full TOKENIZE arrays/frequencies and MATCH IDs/relevance at ten decimals. Snapshot SHA256: `1b2cc9af5f79c2a2aac0af4a87f09eea4d311a0040fd6f4f1b408f4309c20209`.
- An older pre-flush snapshot had SHA256 `82e8bb8d967493c04449088372a57b2a99ec4e11a6212bc0c02f5e71621c837a`. Small BM25 differences appeared when comparing it with the persisted/restarted index. Running the **previous unmodified binary** on that same persisted index reproduced the final snapshot exactly. Assertions were not weakened or rounded further; the control was rerun on the same physical index state.
- 38 aggregate/score/threshold/rank/prepared-plan outputs match the previous binary exactly. 160 distinct documents across eight concurrent sessions match the sequential previous-binary control.
- Twelve existing FTS mysqltests pass: basic_dml_sequel, create_fts_index_afterward, create_table_with_fts_index, domain_id_merge_with_compare_null, drop_index, index_view, load_data, offline_ddl_with_fts_index, online_ddl_on_table_with_fts_index, parser_properties, tokenize_function, simple_query. The sole existing expected-result change is one EXPLAIN `calc_relevance=true` → `false` flag in `simple_query.result`; SQL inputs and expected functional rows remain unchanged.
- New `aggregate_score_elision.test` passes against its identical expected output on **both** binaries, including an empty table, NULL/empty/stopwords, score projection, threshold changes, LIMIT, correlated subquery and a 262-term query that forces TAAT. New cases do not use changed expected hit counts to mask a regression.
- The three previously unavailable local harness cases (basic_dml auxiliary lookup; cn_word/partitioned_simple_query admin setup) are not claimed as passes.

## Reproduction

Build the normal `seekdb` release target. Run the unchanged `tools/benchmark/fts_large_bench.sh` with the parameters above using a fresh OUTPUT file per complete run. Run the FTS mysqltest suite including `aggregate_score_elision`; keep the result file supplied with this change. The native regression source is `unittest/storage/fts/test_fts_character_classification.cpp` (gtest plus the normal kernel libraries).

The branch keeps the preceding AI function, file SQL, AI column and initial FTS changes in its ancestry. This commit does not edit the official benchmark or grading workflow.
