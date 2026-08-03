# Supported Features

This page is the canonical human-readable inventory of features implemented in
the current pgContext 0.2 codebase. It is grounded in the installed SQL surface,
the SQL lifecycle registry, the machine-checked capability contract, and its
focused and lifecycle tests. It does not list roadmap-only work.

The maturity labels mean:

- **Supported:** a release platform covered by the current support policy.
- **Stable:** covered by the public compatibility policy.
- **Experimental:** implemented and testable, but its API, storage format, or
  operational contract may change before promotion.
- **Internal:** a machine-checked implementation invariant rather than a public
  SQL compatibility promise.
- **PostgreSQL-native:** supplied through PostgreSQL rather than a parallel
  pgContext subsystem.

Each label applies to the named feature, not to every child feature it can
compose. When one named surface spans different maturity levels, this page uses
the least mature relevant label and explains the boundary. See the
[SQL API contract](api_reference.md) for exact signatures and the
[support policy](support_policy.md) for compatibility guarantees.

## Platform and data authority

| Feature | Maturity | Description |
|---|---|---|
| PostgreSQL 17 and 18 | Supported | pgContext builds and runs against PostgreSQL 17 and 18. PostgreSQL 17 remains the primary benchmark and deep-lifecycle target. |
| Linux release images | Supported | Release images target `linux/amd64` and `linux/arm64`; source builds remain available for supported PostgreSQL installations. |
| PostgreSQL-native authority <!-- capability:CAP-POSTGRES-NATIVE --> | PostgreSQL-native | Ordinary PostgreSQL rows remain authoritative. MVCC, transactions, ACLs, row-level security, WAL, replication, and PostgreSQL backup tools remain in force. |

## Vector representations

| Feature | Maturity | Description |
|---|---|---|
| Dense `vector` SQL surface <!-- capability:CAP-PGVECTOR-DENSE-SQL --> | Stable | `vector` supports dimensions, checked casts, L2, inner product, cosine, and L1 scoring, comparison operators, B-tree ordering, and sum/average aggregates. |
| Half-precision vectors <!-- capability:CAP-PGVECTOR-HALFVEC --> | Experimental | `halfvec` supports typmods, checked conversions, exact metrics, ordering, aggregates, and metric-specific HNSW opclasses. |
| Sparse vectors <!-- capability:CAP-PGVECTOR-SPARSEVEC --> | Experimental | `sparsevec` supports canonical sparse values, array and dense conversions, exact metrics, ordering, aggregates, and metric-specific HNSW opclasses. |
| Bit vectors <!-- capability:CAP-PGVECTOR-BIT --> | Experimental | `bitvec` supports checked bit representations, Boolean and PostgreSQL bit conversions, Hamming and Jaccard distance, bitwise aggregates, and explicit HNSW opclasses. |

See [Dense vectors and exact search](vector_search.md) and the
[metric and operator matrix](metric_operator_matrix.md) for exact score and
ordering semantics.

## Collections and source management

| Feature | Maturity | Description |
|---|---|---|
| Table-backed collections <!-- capability:CAP-COLLECTIONS --> | Stable | A collection records metadata for an ordinary PostgreSQL source table without copying or taking ownership of its rows. |
| Collection aliases | Stable | Aliases provide an atomic logical name that can be redirected to another registered collection. |
| Collection resource limits | Stable | Per-collection limits bound dimensions, vectors, points, filter nodes, result size, candidate work, query time, and index memory. |
| Named dense vectors <!-- capability:CAP-NAMED-DENSE --> | Stable | A collection can register multiple dense vector columns with independent names, dimensions, and metrics and select them by name during search. |
| Per-vector index and quantization configuration <!-- capability:CAP-VECTOR-CONFIG --> | Experimental | pgContext validates and stores per-vector HNSW, quantization, and status metadata; full build-to-scan consumption is still being completed. |
| Stable point mappings <!-- capability:CAP-POINT-MAPPINGS --> | Stable | Source keys map to stable logical point IDs. Deleting a mapping makes that point unavailable without deleting the authoritative source row. |
| Bulk point maintenance <!-- capability:CAP-BULK-POINTS --> | Stable | Bulk upsert, delete, and source-table backfill APIs validate typed ordered rowsets before mutation, execute one set-oriented statement per bounded chunk, preserve stable point IDs and input order, reject duplicates before mutation, and report progress. |
| Registered structured filters <!-- capability:CAP-FILTERS --> | Stable | A bounded Boolean JSON grammar targets registered columns and JSONB paths, renders typed SQL predicates, and binds values through parameters. |
| Registered payload mutations <!-- capability:CAP-PAYLOAD-MUTATIONS --> | Stable | Set, delete, and clear helpers update only registered source-table columns or JSONB paths and preserve PostgreSQL permissions and transactions. |
| Model-version registry <!-- capability:CAP-MODEL-MIGRATIONS --> | Stable | Collections can record embedding model name, version, dimensions, and metric for operational tracking. |
| Embedding migration tracking | Stable | Migration records track source and target model versions, bounded progress, and typed lifecycle states without performing model inference in PostgreSQL. |

See [Collections](collections.md), [Filters](filters.md), and
[Multi-tenancy](multi_tenancy.md) for the detailed contracts.

## Retrieval and query execution

| Feature | Maturity | Description |
|---|---|---|
| Canonical retrieval contracts | Internal | Core owns score ordering, representations, index/source identities, lifecycle reasons, and typed revisions; query owns candidate branch/provenance and bounded diagnostics; codec compatibility is validated through one exhaustive registry. Crate-boundary checks reject duplicate score, branch, and codec definitions. |
| Exact vector search <!-- capability:CAP-EXACT-SEARCH --> | Stable | Exact search scores explicit arrays or visible rows from registered tables and provides the correctness oracle for approximate paths. |
| Sparse search <!-- capability:CAP-NAMED-SPARSE --> | Experimental | Explicit sparse candidate arrays use exact scoring; registered sparse vectors use exact search or a validated metric-matched HNSW candidate path followed by authoritative reranking. |
| Scroll | Stable | Keyset-style scrolling returns stable point-ID pages over active, visible mappings and accepts the shared filter grammar. |
| Count and facet <!-- capability:CAP-SCROLL-COUNT-FACET --> | Stable | Count and facet operations reuse the visible point and filter plan and apply deterministic missing-value and ordering rules. |
| Grouped search <!-- capability:CAP-GROUPED-SEARCH --> | Stable | Exact dense results can be capped per registered group field with deterministic group and result ordering. |
| Recommendation search <!-- capability:CAP-RECOMMEND --> | Stable | Positive and negative points or raw vectors form an exact query while deleted or unauthorized examples are rejected. |
| Discovery and explore search <!-- capability:CAP-DISCOVER --> | Stable | Visible context points form a centroid used for deterministic diversity-oriented ranking. |
| Dense plus PostgreSQL full-text fusion <!-- capability:CAP-HYBRID --> | Stable | One dense branch and one PostgreSQL full-text branch combine through deterministic reciprocal-rank fusion with final source visibility checks. |
| Composite query execution <!-- capability:CAP-QUERY-CONSTRUCTORS --> | Stable | Typed query constructors compose dense, sparse, full-text, quantized, late-interaction, recommendation, discovery, lookup, prefetch, weighting, threshold, formula, and rerank stages under global budgets. |
| Multi-vector late interaction <!-- capability:CAP-LATE-INTERACTION --> | Experimental | A registered `vector[]` source maintains collection-owned token rows and HNSW candidates, then returns exact MaxSim reranking against the current source row. |

See [Retrieval methods](retrieval_methods.md),
[Hybrid retrieval](hybrid_retrieval.md), and
[Dense vectors and exact search](vector_search.md).

## Indexing and accelerated serving

| Feature | Maturity | Description |
|---|---|---|
| Page-native HNSW <!-- capability:CAP-HNSW-AM --> | Experimental | `pgcontext_hnsw` stores metric-bound graph pages for dense, half, sparse, and bit vectors and performs bounded candidate traversal without a silent exact fallback. |
| Filter-aware ANN <!-- capability:CAP-FILTERED-ANN --> | Experimental | Filtered search builds a bounded candidate mask, permits excluded graph nodes to remain routing connectors, and exact-rechecks visible source rows and predicates. |
| Transactional HNSW maintenance | Experimental | Source inserts, updates, deletes, and VACUUM maintain graph, delta, and tombstone state while final queries recheck the current visible source row. |
| Segmented HNSW serving and compaction | Experimental | HNSW indexes publish at most 16 immutable graph segments plus one exact start/end/generation/count-bounded active delta capped at 10,000 records. Page items and the active cursor commit in one Generic-WAL record; readers hold the metapage shared while copying mutable delta pages, preventing false count-corruption during concurrent appends. Full deltas rotate into generation-stamped segments; live rows move into ANN pages and only unresolved tombstones remain in frozen extents whose persisted record counts are verified. Scans read the overlay first, apply segment-specific retirement masks during traversal, and replay `segment graph → segment mutations` chronologically so both top-k refill and heap-TID reuse are correct. Directory saturation preflights memory and compacts only the smallest adjacent pair while relocating the active delta contiguously. VACUUM uses the same table→append lock order and rotates tombstones in configured bounded chunks. `enqueue_hnsw_compaction` captures a directory epoch and runs one retry-idempotent pair operation through the supervised lifecycle. `hnsw_segment_stats` reports fan-out, active records/blocks, immutable rows, frozen mutations, smallest-pair work, debt, parallel eligibility, and epoch. `hnsw_segment_parallel_workers` uses a reusable backend-local pure-Rust pool with cluster advisory admission, backend-side interrupt polling, cancellation broadcast, panic draining, and visible serial degradation; it conservatively preflights all segment extents against the serving budget before materialization and rechecks exact packed bytes before retaining each graph. PostgreSQL page, mapped-file, and shared-memory access remains backend-affine. Build repartitioning consumes owned graph state without cloning the full graph, while pair and full compaction enforce `maintenance_work_mem` before dangerous allocation. Mapped generation retirement is relfilenode-aware. The clean metapage and mapped identities reject older formats with a `REINDEX` requirement. |
| Quantized HNSW candidates <!-- capability:CAP-QUANTIZATION --> | Experimental | Binary, scalar, and product codes can drive mapped HNSW candidate traversal; final scores always come from authoritative source vectors. |
| Rebuildable mapped graph generations <!-- capability:CAP-REBUILDABLE-ARTIFACTS --> | Experimental | Checksummed immutable graph artifacts support validation, bounded read-only mapping, reader pins, publication, retirement, cleanup, and source-row exact rechecks. |
| Shared generation-build primitives | Internal | One pure contract owns typed jobs and artifacts, monotonic checkpoints, leases and takeover, validation, publication aliases, versioned payload-checksummed manifests, reader pins, retirement, bounded spill runs, deterministic sampling/k-means, and redacted structural findings for later HNSW, IVF, projection, graph, and certification features. Certification fixtures retain authoritative vector seeds, true 100-dimensional counter expansion, independently balanced tenant filters, and deterministic exact top-k IDs and scores for the 100k, 1M, and 10M tiers. |

The non-dense HNSW opclass names are stable SQL contracts, but their vector
types and the HNSW access-method storage lifecycle remain experimental. See
[Indexes](indexes.md) and [Rebuildable storage artifacts](storage.md).

## Operations and migration

| Feature | Maturity | Description |
|---|---|---|
| Index diagnostics and advice <!-- capability:CAP-OBSERVABILITY --> | Stable | Typed functions report index readiness, corruption, memory estimates, optimization state, vacuum advice, recall checks, build/serving counters, segmented-HNSW fan-out and compaction debt, parallel admission/degradation, and index recommendations. |
| Automatic query telemetry | Stable | Executor-backed queries offer bounded events to a PostgreSQL background worker that stores membership-filtered strategy, work, lifecycle, completion, and latency rollups without query contents. |
| Supervised generation jobs | Experimental | Owner-scoped durable jobs support planned/running/validating/publishing states, attempt-and-canonical-backend fencing, bounded leases, crash takeover, idempotent checkpoints, single-transition retries, serialized cooperative cancellation through validation, post-commit wake-up, claim-boundary and active source-delta reconciliation, payload-integrity-checked certification evidence, timezone-independent reader pins, and serialized atomic publication. Registered executors cover certification evidence and one-pair HNSW compaction bound to an owned collection's authoritative source-table index; the worker revalidates the index binding before execution and validation. A worker keeps its fenced ownership for the whole transaction even when validation or alias locking outlives the lease TTL; the lease governs takeover only between transactions. Unsupported job kinds fail closed. Disabled or saturated workers leave retryable planned work and never remove exact retrieval. Native `CREATE INDEX` is not made resumable. |
| Artifact diagnostics and cleanup | Experimental | Operators can validate, inspect, retire, and clean root-confined generated artifact files without treating them as authoritative backups. |
| pgvector coexistence and index comparison | Experimental | Read-only reports and comparisons inspect pgvector columns and ANN indexes while both extensions remain installed. |
| pgvector adoption and ownership conversion | Experimental | Fail-closed dry runs, bridge-assisted index adoption, and restricted fast or online ownership workflows support selected plain table and index shapes. |
| PostgreSQL backup, recovery, and maintenance | PostgreSQL-native | Source tables use normal `VACUUM`, `ANALYZE`, `REINDEX`, `pg_dump`, physical backups, WAL recovery, and replication procedures. Derived artifacts can be rebuilt. |

See [Operations and support](operations.md),
[pgvector coexistence](pgvector_coexist.md), and
[Migrating from pgvector](pgvector_migration.md).

## What this page excludes

This inventory excludes planned-only features. A feature moves here only when
the code, installed surface, focused test, lifecycle evidence, maturity
classification, and detailed user documentation agree. Future work remains in
the [roadmap](roadmap.md).
