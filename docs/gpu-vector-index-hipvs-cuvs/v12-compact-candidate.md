# Compact snapshot with 12-iteration CAGRA

This candidate starts from the submitted 12-iteration configuration and adds
the compact snapshot format validated in the separate v10 experiment. Its
production defaults remain graph degree 160, intermediate degree 192, 12
NN-descent iterations, search width 4, and one random sampling.

The compact envelope avoids writing the FP32 vectors twice: the hipVS snapshot
already contains them, so the seekdb envelope stores the IDs and graph snapshot
only. On reload, the bridge exports the saved FP32 dataset back to host storage
for incremental rebuild and CPU fallback bookkeeping. Legacy flag-zero
snapshots remain readable. The graph is still loaded from the saved snapshot;
reload does not rebuild it.

## Evidence and limits

The v10 compact experiment used 10 iterations and search width 16 with 16
random samplings. Its SIFT and seeded Gaussian SQL runs showed 1.133x and 1.126x
build speedups over the 16-iteration reference, but also changed construction
and query settings together. Those numbers cannot isolate snapshot-format
benefit or predict this 12-iteration candidate. The detailed experiment is in
`evidence/v10-compact/` and `v10-compact-results.md`.

The official 14-iteration result recorded in the working session was 18.877/20
with build=2.747x, query=1.629x, and gates passed. This combined candidate has
not been officially scored. A full GPU build, hidden-set recall, official
memory ratio, and total score are still required before calling it a winner.

The current Windows workstation has no ROCm compiler or AMD GPU, no usable WSL
Linux environment for the test runner, and no running Docker engine. The
inherited bridge-contract and snapshot tests therefore have not been rerun for
this combined candidate on this workstation.
