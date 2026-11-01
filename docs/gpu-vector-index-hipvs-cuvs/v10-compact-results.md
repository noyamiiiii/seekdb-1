# v10: compact snapshots and shorter NN-descent construction

The reference is the officially scored 18.438/20 commit `1230d0955f2d948f5c61e5221dde0a1c26584c53` (160/192/16, build 2.654x, query 1.691x, all gates passed). This change is a direct descendant of that commit. It has no official score yet; no full-marks claim is made.

## Behavior

NN-descent keeps final degree 160 and intermediate degree 192, with 10 construction iterations. Every search uses the organizer's unchanged hipVS CAGRA implementation with supported search width 16 and 16 initial random samplings. More starting samples compensate for the lower-quality graphs seen when reducing iterations alone. The FP32 dataset, L2 metric and external IDs remain unchanged; the policy is fixed across build, initial query, subsequent queries and reload.

The old seekdb envelope stored host vectors and then a hipVS snapshot that included the same vectors again. The new envelope writes flag 1 and stores IDs plus the complete hipVS snapshot once. On reload, the bridge obtains the persisted dataset view and copies its FP32 rows to the host buffer with HIP's pitched copy, respecting strides and byte offset. That buffer remains available for normal incremental rebuild and CPU fallback bookkeeping. Legacy flag-zero envelopes are still readable. No graph is reused between independent builds, and reload adopts the saved graph without rebuilding it.

The bridge also fixes C++ cleanup jumps in the experimental brute-force path and a host API double's allocation expression. These fixes allow all five construction paths to compile as C11/C++17; only NN-descent is the selected production path.

## Full SQL validation on Radeon Cloud

These are real 250K-row SQL measurements, timing CREATE VECTOR INDEX through its first query. Each index then serves 4096 individual Top-10 and 4096 individual Top-100 SQL requests. SIFT uses the public ANN benchmark's first 250K training rows and first 4096 queries; its split is not certified as the official split. The 256D Gaussian data is a public, seeded proxy and is not Hidden250K. The paired reference is the latest scored configuration, not a replacement for the organizer's frozen-V5 pairing. The order is reference/candidate for SIFT, candidate/reference for 256D.

| Dataset | 16-iteration reference | v10 candidate | Relative speedup | Recall@10 delta | Recall@100 delta |
|---|---:|---:|---:|---:|---:|
| sift | 10.200 s | 9.001 s | 1.133x | -0.000049 | -0.000022 |
| gauss256 | 12.607 s | 11.197 s | 1.126x | +0.004956 | +0.013291 |

Both variants passed ID validity, ordinary `lib=vsag` SQL, and process restart tests. Restarting the old flag-zero database with the new adaptor also passed. The JSON records retain absolute recalls, QPS, P95 and timings. The comparisons above pass a development check of recall delta >= -0.005 against the latest scored configuration. Search quality compensation has a cost: some Gaussian query rates regress relative to that configuration. The official 0.90 query floor compares with frozen V5; it has not been established on Hidden250K. Some Gaussian bridge experiments, including the previously scored 16-iteration configuration, lose more than 0.005 recall against frozen V5 on that proxy; all of those results remain in the evidence and are not labeled official passes.

## Other validation

The full seekdb executable and bridge were built from frozen V5 sources plus this adaptor/bridge change with the offline dependency cache, ROCm 7.2.4, and the contest CMake target. The initial build and incremental adaptor build completed. Host checks pass all five algorithm syntax checks in C11/C++17, 43 bridge contract groups under address/undefined/leak sanitizers, and actual adaptor serialization code tests for compact/legacy formats, original vectors/IDs, truncated and malformed data, no rebuild and export-error cleanup. Real GPU export tests preserve every FP32 bit before and after reload at dimensions 7, 127, 128, 129 and 256.

The search sweep excluded unsupported itopk=768/1024 configurations after the fixed library returned an error. More random samplings improved recall but reduced query throughput; 8 iterations with 20 samplings did not give a sufficient quality/throughput margin over the selected 10/16 combination. Bridge measurements are component timings, not the full SQL construction window. First-round GPU-memory samples are recorded in `amd-v10-status.json`; they are development samples and not the official whole-workload resource gate.

## Reproduction

From a checked-out repository, run `bash unittest/oblib/vector/cuvs_bridge_contract/run.sh` and `python3 tools/hipvs_bridge/check_gpu.py`. Compile the bridge with `bash tools/hipvs_bridge/build.sh` and build the seekdb target using the official CMake commands. No new downloadable dependency is needed by production code.

The exact development scripts are saved under `evidence/v10-compact/reproduce/`. They use numpy and PyMySQL supplied separately by the development image/test setup, a 32 MiB dispatch stack, a full source build at `/workspace/seekdb-v10`, and the public NPZ data files under `/workspace/seekdb_hipvs_notebook`. Adjust those development paths to your checkout. `amd-v10-selectedsql.py sift` and `amd-v10-selectedsql.py gauss256` compile the reference (16 iterations, search width 4, one random sampling) and candidate bridge, run paired SQLs, and validate reload/VSAG. The reference executable is saved before applying the compact adaptor at `/workspace/amd-v10-scored16-seekdb`; it is solely a local comparison artifact. Official evaluation rebuilds all binaries from the submitted source.

Only a fresh official evaluation can establish the final score, hidden-data recall and official memory/time gates. This version is uploaded for user-controlled submission.
