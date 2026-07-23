# Known Limitations

This page describes the known limitations of the PostgreSQL 17 0.2 release.
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
  Explicit HNSW opclasses serve L2, inner product, cosine, and L1 for `halfvec`
  and `sparsevec`, plus Hamming and Jaccard for `bitvec`. The opclass names and
  metric bindings are stable; the non-dense SQL types and HNSW on-disk format
  remain experimental.
- `pgcontext.search` is the stable single-vector retrieval surface.
  `pgcontext.query` covers dense plus full-text fusion and experimental exact
  dense+sparse RRF fusion. Named sparse ANN and typed composite execution are
  experimental surfaces with exact recheck and bounded-work contracts.
- Named dense vector registration and search-by-name selection are stable. The
  per-vector metadata functions are stable containers, but HNSW/quantization
  option semantics and full planner use are not part of the stable promise yet.
- Named sparse table-backed ANN/index serving is experimental and requires an
  explicitly attached, metric-matched HNSW index. It falls back to exact search
  when the binding is absent or stale. Internally maintained
  internally maintained multi-vector/late-interaction ANN remains outside the
  stable 0.2 surface.
- Qdrant-style payload mutation helpers and bulk point backfill APIs are stable
  SQL surfaces. Experimental backend-local build-job metadata exists for
  owner-scoped progress, cancellation, retry, abandoned-backend detection, and
  replacement after stale active-row recovery, plus synchronous `segment`/`mmap`
  runner dry-runs. Background artifact publishing and mapped HNSW serving are
  experimental and not part of the current stable SQL promise.
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
- SQL vector values may contain up to 16,000 dimensions, but the current HNSW
  format stores each densified node plus its graph links in one PostgreSQL page.
  An encoded node record is capped at 8,064 bytes; the effective indexable
  dimension therefore also depends on graph degree and layers. `CREATE INDEX`
  fails with SQLSTATE `54000` before appending an oversized record. Reduce the
  dimensions or `pgcontext.hnsw_m`; multi-page and bit-native records are not
  implemented in this format.
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

## Experimental or Unimplemented Serving Paths

- Quantized HNSW traversal is experimental: encoded scalar, product, and binary
  candidates are always exactly reranked from authoritative source vectors.
- Named sparse ANN densifies sparse values for graph traversal, then exactly
  rechecks authoritative sparse source rows. Its index records therefore share
  the documented single-page dimension/degree envelope, and the feature is not
  part of the stable V1 contract. Live post-build delta vectors are scanned
  exactly and included in `explain_sparse.scored_count`; use REINDEX or the
  documented compaction lifecycle when bounded base-graph work is required.
- Internally maintained late-interaction ANN is experimental; pgContext owns
  the token relation and maintains it in the source DML transaction.
- Typed composite execution covers dense, filtered, sparse, full-text,
  quantized, recommendation/discovery, lookup, and late-interaction adapters
  with bounded weighted or reciprocal-rank fusion.
- Mapped HNSW serving traverses immutable, checksummed graph generations in
  place and falls back safely when attachment or validation fails.
- Automatic execution telemetry covers the executor-backed `search` and
  `execute_query` surfaces. Older specialized SQL entry points that do not yet
  use the typed executor retain their existing manual cohort instrumentation.
  A bounded, nonblocking shared-memory queue persists observations in an
  independent background-worker transaction, so errors and cancellations can
  survive caller rollback. Delivery is best-effort, may duplicate, and fail-open: contention,
  shared-memory allocation/attachment failure, queue/database-slot exhaustion,
  launch failure, restart, or a collection that remains invisible for 60
  seconds can lose an observation, while a worker
  failure in the commit/acknowledgement window can duplicate one. Automatic
  rows have bounded dimensions, but their history is not automatically pruned;
  operators must define retention for write-heavy deployments.

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
