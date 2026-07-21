# Known Limitations

This page describes the known limitations of the PostgreSQL 17 V1 release.
Extended production certification and unimplemented product behavior live in
[`roadmap.md`](roadmap.md).

## SQL Surface

- SQL `halfvec`, `sparsevec`, and `bitvec` wrappers are experimental. They cover
  text input/output, typmods, dimensions, exact distance helpers, and distance operators.
  `halfvec` also covers explicit rounding numeric-array casts and aggregates; `sparsevec` covers
  structured construction, dense `real[]`/`vector` casts, aggregates, and exact top-k
  over explicit arrays or registered sparse source columns via
  `pgcontext.search_sparse`; `bitvec` covers `boolean[]`, PostgreSQL `bit` and
  `bit varying` casts, pgvector-compatible built-in `bit` distance
  functions/operators, bitwise OR/AND aggregates, and btree ordering opclasses.
  L2 HNSW index classes for `halfvec` and `sparsevec` are experimental, and
  explicit Hamming HNSW indexing is available for `bitvec`. Non-dense ANN
  promotion is not part of the current stable SQL promise; it is documented in
  the post-V1 roadmap.
- `pgcontext.search` is the stable single-vector retrieval surface.
  `pgcontext.query` covers dense plus full-text fusion and experimental exact
  dense+sparse RRF fusion. ANN sparse and multi-branch planners are post-V1
  roadmap features.
- Named dense vector registration and search-by-name selection are stable. The
  per-vector metadata functions are stable containers, but HNSW/quantization
  option semantics and full planner use are not part of the stable promise yet.
- Named sparse table-backed ANN/index serving and internally maintained
  multi-vector/late-interaction ANN are post-V1 roadmap features.
- Qdrant-style payload mutation helpers and bulk point backfill APIs are stable
  SQL surfaces. Experimental backend-local build-job metadata exists for
  owner-scoped progress, cancellation, retry, abandoned-backend detection, and
  replacement after stale active-row recovery, plus synchronous `segment`/`mmap`
  runner dry-runs. Background artifact publishing is not part of the current
  stable SQL promise; mapped HNSW serving is a post-V1 roadmap feature.
- `pgcontext.count` counts active table-backed point mappings, optionally with
  the same registered-field filter JSON accepted by filtered search and facets.
- Collection strict-mode limits are available for catalog and query guardrails,
  but timeout, filter-node, and index-memory fields are policy metadata until
  later planners and diagnostics consume them directly.

## Indexes And Recall

- `pgcontext_hnsw` is experimental. It is SQL-visible for access-method work but
  not yet covered by the production compatibility promise.
- The experimental HNSW on-page record format is intentionally not backward
  compatible during construction. Recreate an HNSW index after upgrading
  pgContext to a version whose on-page format differs.
- Four-metric dense HNSW has bounded exact-oracle, VACUUM, REINDEX,
  crash/restart, replica-promotion, concurrency, filtered-ANN, and work-budget
  coverage. It remains experimental because V1 does not promise a stable
  on-disk compatibility window or broad production workload certification.
- Sustained writes to an HNSW index degrade once its delta segment fills.
  Inserts are absorbed into a bounded append-only delta segment rather than
  spliced into the graph, which is what keeps incremental writes fast. The
  segment holds `pgcontext.hnsw_delta_segment_limit` records (default 10,000,
  counting inserts and VACUUM tombstones appended since the index was last
  built). Once full, inserts revert to inline graph splicing, whose cost is
  O(graph size) — the update-churn lane measures 1-2 rows/second on a
  100,000-row index, degrading as the index grows.

  Correctness is unaffected: scans merge the delta with the base graph, and the
  inline path is the same graph-splice path used when the delta region is
  disabled. The limitation is throughput.

  By default it is self-correcting, within a bound. The insert that finds the
  segment full compacts the index in place — rebuilding the base graph from the
  index's own pages and reopening an empty segment — so writes return to the
  fast path instead of degrading permanently.

  The cost moves to that one insert, and it is not small. Measured on a
  100,000-row 384-dimension index: 12,000 single-row inserts completed at 263
  rows/second overall, of which one insert took 42.9 seconds (the compaction)
  and the rest were sub-millisecond. Sustained throughput is roughly two orders
  of magnitude better than the 1–2 rows/second inline fallback, but it is
  bought with one long stall per drained segment.

  Because that stall grows with the index,
  `pgcontext.hnsw_compact_on_threshold_max_mb` (default 1GB of projected
  vectors, about 700,000 rows at 384 dimensions) caps which indexes an insert
  will compact by itself. The default is deliberately permissive so ordinary
  workloads keep self-maintaining, which means it admits stalls of several
  minutes on the largest index it accepts; lower it when bounded write latency
  matters more than sustained throughput, and note that `maintenance_work_mem`
  applies independently and is often the tighter limit. Above it the insert declines and takes the inline
  path, leaving the rebuild to `pgcontext.compact()` or `REINDEX` where the
  cost is expected rather than a surprise. Compaction also declines when it
  cannot take the parent-table lock without waiting, or when the rebuilt graph
  would exceed `maintenance_work_mem`. Set
  `pgcontext.hnsw_compact_on_threshold = off` to keep inserts uniformly cheap
  and schedule compaction yourself.

  Running the rebuild in a background worker, so no statement pays for it, is
  the intended end state; the synchronous path plus this bound is the interim
  answer.

  Raising `hnsw_delta_segment_limit` defers the fallback but slows every query,
  because scans exact-scan the whole delta region and that cost grows linearly
  with the segment size; the default balances those two costs. Workloads
  that bulk-load, build the index, then mostly read never approach the limit.
  The update-churn lane in the [benchmark](../benchmarks/pgvector.md) records
  both the pre-compaction measurements and the re-measurement above.

## Unimplemented Serving Paths

- Complete non-dense ANN metric coverage is not implemented. Only the
  experimental variant opclasses described under SQL Surface exist.
- Quantization helpers do not provide quantized HNSW build or serving.
- Named sparse vector search is exact; named sparse ANN is not implemented.
- Late-interaction ANN is not internally maintained and requires an
  experimental user-managed token companion table.
- Typed composite query structures do not yet execute every advanced candidate
  source as one pipeline.
- Artifact and mapping helpers do not perform memory-mapped HNSW graph
  traversal.
- Query telemetry does not yet automatically record the complete execution
  strategy and outcome for every serving path.

## Lifecycle And Operations

- PostgreSQL 17 is the only supported V1 major. PostgreSQL 15, 16, and 18
  remain planned support targets until their version-specific gates pass.
- Bounded PG17 dump/restore, WAL replay, fresh-install, extension-drop,
  packaged-source, and multi-architecture image gates pass.
  Long-duration certification remains post-V1 work.
- Segment artifact import/export/rebuild is not a stable SQL surface.
  Rebuildable artifacts are treated as acceleration state, not primary data.
- Multi-tenancy patterns use PostgreSQL source-table ownership, ACL/RLS,
  tenant filter registrations, partitioning, per-tenant recall checks, and
  cohort telemetry as documented in the multi-tenancy runbook.
- Cross-version artifact compatibility is limited to the documented segment
  format version window. V1 segment fixture import and future-version rejection
  paths are covered by tests; broader upgrade/rollback artifact migration
  remains planned until there is more than one readable segment format.

## Security And Telemetry

- SQL-visible security tests cover ACL/RLS boundaries for implemented paths.
  The bounded PostgreSQL 17 hostile `search_path`, injection, telemetry privacy,
  supply-chain, and independent unsafe/FFI reviews passed; extended sanitizer
  and hostile-input campaigns are post-V1 certification.
- Telemetry is intended to store counters and typed statuses, not vectors,
  payload values, literal query text, filters, secrets, or other free-form user
  text.
