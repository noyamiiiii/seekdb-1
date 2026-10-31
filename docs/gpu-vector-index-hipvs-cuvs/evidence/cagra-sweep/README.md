# CAGRA construction sweep

The construction knobs live in `tools/hipvs_bridge/seekdb_cuvs_bridge.c` as
`SK_CAGRA_GRAPH_DEGREE`, `SK_CAGRA_INTERMEDIATE_DEGREE` and `SK_CAGRA_NITER`.
Their defaults are the swept best (160 / 192 / 40), so a normal build is
unchanged. The sweep overrides them with `-D` at compile time and never edits
the source between runs.

## Running on the Radeon instance

From the repository root on the GPU host:

```bash
AMD_TEST_ROWS=250000 python3 docs/gpu-vector-index-hipvs-cuvs/evidence/cagra-sweep/sweep.py
```

Pass configurations as arguments to replace the default list, where each entry
is `graph_degree:intermediate_degree:niter`, `iter:graph_degree[:intermediate_degree]`,
or an `ivfpq:...` configuration:

```bash
AMD_TEST_ROWS=250000 AMD_SWEEP_DIMS=256 \
  python3 docs/gpu-vector-index-hipvs-cuvs/evidence/cagra-sweep/sweep.py 160:256:50 192:256:35 192:192:35
```

Each configuration is compiled into `libbridge_<tag>.so` in this directory,
linked into `libdispatch_<tag>.so` next to the shared harness, and timed by the
existing `query-worker/gpu/benchmark.py`, which builds, runs the first query,
saves and reloads the snapshot, and measures recall against exact squared-L2
ground truth. Per-configuration logs are `libbridge_<tag>-<dim>.log`, and the
summary is written to `sweep-results.json`.

## Reading the output

`build_sec` is construction plus the mandatory first query, the same window the
official timing uses. Recall must stay at or above the candidate that already
scored `build=1.379x`; the pairing matters more than the absolute seconds,
because the evaluator always compares against the frozen baseline in the same
run.

## Results so far

`sweep-2026-10-30.json` is the first round that selected `160:192:50` over
`160:256:50`. `sweep-2026-10-30-round2.json` is the follow-up that moved the
iteration count to 40: on the same 250K-row proxy data it builds in
10.13 s (128D) and 10.61 s (256D), against 17.36 s and 17.61 s for the
previous submission's `160:256:50`, a 1.71x / 1.66x reduction. Recall@10 stays
at 0.9891 (128D) and 0.9688 (256D).

Both files report `rel` as speed relative to the fastest configuration within
the same dimension and round. Earlier versions of this script printed wrong
relative values for rows after the first one because of a `dict.setdefault`
mistake; that is fixed now, and the JSON numbers were computed from the
recorded `build` seconds.

## IVF-PQ construction

hipVS also exposes `build_algo = IVF_PQ`, which builds the neighbour graph with
an IVF-PQ all-neighbours search instead of NN-descent. The bridge accepts this
through `SK_CAGRA_BUILD_ALGO=1` plus `SK_CAGRA_IVFPQ_LISTS`,
`SK_CAGRA_IVFPQ_PROBES`, `SK_CAGRA_IVFPQ_PQ_BITS`, `SK_CAGRA_IVFPQ_PQ_DIM`,
`SK_CAGRA_IVFPQ_KMEANS_ITERS`, `SK_CAGRA_IVFPQ_TRAINSET` and
`SK_CAGRA_IVFPQ_REFINE`; zero keeps the library heuristic, and a refinement
rate above 1 recomputes exact distances for that multiple of the graph degree.
The sweep writes those configurations as
`ivfpq:degree:intermediate:n_lists:n_probes[:pq_bits[:refine]]`, for example:

```bash
AMD_TEST_ROWS=250000 python3 docs/gpu-vector-index-hipvs-cuvs/evidence/cagra-sweep/sweep.py \
  160:192:40 ivfpq:160:192:512:20 ivfpq:160:192:1024:32 ivfpq:160:192:2048:64
```

The production default remains NN-descent 160/192/40 until another path beats
it on build time and keeps Recall@10/100 within the official gates.

The 2026-10-30 IVF-PQ round on 250K Gaussian rows measured 7.70 s / 0.9859 at
128D for `512:20`, but 11.28 s at 256D, slower than NN-descent. Every IVF-PQ
setting plateaued at Recall@10 around 0.986, which points at the PQ codebook
rather than the number of probed lists. Refinement is the remaining knob for
that gap.

## Exact-graph construction

`SK_CAGRA_BUILD_ALGO=3` skips NN-descent entirely: hipVS brute force computes
the exact `graph_degree` nearest neighbours of every row, then
`cuvsCagraIndexFromArgs` adopts that graph together with the dataset. There is
no pruning pass and the graph rows are already distance-sorted. The sweep spells
this `bf:graph_degree`:

```bash
AMD_TEST_ROWS=250000 python3 docs/gpu-vector-index-hipvs-cuvs/evidence/cagra-sweep/sweep.py \
  bf:160 bf:96 bf:64
```

If brute force is cheaper than 250K x 250K x 40 NN-descent iterations on this
GPU, the build becomes compute bound on one fused pass and recall is the exact
kNN graph rather than an approximation. The trade-off to measure is wall time,
not quality.

## Iterative CAGRA construction

`ITERATIVE_CAGRA_SEARCH` builds its kNN graph by repeatedly calling CAGRA search
and graph optimization. The sweep accepts `iter:graph_degree:intermediate_degree`;
for example:

```bash
AMD_TEST_ROWS=250000 python3 docs/gpu-vector-index-hipvs-cuvs/evidence/cagra-sweep/sweep.py \
  160:192:40 192:192:30 192:192:25 224:192:25 224:192:20 256:192:20 iter:160 iter:192
```

The hipVS API marks this build algorithm experimental. Keep it as a benchmark
candidate until the real instance confirms both build time and Recall@10/100
on the relevant data. The latest official result supplied by the user is
16.686/20 (`build=2.313x`, `query=1.680x`, gates pass); it is the current
submission baseline, not an estimate of this experiment.

## Second change in this branch

`ObCuvsSnapshotFile` staged the cuVS graph in `tmpfile()` on the data disk and
then copied it back into the index stream, so a 250K build pushed several
hundred megabytes through the disk twice. The staging file now lives in
`/dev/shm` when available and falls back to `tmpfile()` otherwise, and the copy
buffer grew from 64 KiB to 8 MiB. This does not touch the graph, the search
implementation, recall or the persisted format.
