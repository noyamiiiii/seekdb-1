# Evidence index

- `concrete-dict-microbench.csv`: eight alternating 6,000-query runs against
  the current commit and the concrete-dictionary candidate.
- `current-full-adjacent-report.txt`: complete baseline report.
- `concrete-full1-report.txt`: complete candidate report.
- `concrete-native.txt`: all native FTS tests.
- `concrete-sql.txt`: `tokenize_function` and `cn_word` SQL suites.

Both benchmark servers reused the same local data layout and were run under the
same host load. The full report labels are dirty because the candidate was
measured before its final commit.
