# CAGRA construction sweep

The construction knobs live in `tools/hipvs_bridge/seekdb_cuvs_bridge.c` as
`SK_CAGRA_GRAPH_DEGREE`, `SK_CAGRA_INTERMEDIATE_DEGREE` and `SK_CAGRA_NITER`.
The last official result is 18.438/20 for commit
`1230d0955f2d948f5c61e5221dde0a1c26584c53`, with 160 / 192 / 16,
build=2.654x, query=1.691x and gates passed. This follow-up uses 160 / 192 / 10,
search width 16 and 16 random samplings, together with a compact seekdb
snapshot envelope. See [the v10 validation report](../../v10-compact-results.md)
for measurements and their limits. It does not have an official score yet.

The sweep overrides construction values with `-D` at compile time. Search
settings can likewise be overridden with `SK_CAGRA_SEARCH_WIDTH` and
`SK_CAGRA_RANDOM_SAMPLINGS`; comparisons must report their actual settings.

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

`build_sec` measures the bridge build and first query. It excludes SQL data
scanning, seekdb snapshot persistence and catalog work, so it is a component
measurement and must not be presented as the official end-to-end window.
Full SQL validation times CREATE VECTOR INDEX through the first result.
Official recall gates compare against frozen V5 on the official data in the
same run. Proxy data and comparisons with the latest scored candidate are
development checks; they cannot establish the hidden-data gates or full marks.

## Results so far

Official progression: 20 iterations scored 17.962/20; 16 iterations scored
18.438/20. The current 10-iteration configuration also changes supported search
parameters to preserve quality. The following October 30 results are historical
experiments and do not describe the current defaults.

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

NN-descent remains the default construction algorithm. IVF-PQ remains an
experimental path; it is not used by the v10 candidate.

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

The hipVS API marks this build algorithm experimental. Iterative search remains
a benchmark option and is not used by the v10 candidate.

## Second change in this branch

`ObCuvsSnapshotFile` staged the cuVS graph in `tmpfile()` on the data disk and
then copied it back into the index stream, so a 250K build pushed several
hundred megabytes through the disk twice. The staging file now lives in
`/dev/shm` when available and falls back to `tmpfile()` otherwise, and the copy
buffer grew from 64 KiB to 8 MiB. This does not touch the graph, the search
implementation, recall or the persisted format.
