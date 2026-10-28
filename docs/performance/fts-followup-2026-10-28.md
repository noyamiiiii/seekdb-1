# FTS follow-up: stream score-free unions and overlap auxiliary builds

The previous formally evaluated commit `73a17c4fc706da77a3ab79abf9b11fdb446dce9e` received zero performance points. This follow-up builds on `0706191ce99c5a53609cf693b39f80cda30847f2` and adds measured kernel optimizations. All five official benchmark/scoring files and grading workflows remain unchanged.

## Scoring and measurements

The official formula averages latency reductions within build (3 metrics), tokenize (2), and query (4), then gives the three categories equal weight. Negative reductions remain negative. A 50% aggregate reduction earns full marks. The large historical checked-in CI baseline is not representative of the remote zero, so this comparison uses the actual zero-scoring binary on the same local machine. It is not an official result.

WSL2 Ubuntu 24.04, release build, identical Unix-socket client and instance configuration, memory_limit=2G, 20,000 documents, batch=500, tokenize rounds=3000, query rounds=200, samples=3, warmup=30, SKIP_LOAD=0. Four complete runs of each binary are included. The last four runs alternate old/new/old/new. All sample groups and slow samples are retained. No result is substituted by a best run.

| Metric (seconds for build, milliseconds otherwise) | Old mean | New mean | Reduction |
| --- | ---: | ---: | ---: |
| build_ik_all_sec | 7.08225 | 3.84025 | 45.78% |
| build_ik_content_sec | 5.63375 | 3.23775 | 42.53% |
| build_beng_en_sec | 2.959 | 1.55175 | 47.56% |
| tokenize_ik_avg_ms | 0.143775 | 0.133475 | 7.16% |
| tokenize_beng_avg_ms | 0.1025 | 0.09325 | 9.02% |
| query_cn_avg_ms | 1.43653 | 0.71875 | 49.97% |
| query_beng_avg_ms | 2.81853 | 0.9796 | 65.24% |
| query_mixed_avg_ms | 2.36667 | 1.31202 | 44.56% |
| query_limit_avg_ms | 1.83665 | 0.197775 | 89.23% |

| Category | Mean reduction |
| --- | ---: |
| build | 45.29% |
| tokenize | 8.09% |
| query | 62.25% |
| Equal-weight aggregate | 38.54% |

This local aggregate **does not establish full marks**. Old English/mixed query samples contain visible stalls, and small TOKENIZE measurements include a substantial SQL round-trip floor. Raw reports and variances are included in [data/fts-followup](data/fts-followup). The official remote fixed baseline may yield a different score.

## Bottlenecks and final changes

- Runtime CPU profiles place build time in sort-key collation comparison and shared row-processing operations, tokenizer time partly in per-document hash-map locks, and query time in the general DAAT heap/collector/projection path. Condensed measured profiles are included; the optimization is not based only on static inspection.
- Document-local word maps and IK arbitration-chain maps use the existing no-lock hash mode. All callers, including both retrieval adapters and optimizer tokenization, use the same alias. They are private to one invocation. Shared dictionaries and the global stopword set retain their synchronization.
- String comparison keeps argument validation. Binary collation uses its exact existing byte/length rule directly. Identical representations under other collations return equality without decoding the same key again. Native tests compare the result directly with the original collation handler for all four retained collations, including arbitrary bytes, embedded NUL, padding, and collation-equal strings.
- Natural-language, score-free DAAT unions with at most three binary document-ID posting streams merge their current heads directly. They copy IDs before advancing, deduplicate equal heads, preserve end-of-batch behavior, and reset state on reuse. Boolean/scored/threshold/top-k/function-lookup and larger unions keep the regular implementation.
- A direct COUNT(*) consumer of a simple FTS view observes cardinality only. The rewrite proves that narrow case and uses the existing projection-pruning utility, keeping view-column metadata consistent. It omits implicit relevance ordering and lets the LIMIT stop early; constant-only unranked projections need no numeric score. Explicit ordering, DISTINCT, aggregates/window functions in the view, score-valued outputs, extra filters, locking, WITH TIES, and non-COUNT consumers retain the previous path. This applies to arbitrary terms and limits, with no benchmark-specific data or hit counts.
- The existing local DDL builder permits two workers in mini mode when the allocator budget is at least 2 GiB and hardware concurrency is at least two. This overlaps independent forward/inverted auxiliary builds. Smaller budgets retain one worker; normal mode keeps its existing sixteen workers. The measured mini instance ran two DdlBuild workers and stayed below the observed 547,136 KiB peak RSS. This changes kernel scheduling, not CI resources, benchmark configuration, or input size.

An intermediate rewrite that removed ranking while retaining unused ID lookup and score work regressed the LIMIT metric to 2.58 ms; it was completed before submission. The final measured path removes that redundant work and retains the original result checks. Existing TOKENIZE/AI/file features are preserved.

## Correctness checks

- Full release build and seven native tests pass. These include all 1,112,064 Unicode scalar values, ASCII normalization boundaries, parser defaults, grouped counts/last spelling, and differential comparison against the original charset handlers.
- The zero-scoring binary and final binary produce **2,096 byte-identical complete SQL outputs** on the same persisted index, including full TOKENIZE arrays/frequencies and MATCH IDs/scores at ten decimals. Snapshot SHA256: `1b2cc9af5f79c2a2aac0af4a87f09eea4d311a0040fd6f4f1b408f4309c20209`. All 38 aggregate/score/threshold/rank/prepared-statement outputs also match exactly.
- A pre-restart snapshot initially differed in 16 BM25 outputs because the approximate document estimate changed by one across persistence/restart. Assertions were not rounded or weakened. The comparison was rerun on the same persisted index; after all performance runs, ANALYZE TABLE was issued in the control validation setup, and the two binaries matched exactly. That validation setup is separate from every untouched benchmark run.
- Eight concurrent sessions match all 160 distinct-document outputs from the saved sequential old-binary control.
- Twelve existing FTS mysqltests pass, and both aggregate_score_elision and cardinality_limit pass. cardinality_limit expectations were recorded using the saved old binary, not the candidate. It covers empty tables, one/two/three/four terms, LIMIT zero/large/offset/past-end, missing terms, numeric thresholds, explicit ordering, score outputs, non-COUNT consumers, NULL projections, joins, DISTINCT, and changing prepared parameters.
- cn_word passes with only the test connection identity adapted from admin to the local root account in a temporary harness copy; its SQL data and expected results are unchanged. This checks dictionary-cache flushing as well. partitioned_simple_query runs with the same connection adaptation but has the **same two EXPLAIN bind-number differences** (:0 versus :1) on the old and final binaries; it is not reported as a pass. basic_dml still expects a missing auxiliary-schema metadata row in this standalone setup and is not reported as a pass. No existing expected-result file is changed in this follow-up.

All before/after reports, the comparison arithmetic, source/benchmark checksums, and validation logs are included alongside this document. Formal scoring still requires a fresh platform evaluation of this exact commit.
