# V5 query-worker design

The V5 SQL query path already serializes access under g_ob_cuvs_mu, yet creates and joins a 32 MiB pthread per query. Reuse one lazy worker with the same stack size and the same ob_cuvs_job callback. Wait synchronously for completion before returning, preserving ID mapping, filters, fallback conditions, graph parameters and search settings. Keep build/snapshot/batch paths unchanged.

Reject null callbacks; handle thread creation failure without consuming the job so future calls can retry. On callback exception return failure and keep the worker usable. Destroy by signalling shutdown and joining. No graph or query-result caching is introduced.

Alternatives: leave repeated thread creation as baseline; tune graph parameters only after GPU recall experiments. This isolated change targets host scheduling overhead, not a claimed full-score speedup.

Test actual stack size, thread reuse, concurrent caller isolation, memory visibility, callback exceptions, creation failure/retry and shutdown, under sanitizers. Benchmark identical callbacks in alternating order for per-call pthread versus persistent worker. Report host overhead only; full server/GPU/recall validation remains outstanding. Publish a separate candidate branch without replacing the queued submission.
