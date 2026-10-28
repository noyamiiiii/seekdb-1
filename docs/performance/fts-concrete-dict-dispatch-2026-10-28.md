# IK concrete dictionary dispatch

The built-in IK dictionaries used by `TOKENIZE` and index construction are
always `ObFTRangeDict` views over `ObFTCacheDict` DAT readers. Both concrete
classes are `final`, but the hot path previously reached them through
`ObIFTDict` virtual calls for every character.

This change keeps the generic dictionary interface for other users, while the
built-in IK parser retains the concrete range dictionary and stores the concrete
cache dictionary in each trie hit. Character matching therefore calls the
already selected concrete reader directly. Dictionary construction, range
selection, DAT transitions, token arbitration and output remain unchanged.

## Measurements

Eight alternating sets of 6,000 `TOKENIZE` statements ran against two server
processes started from the same persisted-data snapshot:

| Version | Median | Mean |
| --- | ---: | ---: |
| Current commit `e403dbeec` | 904.382 ms | 895.904 ms |
| Concrete dispatch | 873.470 ms | 851.193 ms |

The median improved by 3.42%. A complete adjacent benchmark on the same host
measured IK index build from 4.914 s to 4.352 s and IK tokenization from
0.1678 ms to 0.1575 ms. BENG tokenization and queries are not on the changed
dispatch path, and their variation in these host-loaded runs is treated as
noise. Raw reports are retained in
[data/fts-concrete-dict-dispatch-2026-10-28](data/fts-concrete-dict-dispatch-2026-10-28).

## Correctness

- The release target builds successfully.
- All 10 native FTS tests pass.
- `tokenize_function` and `cn_word` SQL suites pass with unchanged expected
  output. The latter used only the local root connection instead of the test
  harness admin account.
- All benchmark, grading and CI files remain unchanged.
