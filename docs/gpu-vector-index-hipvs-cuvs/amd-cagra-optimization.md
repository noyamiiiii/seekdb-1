# AMD CAGRA bridge optimization

This submission directly descends from the mandatory frozen baseline
`08c76418a8e1fef3c1aae83010658ff56a4faa57` (`amd-cagra-contest-baseline-v1`).
It changes the maintained C bridge and its pthread link option. It does not
change the evaluator, build workflows, SQL interface, dataset precision or CPU
VSAG implementation. The official statement is
[AMD contest](https://oceanbase.yuque.com/org-wiki-obtech-vh7w9r/hc3pio/seebnbaoxot1b0tb).

## Implementation

- Use the existing CAGRA NN-descent option for dimensions >=128 to avoid the
  IVF-PQ training pass on measured high-dimensional workloads. Lower dimensions
  retain the library's default algorithm: an all-NN-descent trial regressed a
  64-dimensional SQL batch workload and was rejected. Graph degrees, iteration
  counts, input FP32 values and distance semantics are preserved.
- Own query, neighbor and distance device buffers per index and reuse capacity.
  Each buffer tracks its own allocated size. Larger allocations replace older
  buffers only after allocation succeeds. Destruction releases every allocation.
- Reuse search parameters while resetting the internal candidate-list size to
  the original default for each request. Requests such as top-100 grow the list
  to a supported multiple of 32. A large request does not silently make later
  small requests use a larger candidate list.
- Serialize searches sharing an index with a per-index mutex so callers cannot
  overwrite another query's scratch space. This does not cache query results,
  input vectors or previous-round graphs.
- Check pointers, dimensions, top-k bounds and byte-size overflow before GPU
  allocation. Initialize all DLPack metadata and explicitly synchronize CAGRA
  build/search before returning. FP32 L2 distances and every output-copy failure
  are checked.

## Validation and measurement limits

The changed bridge was compiled from source using the Radeon Cloud template's
ROCm 7.2.4/hipVS installation. Full seekdb was **not** rebuilt from the frozen
source locally: cloud Git clone failed through the proxy and local archive
retrieval timed out. SQL integration uses the template's supplied seekdb binary
with the newly compiled bridge selected by LD_LIBRARY_PATH. That binary predates
the October 26 frozen commit; these SQL runs cannot replace the official
source-build evaluation. No precompiled binary is included in this submission.

Bridge-only cold comparisons run independent processes in the order baseline,
candidate, candidate, baseline, baseline, candidate. Every build is new, and
the first query is included. Each version's median is reported. Data generation
and brute-force ground truth happen outside the timer. All completed samples
are retained in the evidence directory.

| Rows | Dimensions | Frozen bridge + first query (s) | Candidate (s) | Speedup | Warm batch speedup |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 2048 | 32 | 0.823055 | 0.831669 | 0.990x | 1.034x |
| 10000 | 128 | 1.130505 | 0.485543 | 2.328x | 0.981x |
| 50000 | 128 | 1.272124 | 1.265296 | 1.005x | 0.999x |

These are **bridge-only build times**, not seekdb ALTER/CREATE INDEX end-to-end
times. The 2.33x result is limited to one tested size; the other tested sizes do
not establish the proposed 2.0x full-mark target. Warm batch query improvement
also does not establish the proposed 1.5x full-mark target.

The final 64-dimensional SQL batch comparison includes input scan, new GPU
CAGRA construction, search, result inserts and reading all returned rows. Its
median baseline/candidate speedup is 1.006x. Every run opened /dev/kfd and a GPU
render device, and returned legal mapped IDs and correct FP32 distances. This
test is a batch integration measurement, not the official single-index build
metric.

In that SQL batch sample the median Recall@10 was 0.9875 for the original bridge
and 0.984375 for the candidate (delta -0.003125). Individual builds are stochastic:
the worst same-round difference was -0.0125 on only 32 queries. All individual
values are retained. Therefore these results do **not** establish every official
Recall gate. Candidate SQL Recall@100 was 0.9803125 on the final run. The public
statement does not provide the final absolute Recall floor or hidden dataset
weights, so no thresholds or formal score are invented.

The final standalone GPU validation used 10,000 128-dimensional vectors and
128 probes near stored vectors. Recall@10 was 0.99609375 and Recall@100 was about
0.973. Exact values are in the attached JSON. This dataset differs from the
Gaussian independent-probe benchmark above. All returned IDs were unique and
in range; returned distances matched direct FP32 L2 calculations. Tests also
cover batch growth/shrink, top-1/10/100, four simultaneous callers on one handle,
NULL handles and invalid dimensions/overflow. These are development tests, not
substitutes for hidden Recall evaluation.

## Rejected measurements and retained limitations

The initial all-NN-descent SQL batch trial measured 0.788x, below the 90% query
throughput gate. It is retained as rejected evidence and is not the final code.
Low-dimensional inputs now keep the default build algorithm.

The supplied binary's standalone CREATE INDEX / reload path returned valid
CPU VSAG results without a GPU device descriptor. That experiment was rejected
as evidence for GPU index construction, despite its valid output and near-equal
timings. A separate explicit batch_knn test established real GPU activity. CPU
index isolation, filters, deletes and insertion freshness were tested in that
supplied binary, but not claimed as a full frozen-source server regression pass.

The frozen original bridge also rejected a direct top-100 request with its
default internal search-list size. The candidate fixes that configuration;
original top-100 bridge timings are not silently substituted or reported as a
valid control.

## Reproduce

Use a compatible AMD GPU environment with hipVS, ROCm, Python and NumPy:

```bash
docs/gpu-vector-index-hipvs-cuvs/validation/run_bridge_comparison.sh /tmp/seekdb-cagra-validation
```

This compiles the exact frozen bridge and candidate, validates the candidate,
and runs alternating bridge comparisons. The script explicitly labels them
bridge-only. For the full source build and official SQL smoke test, follow the
existing README:

```bash
tools/hipvs_bridge/build.sh /tmp/seekdb-cagra-build/libseekdb_cuvs_bridge.so
./build.sh release --init -DOB_BUILD_CUVS=ON -DOB_BUILD_CUVS_TRACE=OFF \
  -DCUVS_BRIDGE_LIB=/tmp/seekdb-cagra-build/libseekdb_cuvs_bridge.so --make -j16
```

Official scoring must rebuild the submitted exact commit and run the fixed
cold-start datasets and all hard gates. Full marks are **not established** by
these development measurements.
