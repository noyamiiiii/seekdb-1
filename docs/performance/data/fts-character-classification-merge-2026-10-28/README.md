# Evidence index

- `classify-bench.txt`: three pure CPU comparisons of the original and combined
  character validation/classification paths.
- `dual-microbench.csv`: eight alternating 6,000-query runs against baseline and
  candidate servers using the same persisted-data snapshot.
- `current-baseline-report.txt`, `base2-report.txt`,
  `base-after-candidate-report.txt`: complete baseline benchmark reports.
- `candidate-merged-report.txt`, `candidate-merged2-report.txt`,
  `candidate-final-report.txt`: complete candidate benchmark reports.
- `baseline-skip-1.txt` through `baseline-skip-3.txt`: baseline tokenize/query
  runs over one persisted index.
- `candidate-skip-1.txt` through `candidate-skip-3.txt`: candidate tokenize/query
  runs over the same persisted index.
- `fts-native-final.txt`: all native tests.
- `fts-sql-final.txt`: `tokenize_function` and `cn_word` SQL suites.

All reports were produced before the final commit and therefore identify the
source as `unknown` or dirty. The committed source is the exact code measured;
no benchmark or grading file was changed.
