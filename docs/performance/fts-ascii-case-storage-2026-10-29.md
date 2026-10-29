# FTS ASCII processing and case-conversion storage

This follows `fb6e49745be61f03d2ae10e022f882c6cf58c33b`. The latest official
feedback belongs to `1161f8f0b845574b10f1b62d8d4caf028c5082e5`, submitted
2026-10-29 at 13:14 Asia/Shanghai, with a 47.3707567% aggregate reduction.
It is not feedback for the later merged changes.

## Implementation

The BENG parser first checks whether the document consists entirely of ASCII
bytes and uses the UTF-8 charset. For those documents, delimiter scanning,
alphanumeric trimming, lowercasing and copying use one direct parser path.
Delimiter and alphanumeric classification still use the same charset tables.
Tokens are copied into allocator-owned blocks and remain valid independently
of the input or the parser. Other documents use the existing tokenizer and
normalizer, preserving non-ASCII and invalid-input behavior.

`ObAddWord` obtains the case-conversion charset once and allocates converted
words from 4 KiB blocks, or a larger block for an individual large word. The
original charset case-conversion functions and expansion multipliers remain
in use, including the original in-place path for multiplier one. Every result
still owns its converted bytes for the lifetime of the caller's allocator.

A three-worker FTS parallelism experiment was measured and rejected because
its median aggregate improvement over the preceding candidate was only 0.12%.
The retained code keeps the existing two-worker minimum.

## Validation

The release build and 18 native tests pass. Tests compare the ASCII path with
the original generic pipeline across byte values, supported charsets, long
tokens and parser resets. Case-conversion tests compare with `ObCharset::tolower`
and retain earlier words while subsequent allocations cross block boundaries.

The candidate preserves all 2,096 captured complete SQL outputs against the
original pre-optimization version, passes 38 query-boundary cases, processes
160 distinct documents through eight concurrent sessions, and passes the three
added posting-union/aggregate/LIMIT mysqltests plus 12 existing FTS suites.
Benchmark data, SQL, timing code, grading code and baseline files are unchanged.

## Six-pair local results

The same machine and 2 GiB instance run the unchanged full benchmark in
`AB BA AB BA AB BA` order, using original pre-FTS sources at `6263dfd3` as A.
All 12 reports and all outliers are retained.

| Category | Reduction using mean latencies |
| --- | ---: |
| Index build | 52.1756% |
| Tokenization | 26.6331% |
| Queries | 80.8701% |
| Three-category aggregate | **53.2262%** |

Using median latencies, the three-category aggregate is **50.5128%**. The first
baseline BENG run has a 0.2513 ms sample mean and raises the aggregate mean;
it is retained rather than discarded. `margin.json` also reports a paired
bootstrap interval to expose the uncertainty. Local cross-version results do
not certify the official fixed-baseline score or guarantee full marks.

See [all evidence](data/fts-ascii-case-storage-2026-10-29) for raw runs,
validation, the rejected parallelism experiment and the official feedback.
