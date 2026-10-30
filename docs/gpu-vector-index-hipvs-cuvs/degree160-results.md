# Construction improvement after official feedback

Task 47231289 / commit 76a68f2 scored 5.655 / 20: build=1.135x, query=1.009x, gates=pass. This was the repair submission; it was not the later worker commit b2487dd, which was still queued when development began.

This candidate inherits b2487dd and uses a constant CAGRA final graph degree of 160 and 50 NN-descent iterations on every input. Intermediate graph degree remains 256. GPU search uses the frozen hipVS implementation and unchanged search settings; all inputs remain FP32/L2. Thread reuse, persistence, input checks, SQL, ID mapping and CPU VSAG are unchanged. No dataset recognition, evaluator modification or external library modification is introduced.

Simply reducing iterations failed the recall delta requirement at 20/50/60/80 iterations. A conservative 90-iteration variant passed with a small improvement. Increasing the final graph's retained neighbors to 160 restored recall even at 50 iterations, with more substantial improvement. Its bridge was compiled and executed on Radeon Cloud.

Independent-seed Gaussian data: 250,000 rows, dimensions 128 and 256, 256 exact-ground-truth queries each, Top-10 and Top-100, three alternating timing rounds. All IDs are valid and unique; returned distances match CPU squared L2; snapshot reload succeeds. Candidate Recall@10/100 is 0.990625 / 0.9767578125 (128D) and 0.958203125 / 0.91296875 (256D). Frozen baseline is 0.98828125 / 0.965078125 and 0.95 / 0.9018359375. All candidate recalls improved in these samples.

Bridge build+first-query time is 16.018 / 17.299 seconds versus 25.494 / 24.842, ratios 1.592x / 1.436x. Query throughput ratios versus fresh-thread frozen baseline are 1.364x / 1.392x (128D Top-10/100), 1.281x / 1.281x (256D). C11/C++17 and all 32 host contract groups pass under memory/undefined/leak sanitizers with the actual 160-degree/50-iteration production source.

These are development bridge tests, not official SIFT/Hidden datasets, 4096 SQL queries or full server evaluation. Only one construction sample per configuration was taken, so statistical certainty is limited. Full frozen offline server build/link and official memory/quality/performance gates still require organizer evaluation. This does not establish full marks or 3x build acceleration. Previous reports describe historical configurations and are superseded for this candidate.

Memory check limitation: the available cloud instance expired after its four-hour lifetime before the candidate peak-memory result could be retrieved. Baseline 128D incremental peak was 559,939,584 bytes; no candidate peak is available. The 160-edge final graph adds 25% edge storage relative to 128, while the intermediate degree stays 256; this is not proof that overall peak meets the official 1.25x gate. Formal memory validation remains outstanding.
