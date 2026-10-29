# V5 submission repair (2026-10-29)

## Verified causes

The previous submission `b1b3428b8bde6f35c92ec7ad8a69f6c52bada3af` was rejected
with `failMsg=4`. The platform reported no error text or test cases, and its
execution timestamps span one second (2026-10-29 09:58:05–09:58:06 Asia/Shanghai).
The current official statement maps this code to repository URL/authentication
failure. That submission used an HTTPS repository URL without embedded read-only
authentication. The registration must be corrected separately; no credential is
stored in this source, documentation, logs or test artifacts.

The current statement also requires V5 commit
`634888d9b037153fa143d2c913ba4b25503e1d62`; the prior submission descended from V1.
A new submission must satisfy V5 ancestry even after correcting authentication.
The public rules now define the build full-score target as 3.0x and query target
as 1.5x. Old bridge-only V1 measurements do not apply to V5 or establish either.

## Repair

This version directly descends from the V5 frozen commit and retains its complete
kernel and bridge persistence ABI. In particular, `seekdb_cuvs_serialize` and
`seekdb_cuvs_deserialize` remain available. Deserialization checks the stored
shape and restores a GPU graph without rebuilding it. No V1 kernel files are
applied over V5.

The candidate preserves **every V5 quality parameter**: NN_DESCENT, graph degree
128, intermediate degree 256, 100 iterations, search width 4 and candidate-list
size `max(library_default, 512, 4*k)`. Consequently no previous small-graph or
low-recall tuning is inherited. The default is restored for each query so a
larger earlier k does not silently alter later searches.

The optimization reuses per-index query/neighbor/distance buffers and search
parameter objects. Each allocation has an independent byte count, allocation
failure preserves usable old storage, and a per-index mutex protects scratch
reuse and serialization. Destruction releases all buffers. Input/size validation,
initialized DLPack metadata, explicit GPU synchronization and checked copy errors
make failures observable.

The CPU VSAG code, fixed evaluator, index parameters, CMake configuration and
third-party libraries are unchanged. The bridge build script adds `-pthread`.

## Validation

Run the host-only contract suite:

```bash
unittest/oblib/vector/cuvs_bridge_contract/run.sh
```

It compiles the actual bridge as C11 and C++17, then tests it with cuVS API doubles
under address/undefined/leak sanitizers. All 32 groups pass: all six exports,
exact frozen build/search parameters, serialize/deserialize without a build,
wrong snapshot shapes, scratch reuse/growth/shrink, four concurrent callers,
NULL and overflow handling, injected creation/build/search/sync/copy/allocator
failures, subsequent recovery, and final resource accounting. These tests
validate host-side contracts; **they do not execute GPU code or measure speed**.

A fresh Radeon Cloud launch returned `No matching Node has enough free CPU,
memory, and GPUs for this launch`. Therefore this V5 repair has not been run on
a real GPU or compiled as a complete seekdb server in this local session. The
old prebuilt V1 template cannot validate V5's offline-DDL persistence path and is
not used as replacement evidence. Full official build, GPU profiling, recall,
throughput and VRAM gates remain to be evaluated.

Use the exact official offline compilation commands in the current contest
statement once a compatible environment is available. Existing V5 GPU tests,
including `tools/hipvs_bridge/test_persistence.cpp` and
`docs/gpu-vector-index-hipvs-cuvs/validation/run_ddl_persistence_smoke.sh`, are
preserved unchanged. Full marks and successful formal evaluation are not claimed.
