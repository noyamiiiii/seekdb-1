# Query worker host validation (2026-10-30)

This candidate inherits the V5 repair and changes only scheduling in the single-query CAGRA adaptor path. The same `ob_cuvs_job` runs on a reused 32 MiB stack. Build, search, snapshot and recall settings are unchanged.

Correctness checks pass for lazy startup, thread-creation failure and retry, actual stack size, thread reuse, synchronous output visibility, exceptions/recovery, recursive-call rejection, eight concurrent clients submitting 8,000 jobs, and shutdown/repeated lifecycles. ASan, UBSan, leak checking and ThreadSanitizer pass. Both OB_BUILD_CUVS enabled/disabled versions of the actual V5 vector adaptor compile into object files with clang 17 and the locally available seekdb dependencies. This is not a complete server link or a reproduction of the organizer's frozen offline build.

Seven alternating-order rounds compare identical callbacks. Median create/join overhead is 32.418 us versus 26.824 us for the worker, a 1.209x host dispatch ratio (17.26% time reduction). This measures host scheduling only, not GPU kernel time, SQL QPS, recall, or official score. No full-score claim is made.

Run `unittest/oblib/vector/cuvs_query_worker/run.sh` for correctness checks and the host benchmark. Run the same test source with `-fsanitize=thread` for the race check. The initial host-only stage did not establish GPU or SQL performance. Subsequent GPU bridge checks are documented below; full SQL validation remains outstanding.


## Actual GPU bridge validation (2026-10-30)

The production bridge was compiled with hipcc on the Radeon Cloud image. The test harness executes exactly the candidate worker header with the production C bridge; the frozen V5 bridge is the comparison. It tests build, synchronous one-query search, serialize, free, deserialize, and search again. It does not replace or modify hipVS.

On seeded Gaussian FP32 vectors, 10,000 and 250,000 rows, dimensions 128 and 256, all returned IDs were valid and unique, distances matched CPU squared L2, snapshots reloaded successfully. Top-10/100 recall was measured against exact neighbors on 64 queries. Each timing mode used five alternating-order rounds of 256 single queries. Equal neighbor ordering is not required when distances tie; all returned distances and recall are checked. An initial strict ordering assertion was rejected because the unchanged baseline itself was observed to return a different ordering while distance and recall checks passed; it was not a runtime failure of the candidate.

For 250K x 128, frozen-baseline Recall@10/100 was 0.9796875 / 0.96109375; candidate was 0.984375 / 0.9653125. For 250K x 256, baseline was 0.9515625 / 0.89609375; candidate was 0.95625 / 0.90125. Reusing the thread did not change recall within either built graph. Against frozen-bridge fresh-thread queries, candidate reuse achieved geometric-mean speedups 1.438x (128D) and 1.358x (256D). Bridge build ratios were only 1.054x and 1.002x; this change does not establish a build-performance score.

These are development tests, not official SIFT/Hidden datasets or 4096 SQL queries, and do not establish official performance or full marks. Peak VRAM was not formally profiled. Complete server build/link, SQL integration and organizer scoring remain outstanding. The library on the available image was not independently proven to be the organizer's exact frozen hipVS commit.

A separate rejected experiment reduced NN-descent to 20 iterations: build improved to about 11 seconds but recall fell too far. Increasing candidate-list size to 1024 produced search error 15 on the available library. Neither change is present in this candidate's production source.

A final 50-iteration experiment reduced bridge build to 16.70/15.89 seconds but 256D Recall@100 was 0.8859375 versus baseline 0.89609375 (delta -0.01015625), outside the allowed -0.005 rule. Search-width 8 did not rescue the 20-iteration variant. All these graph changes were rejected; production still uses 100 iterations and search width 4.
