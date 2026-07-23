# Known Issues and Fit

- PostgreSQL 17 is the only V1 target. Other majors are not supported release
  configurations yet.
- Dense HNSW and filtered ANN are implemented but experimental. Rebuild indexes
  after an incompatible on-page format update; do not assume on-disk
  compatibility.
- Sustained write throughput against an HNSW index is limited. Each index
  absorbs about 10,000 writes (`pgcontext.hnsw_delta_segment_limit`, counting
  inserts and VACUUM tombstones since its last `CREATE INDEX`/`REINDEX`) into a
  fast append-only delta segment. Past that, inserts fall back to inline graph
  splicing, measured at 1-2 rows/second on a 100,000-row index. Results stay
  correct throughout — this is a throughput limit, not a failure. By default
  the segment drains itself: the insert that finds it full compacts the
  index in place and reopens an empty segment, so the fallback is a periodic
  stall on one insert rather than a permanent cliff
  (`pgcontext.hnsw_compact_on_threshold`, on by default). Measured on a
  100,000-row 384-dimension index, that lifts sustained inserts from 1-2 to
  263 rows/second — but the insert that compacts blocks for 42.9 seconds.
  `pgcontext.hnsw_compact_on_threshold_max_mb` (default 1GB of projected
  vectors, tunable) caps which indexes an insert will compact by itself so the
  stall cannot grow without bound; above it, compaction is left to the
  operator. The default is permissive, so lower it when bounded write latency
  matters more than sustained throughput. A
  latency-sensitive writer may prefer to turn the trigger off and schedule
  compaction itself. Moving the rebuild into a background worker, so no
  statement pays for it, is the intended end state and is not implemented. `pgcontext.compact(index)` rebuilds the base
  graph from the index's own pages and reopens an empty segment, and `REINDEX`
  does the same from the heap while also reclaiming disk. Compaction leaves the
  superseded pages in place, so it
  restores write throughput without shrinking the relation, and it can only
  drop deleted rows that a preceding `VACUUM` has tombstoned. Raising the
  limit buys write headroom at
  the cost of query latency, because every scan exact-scans the delta in full
  and that cost grows with the segment size. Bulk-load-then-query workloads do
  not reach this; continuous-ingest workloads will. See the update-churn lane
  in the [benchmark](benchmarks/pgvector.md).
- The official GHCR image is published with the v0.1.0 release; PGXN and
  Homebrew follow in a future update. Until the image is available, use a local
  Compose or manual source build.
- pgContext is not a drop-in pgvector replacement. IVFFlat and several pgvector
  helper, subvector, iterative-scan, parallel-build, and GUC contracts are not
  implemented.
- Standalone native `.deb`, `.rpm`, MSI, and platform tarball packages are not
  V1 distribution methods.
- `pgcontext.query()` can return `permission denied for table _collection_points`
  for a valid, correctly-permissioned non-owner role. Collection-metadata
  resolution reads `_collection_points` directly instead of going through the
  public visibility view the rest of the retrieval path uses, so a role that
  should be allowed to query a collection can be blocked from doing so. No
  cross-tenant data is exposed by this; the effect is an over-strict denial,
  not a leak. A fix is planned; in the meantime, grant the affected role
  ownership of the collection or query as the owner role.

For full detail and repair guidance, see [Known Limitations](user_guide/limitations.md)
and [Troubleshooting](user_guide/troubleshooting.md).
