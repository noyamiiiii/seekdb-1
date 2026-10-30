# Query worker host validation (2026-10-30)

This candidate inherits the V5 repair and changes only scheduling in the single-query CAGRA adaptor path. The same `ob_cuvs_job` runs on a reused 32 MiB stack. Build, search, snapshot and recall settings are unchanged.

Correctness checks pass for lazy startup, thread-creation failure and retry, actual stack size, thread reuse, synchronous output visibility, exceptions/recovery, recursive-call rejection, eight concurrent clients submitting 8,000 jobs, and shutdown/repeated lifecycles. ASan, UBSan, leak checking and ThreadSanitizer pass. Both OB_BUILD_CUVS enabled/disabled versions of the actual V5 vector adaptor compile into object files with clang 17 and the locally available seekdb dependencies. This is not a complete server link or a reproduction of the organizer's frozen offline build.

Seven alternating-order rounds compare identical callbacks. Median create/join overhead is 32.418 us versus 26.824 us for the worker, a 1.209x host dispatch ratio (17.26% time reduction). This measures host scheduling only, not GPU kernel time, SQL QPS, recall, or official score. No full-score claim is made.

Run `unittest/oblib/vector/cuvs_query_worker/run.sh` for correctness checks and the host benchmark. Run the same test source with `-fsanitize=thread` for the race check. Full GPU and SQL validation remains outstanding. The previously submitted repair commit remains queued; this separate candidate is not submitted merely on the basis of host-only measurements.
