# FTS query and token storage follow-up

This change continues from commit `59e63ecf10cc563154e28bf18fc127d8df45297c`,
whose official result is 46.170 points and 46.169658% mean latency reduction.
The target is at least 50% mean latency reduction under the official rule:
average the three build reductions, the two tokenization reductions, and the
four query reductions separately, then average those three categories equally.
Individual regressions remain negative contributions.

## Changes

For small unscored natural-language posting unions, the merge specializes its
one-, two-, and three-term loops. Fixed 16-byte binary IDs use complete-word
loads and byte swaps for portable binary ordering, and an inline owned copy
where a copy is required. The posting saver tests whether a batch has any
skipped rows before entering its copying loop. The binary string-difference
decoder precomputes the common bytes and changed positions once per batch;
16-byte output occupies 16 bytes per row instead of the generic 128-byte stride.
Packed, variable-length, non-16-byte, and unsupported cases retain their paths.

Vector projection can reference the posting block's owned ID bytes directly.
It stops a returned batch before refilling any child block referenced by that
batch. A pending-advance mask is applied only on the following call, after the
consumer has consumed the preceding batch. Reset and reuse clear that state.
This keeps real document IDs and their original ordering; it does not replace
them with synthetic IDs or shortcut the union cardinality. Scalar projection,
numeric relevance, threshold predicates, ranking, and other retrieval modes
retain their normal semantics.

IK processing calls its four known concrete processors in the original order.
Character-context accessors are inline. Immutable optional BMP range and DAT
root-transition tables replace repeated range searches and transitions;
unsupported characters and allocation failure use existing lookup logic.
The generic dictionary factory exposes an already-published built-in UTF-8 IK
loader through an acquire/release pointer, keeping the map's owning reference
and each caller's normal loader handle.

BENG stores its existing whitespace tokenizer and basic normalizer in the
parser object and copies returned tokens into owned blocks of at most 4 KiB,
except for an individual larger token. Tokens remain valid after another
normalization, parser reset, and changes to the input. Canonical built-in
TOKENIZE names use immutable versioned strings after normal validation; a
compile-time assertion guards a change to the built-in version.

Full-text index row generation reuses one bounded worker-local word map. A
lease clears every word on exit, and nested calls use a private map. Cached
storage holds buckets and an allocator block, never documents or tokenization
results across calls. Default TOKENIZE JSON output allocates its string nodes
as one block while preserving map iteration order and the existing JSON tree
serialization.

## Validation

The candidate release build and 17 native tests pass. Coverage includes every
valid Unicode scalar, malformed UTF-8, all 65,536 16-byte difference layouts
with and without NULL rows, unaligned owned IDs, parser reset, mixed-case and
large BENG tokens, comparison with the original generic English pipeline, and
concurrent built-in dictionary handles.

All 2,096 complete TOKENIZE and scored MATCH outputs match the captured
control exactly, including token order, frequency, IDs, and rounded numeric
scores. The 38 aggregate, score, threshold, LIMIT, NULL, and prepared-plan
checks pass after analyzing the same benchmark table. Index statistics must
be refreshed before numeric-score comparisons; stale estimates can otherwise
produce different BM25 constants without changed postings.

The 160-document test through eight concurrent sessions, the aggregate-score
and cardinality/LIMIT mysqltests, and a new 2,002-document posting-union test
pass. The new test crosses batch boundaries and covers one to four terms,
duplicate terms, empty postings, changing prepared parameters, and DML.

The 12 relevant existing FTS mysqltests pass, with `drop_index` run separately
after restarting the instance. Its earlier run after other suites showed only
row-order differences in system-column queries that have no ORDER BY; both
the unchanged old executable and the restarted candidate pass the unchanged
expected result. The earlier failure is retained in the evidence.

## Measurement policy

Measurements use unchanged official data, SQL, timing, scoring code, and
baseline files: 20,000 rows, batch 500, 3,000 tokenization rounds, 200 query
rounds, three samples, 30 warmups, and a complete reload. Hardware, a 2 GiB
instance, clients, and release settings are shared. Six pairs alternate
`AB BA AB BA AB BA`; every report and outlier is retained. Means and medians
are both reported.

The original zero-scoring submission `73a17c4` already contained the first
character-classification and dictionary optimizations. Against that version,
the complete latest six-pair series measured 49.1530% mean reduction and
48.6514% using median latencies. These results do not establish the requested
50% threshold. A separate build of the original pre-FTS-optimization sources
at `6263dfd3c83c7569937368bdd0d45ddc9cb382f8` provides the appropriate original
implementation comparison. Its results must be recorded separately, with the
different reference explicitly identified.

The checked-in historical CI baseline reports full marks even for much older
executables on this faster local machine. Its local score is not used as proof
of meeting the contest threshold. Official full marks require the official
evaluation; no local comparison is represented as an official result.

## Final original-version comparison

The final merged release passes the same 17 native tests, 2,096 exact SQL
comparisons, 38 query checks, eight-session concurrency test, three additional
mysqltests, and 12 existing FTS mysqltests. It includes both upstream follow-ups
through `1161f8f0b845574b10f1b62d8d4caf028c5082e5`.

The six complete pairs against the pre-optimization original measured
**48.6180% aggregate mean reduction**. The calculation using median latencies
gave **47.8723%**. A paired bootstrap interval from these six local pairs is
40.3607% to 52.6654%. Neither the complete means nor medians establish the
50% threshold; the earlier three-pair estimate of 52.11% was preliminary and
is superseded by the full series. No samples or outliers were dropped.

All final raw reports, complete benchmark output, comparison summaries and
validation evidence are under
[data/fts-query-and-token-storage-2026-10-29](data/fts-query-and-token-storage-2026-10-29).
The official fixed-baseline score is pending.
