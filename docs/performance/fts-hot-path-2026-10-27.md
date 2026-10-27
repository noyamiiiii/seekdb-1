# Full-text hot-path optimization

The kernel changes remove repeated work without changing dictionary contents, token boundaries, frequencies or MATCH semantics. The baseline is the team's preceding combined-feature commit `6263dfd3c83c7569937368bdd0d45ddc9cb382f8`.

## Bottlenecks and changes

- IK decoded a character independently for successive classification predicates. Decode once and preserve predicate precedence.
- IK fetched the same character and category again in each of its four processors. Pass the values already fetched by the parser loop.
- Each initial IK dictionary match scanned the ordered dictionary ranges. Sort the descriptor view and use binary search with the same inclusive UTF8MB4_BIN comparisons.
- English normalization allocated a lowercase copy for every word. Borrow the input view only for UTF8MB4 ASCII tokens that already contain no uppercase letters. Other tokens retain charset-based conversion. Persistent parser consumers still copy before requesting the next token.

No shared sort, index format, SQL optimizer or scoring code is changed. No result cache or input-specific behavior is introduced.

## Reproduction and measurements

The original `tools/benchmark/fts_large_bench.sh` and scoring files are unchanged. Every complete run used ROWS=20000, BATCH=500, ROUNDS=3000, QUERY_ROUNDS=200, SAMPLES=3, WARMUP=30, SKIP_LOAD=0. Ubuntu 24.04 in WSL, release build, one native instance with memory_limit=2G, local Unix socket. Before/after runs used the same configuration. Each column below is a separate complete run; units follow metric suffixes.

| Metric | Before | After 1 | After 2 | After 3 |
| --- | ---: | ---: | ---: | ---: |
| build_ik_all_sec | 6.978 | 8.370 | 6.540 | 6.976 |
| build_ik_content_sec | 5.639 | 5.135 | 5.036 | 5.339 |
| build_beng_en_sec | 3.936 | 2.727 | 2.837 | 2.931 |
| tokenize_ik_avg_ms | 0.1388 | 0.1278 | 0.2963 | 0.1267 |
| tokenize_beng_avg_ms | 0.1052 | 0.0945 | 0.0885 | 0.0871 |
| query_cn_avg_ms | 1.4599 | 1.3937 | 1.3816 | 1.2985 |
| query_beng_avg_ms | 2.1655 | 2.0856 | 2.0337 | 5.0955 |
| query_mixed_avg_ms | 1.6105 | 1.5839 | 1.5735 | 1.4660 |
| query_limit_avg_ms | 1.8445 | 1.8597 | 1.7746 | 1.6900 |

The first after run reduced IK TOKENIZE mean latency by 7.93% and BENG by 10.17%. Subsequent runs contain visible scheduler/system-load outliers (IK in after2, English query in after3). Index build and query deltas are mixed; these measurements do not establish a guaranteed aggregate gain. All samples and means are retained below rather than dropping outliers.

The unchanged fixed-CI-baseline scoring script reports 100/100 even for the local before version because this machine/socket setup is faster than that fixed environment. This is not an earned platform score and is not evidence of 50% improvement from these changes. Formal performance depends on the uniform remote evaluator.

## Correctness and regression evidence

- Full release target built successfully.
- 2,096 deterministic SQL outputs compare byte-for-byte with the before binary: multilingual/default/detail TOKENIZE outputs, repeated words and frequencies, punctuation, numeric connectors, malformed input, empty and NULL values, long inputs, MATCH row IDs and rounded relevance values, deterministic top-k ordering. The optimized index is rebuilt from the original 20,000 documents before comparison. Both snapshots SHA256: `82e8bb8d967493c04449088372a57b2a99ec4e11a6212bc0c02f5e71621c837a`.
- New native unit coverage checks classification for all 1,112,064 Unicode scalar values and invalid arguments. Both tests pass.
- Existing FTS mysqltests pass: basic_dml_sequel, create_fts_index_afterward, create_table_with_fts_index, domain_id_merge_with_compare_null, drop_index, index_view, load_data, offline_ddl_with_fts_index, online_ddl_on_table_with_fts_index, parser_properties, simple_query, tokenize_function.
- Three additional existing cases were attempted but not counted as passes: basic_dml stops at a missing auxiliary-table lookup; cn_word and partitioned_simple_query require the test harness admin account. Their assertions and expected results were not changed.

## Raw complete benchmark reports

### before

```text
========================================
FTS Large Benchmark Report
========================================
timestamp:              2026-10-27 15:25:21
label:                  before
git_head:               unknown
git_dirty:              1
rows:                   20000
batch:                  500
rounds:                 3000
query_rounds:           200
samples:                3
warmup:                 30
skip_load:              0
----------------------------------------
select1_avg_ms:         0.0753
select1_stdev_ms:       0.0014
raw_load_sec:           0.270
raw_load_rows_per_sec:  74074.1
build_ik_all_sec:       6.978
build_ik_content_sec:   5.639
build_beng_en_sec:      3.936
build_total_sec:        16.560
----------------------------------------
tokenize_ik_avg_ms:     0.1388
tokenize_ik_median_ms:  0.1373
tokenize_ik_stdev_ms:   0.0038
tokenize_beng_avg_ms:   0.1052
tokenize_beng_median_ms:0.1036
tokenize_beng_stdev_ms: 0.0037
----------------------------------------
query_cn_hits:          8001
query_cn_avg_ms:        1.4599
query_cn_stdev_ms:      0.0428
query_beng_hits:        11000
query_beng_avg_ms:      2.1655
query_beng_stdev_ms:    0.0589
query_mixed_hits:       7332
query_mixed_avg_ms:     1.6105
query_mixed_stdev_ms:   0.0176
query_limit_hits:       20
query_limit_avg_ms:     1.8445
query_limit_stdev_ms:   0.0479
========================================
```

### after

```text
========================================
FTS Large Benchmark Report
========================================
timestamp:              2026-10-27 15:35:18
label:                  after
git_head:               unknown
git_dirty:              1
rows:                   20000
batch:                  500
rounds:                 3000
query_rounds:           200
samples:                3
warmup:                 30
skip_load:              0
----------------------------------------
select1_avg_ms:         0.1118
select1_stdev_ms:       0.0294
raw_load_sec:           0.478
raw_load_rows_per_sec:  41841.0
build_ik_all_sec:       8.370
build_ik_content_sec:   5.135
build_beng_en_sec:      2.727
build_total_sec:        16.240
----------------------------------------
tokenize_ik_avg_ms:     0.1278
tokenize_ik_median_ms:  0.1234
tokenize_ik_stdev_ms:   0.0071
tokenize_beng_avg_ms:   0.0945
tokenize_beng_median_ms:0.0953
tokenize_beng_stdev_ms: 0.0027
----------------------------------------
query_cn_hits:          8001
query_cn_avg_ms:        1.3937
query_cn_stdev_ms:      0.0148
query_beng_hits:        11000
query_beng_avg_ms:      2.0856
query_beng_stdev_ms:    0.0440
query_mixed_hits:       7332
query_mixed_avg_ms:     1.5839
query_mixed_stdev_ms:   0.0404
query_limit_hits:       20
query_limit_avg_ms:     1.8597
query_limit_stdev_ms:   0.0334
========================================
```

### after-repeat

```text
========================================
FTS Large Benchmark Report
========================================
timestamp:              2026-10-27 15:44:07
label:                  after-repeat
git_head:               unknown
git_dirty:              1
rows:                   20000
batch:                  500
rounds:                 3000
query_rounds:           200
samples:                3
warmup:                 30
skip_load:              0
----------------------------------------
select1_avg_ms:         0.0712
select1_stdev_ms:       0.0018
raw_load_sec:           0.329
raw_load_rows_per_sec:  60790.3
build_ik_all_sec:       6.540
build_ik_content_sec:   5.036
build_beng_en_sec:      2.837
build_total_sec:        14.419
----------------------------------------
tokenize_ik_avg_ms:     0.2963
tokenize_ik_median_ms:  0.1261
tokenize_ik_stdev_ms:   0.2430
tokenize_beng_avg_ms:   0.0885
tokenize_beng_median_ms:0.0884
tokenize_beng_stdev_ms: 0.0012
----------------------------------------
query_cn_hits:          8001
query_cn_avg_ms:        1.3816
query_cn_stdev_ms:      0.0063
query_beng_hits:        11000
query_beng_avg_ms:      2.0337
query_beng_stdev_ms:    0.0132
query_mixed_hits:       7332
query_mixed_avg_ms:     1.5735
query_mixed_stdev_ms:   0.0205
query_limit_hits:       20
query_limit_avg_ms:     1.7746
query_limit_stdev_ms:   0.0079
========================================
```

### after-isolated

```text
========================================
FTS Large Benchmark Report
========================================
timestamp:              2026-10-27 15:54:17
label:                  after-isolated
git_head:               unknown
git_dirty:              1
rows:                   20000
batch:                  500
rounds:                 3000
query_rounds:           200
samples:                3
warmup:                 30
skip_load:              0
----------------------------------------
select1_avg_ms:         0.0886
select1_stdev_ms:       0.0040
raw_load_sec:           0.306
raw_load_rows_per_sec:  65359.5
build_ik_all_sec:       6.976
build_ik_content_sec:   5.339
build_beng_en_sec:      2.931
build_total_sec:        15.252
----------------------------------------
tokenize_ik_avg_ms:     0.1267
tokenize_ik_median_ms:  0.1271
tokenize_ik_stdev_ms:   0.0037
tokenize_beng_avg_ms:   0.0871
tokenize_beng_median_ms:0.0854
tokenize_beng_stdev_ms: 0.0027
----------------------------------------
query_cn_hits:          8001
query_cn_avg_ms:        1.2985
query_cn_stdev_ms:      0.0092
query_beng_hits:        11000
query_beng_avg_ms:      5.0955
query_beng_stdev_ms:    4.4699
query_mixed_hits:       7332
query_mixed_avg_ms:     1.4660
query_mixed_stdev_ms:   0.0299
query_limit_hits:       20
query_limit_avg_ms:     1.6900
query_limit_stdev_ms:   0.0384
========================================
```

## Individual timed samples


### before

`	ext
  select1: sample=1/3 rounds=3000 total_ms=223.043 avg_ms=0.0743
  select1: sample=2/3 rounds=3000 total_ms=222.976 avg_ms=0.0743
  select1: sample=3/3 rounds=3000 total_ms=231.962 avg_ms=0.0773
  tokenize_ik: sample=1/3 rounds=3000 total_ms=411.864 avg_ms=0.1373
  tokenize_ik: sample=2/3 rounds=3000 total_ms=405.399 avg_ms=0.1351
  tokenize_ik: sample=3/3 rounds=3000 total_ms=431.981 avg_ms=0.1440
  tokenize_beng: sample=1/3 rounds=3000 total_ms=310.664 avg_ms=0.1036
  tokenize_beng: sample=2/3 rounds=3000 total_ms=331.240 avg_ms=0.1104
  tokenize_beng: sample=3/3 rounds=3000 total_ms=305.168 avg_ms=0.1017
  query_cn: sample=1/3 rounds=200 total_ms=291.166 avg_ms=1.4558
  query_cn: sample=2/3 rounds=200 total_ms=281.929 avg_ms=1.4096
  query_cn: sample=3/3 rounds=200 total_ms=302.865 avg_ms=1.5143
  query_beng: sample=1/3 rounds=200 total_ms=417.934 avg_ms=2.0897
  query_beng: sample=2/3 rounds=200 total_ms=434.674 avg_ms=2.1734
  query_beng: sample=3/3 rounds=200 total_ms=446.670 avg_ms=2.2334
  query_mixed: sample=1/3 rounds=200 total_ms=320.355 avg_ms=1.6018
  query_mixed: sample=2/3 rounds=200 total_ms=326.996 avg_ms=1.6350
  query_mixed: sample=3/3 rounds=200 total_ms=318.944 avg_ms=1.5947
  query_limit: sample=1/3 rounds=200 total_ms=356.238 avg_ms=1.7812
  query_limit: sample=2/3 rounds=200 total_ms=379.417 avg_ms=1.8971
  query_limit: sample=3/3 rounds=200 total_ms=371.037 avg_ms=1.8552
`

### after

`	ext
  select1: sample=1/3 rounds=3000 total_ms=238.403 avg_ms=0.0795
  select1: sample=2/3 rounds=3000 total_ms=451.822 avg_ms=0.1506
  select1: sample=3/3 rounds=3000 total_ms=315.633 avg_ms=0.1052
  tokenize_ik: sample=1/3 rounds=3000 total_ms=366.266 avg_ms=0.1221
  tokenize_ik: sample=2/3 rounds=3000 total_ms=370.077 avg_ms=0.1234
  tokenize_ik: sample=3/3 rounds=3000 total_ms=413.363 avg_ms=0.1378
  tokenize_beng: sample=1/3 rounds=3000 total_ms=272.321 avg_ms=0.0908
  tokenize_beng: sample=2/3 rounds=3000 total_ms=291.921 avg_ms=0.0973
  tokenize_beng: sample=3/3 rounds=3000 total_ms=285.776 avg_ms=0.0953
  query_cn: sample=1/3 rounds=200 total_ms=279.941 avg_ms=1.3997
  query_cn: sample=2/3 rounds=200 total_ms=274.653 avg_ms=1.3733
  query_cn: sample=3/3 rounds=200 total_ms=281.620 avg_ms=1.4081
  query_beng: sample=1/3 rounds=200 total_ms=417.214 avg_ms=2.0861
  query_beng: sample=2/3 rounds=200 total_ms=427.833 avg_ms=2.1392
  query_beng: sample=3/3 rounds=200 total_ms=406.271 avg_ms=2.0314
  query_mixed: sample=1/3 rounds=200 total_ms=322.915 avg_ms=1.6146
  query_mixed: sample=2/3 rounds=200 total_ms=322.051 avg_ms=1.6103
  query_mixed: sample=3/3 rounds=200 total_ms=305.371 avg_ms=1.5269
  query_limit: sample=1/3 rounds=200 total_ms=368.905 avg_ms=1.8445
  query_limit: sample=2/3 rounds=200 total_ms=381.212 avg_ms=1.9061
  query_limit: sample=3/3 rounds=200 total_ms=365.713 avg_ms=1.8286
`

### after-repeat

`	ext
  select1: sample=1/3 rounds=3000 total_ms=210.445 avg_ms=0.0701
  select1: sample=2/3 rounds=3000 total_ms=209.278 avg_ms=0.0698
  select1: sample=3/3 rounds=3000 total_ms=221.297 avg_ms=0.0738
  tokenize_ik: sample=1/3 rounds=3000 total_ms=1919.909 avg_ms=0.6400
  tokenize_ik: sample=2/3 rounds=3000 total_ms=368.252 avg_ms=0.1228
  tokenize_ik: sample=3/3 rounds=3000 total_ms=378.331 avg_ms=0.1261
  tokenize_beng: sample=1/3 rounds=3000 total_ms=265.219 avg_ms=0.0884
  tokenize_beng: sample=2/3 rounds=3000 total_ms=260.946 avg_ms=0.0870
  tokenize_beng: sample=3/3 rounds=3000 total_ms=269.994 avg_ms=0.0900
  query_cn: sample=1/3 rounds=200 total_ms=274.528 avg_ms=1.3726
  query_cn: sample=2/3 rounds=200 total_ms=277.131 avg_ms=1.3857
  query_cn: sample=3/3 rounds=200 total_ms=277.271 avg_ms=1.3864
  query_beng: sample=1/3 rounds=200 total_ms=409.985 avg_ms=2.0499
  query_beng: sample=2/3 rounds=200 total_ms=406.749 avg_ms=2.0337
  query_beng: sample=3/3 rounds=200 total_ms=403.493 avg_ms=2.0175
  query_mixed: sample=1/3 rounds=200 total_ms=316.012 avg_ms=1.5801
  query_mixed: sample=2/3 rounds=200 total_ms=309.155 avg_ms=1.5458
  query_mixed: sample=3/3 rounds=200 total_ms=318.946 avg_ms=1.5947
  query_limit: sample=1/3 rounds=200 total_ms=355.388 avg_ms=1.7769
  query_limit: sample=2/3 rounds=200 total_ms=352.804 avg_ms=1.7640
  query_limit: sample=3/3 rounds=200 total_ms=356.576 avg_ms=1.7829
`

### after-isolated

`	ext
  select1: sample=1/3 rounds=3000 total_ms=256.728 avg_ms=0.0856
  select1: sample=2/3 rounds=3000 total_ms=257.642 avg_ms=0.0859
  select1: sample=3/3 rounds=3000 total_ms=282.486 avg_ms=0.0942
  tokenize_ik: sample=1/3 rounds=3000 total_ms=365.947 avg_ms=0.1220
  tokenize_ik: sample=2/3 rounds=3000 total_ms=393.394 avg_ms=0.1311
  tokenize_ik: sample=3/3 rounds=3000 total_ms=381.317 avg_ms=0.1271
  tokenize_beng: sample=1/3 rounds=3000 total_ms=254.789 avg_ms=0.0849
  tokenize_beng: sample=2/3 rounds=3000 total_ms=256.280 avg_ms=0.0854
  tokenize_beng: sample=3/3 rounds=3000 total_ms=272.705 avg_ms=0.0909
  query_cn: sample=1/3 rounds=200 total_ms=262.186 avg_ms=1.3109
  query_cn: sample=2/3 rounds=200 total_ms=259.151 avg_ms=1.2958
  query_cn: sample=3/3 rounds=200 total_ms=257.758 avg_ms=1.2888
  query_beng: sample=1/3 rounds=200 total_ms=391.468 avg_ms=1.9573
  query_beng: sample=2/3 rounds=200 total_ms=2283.368 avg_ms=11.4168
  query_beng: sample=3/3 rounds=200 total_ms=382.484 avg_ms=1.9124
  query_mixed: sample=1/3 rounds=200 total_ms=288.815 avg_ms=1.4441
  query_mixed: sample=2/3 rounds=200 total_ms=289.126 avg_ms=1.4456
  query_mixed: sample=3/3 rounds=200 total_ms=301.648 avg_ms=1.5082
  query_limit: sample=1/3 rounds=200 total_ms=348.835 avg_ms=1.7442
  query_limit: sample=2/3 rounds=200 total_ms=333.327 avg_ms=1.6666
  query_limit: sample=3/3 rounds=200 total_ms=331.866 avg_ms=1.6593
`
