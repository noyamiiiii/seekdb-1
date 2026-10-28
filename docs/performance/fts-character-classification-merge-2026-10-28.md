# FTS character validation and classification merge

This follow-up reduces repeated work in the IK UTF-8 hot path. The existing
`first_valid_char` call validates and measures one character, then
`classify_first_char` decodes the same character again. The new path validates,
decodes and classifies it once, returns its byte length through the same call,
and gives the parser character pointer, length and classification in one
context lookup.

The output-stage arbitrator uses the same combined decode when it starts a new
character. Non-UTF8 charsets retain the original validation and classification
path. The change does not alter benchmark data, SQL, timing, grading scripts,
benchmark selection, token output, or query semantics.

## Measurements

A pure CPU comparison ran 200,000 passes over the same 53-character mixed
Chinese/English input. Three consecutive measurements were:

| Original validation plus classification | Combined path | Ratio |
| ---: | ---: | ---: |
| 228,530 us | 109,626 us | 47.97% |
| 220,988 us | 109,734 us | 49.66% |
| 223,194 us | 111,193 us | 49.82% |

This isolates the changed character path. End-to-end SQL is dominated by client,
protocol and statement overhead on the local host. Two server processes were
therefore started from the same persisted data snapshot and given eight
alternating sets of 6,000 `TOKENIZE` statements:

| Version | Median | Mean |
| --- | ---: | ---: |
| Original | 1,328.672 ms | 1,338.856 ms |
| Combined path | 1,315.423 ms | 1,361.183 ms |

The median improved by 1.00%; the means overlap because of host load variation.
Twelve fixed-setting SQL benchmark reports are retained in
[data/fts-character-classification-merge-2026-10-28](data/fts-character-classification-merge-2026-10-28).
They include complete build/index, tokenize, and query runs. The host was noisy
enough that those full-run tokenize figures do not establish a stable aggregate
score change, so no official-score increase is claimed.

## Correctness

- The release target builds successfully.
- All 10 native FTS tests pass. `EveryUnicodeScalar` now checks both the original
  classifier and the combined path across every valid Unicode scalar; the
  invalid-input test checks malformed and truncated UTF-8.
- The existing `tokenize_function` suite passes with unchanged expected output.
- The existing `cn_word` suite passes with only the local admin connection
  changed to root; its SQL data and expected results are unchanged.

Raw native and SQL test output is preserved alongside the benchmark reports.
