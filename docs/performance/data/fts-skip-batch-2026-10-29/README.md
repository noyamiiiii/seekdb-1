# Evidence index

- `skipbatch-paired-microbench.csv`: 12 alternating port pairs, 1000 queries per
  sample, run against the baseline commit and the candidate.
- `skipbatch-forward-microbench.csv`: single 4-iteration run in the canonical
  baseline/candidate port layout, kept for comparison with earlier rounds.
- `native-fts.txt`: all 17 native `fts_index` tests plus the two LF-normalized
  SQL suites on the candidate.
- `results.txt`: paired deltas, correctness evidence and the explanation for the
  five reject files (line endings only, identical to the baseline commit).
