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
is `graph_degree:intermediate_degree:niter`:

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

## Second change in this branch

`ObCuvsSnapshotFile` staged the cuVS graph in `tmpfile()` on the data disk and
then copied it back into the index stream, so a 250K build pushed several
hundred megabytes through the disk twice. The staging file now lives in
`/dev/shm` when available and falls back to `tmpfile()` otherwise, and the copy
buffer grew from 64 KiB to 8 MiB. This does not touch the graph, the search
implementation, recall or the persisted format.
