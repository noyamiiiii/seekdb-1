# Evidence index

- `diffdec-microbench-forward.csv`: 12 pairs with the previous commit on 2881 and
  the candidate on 2882.
- `diffdec-microbench-swapped.csv`: the same measurement with the binaries
  swapped, used to separate code effect from port bias.
- `native-fts.txt`: all 17 native `fts_index` tests plus the two LF-normalized
  SQL suites on the candidate.
- `results.txt`: per-orientation deltas, the combined estimate and the
  correctness evidence.
