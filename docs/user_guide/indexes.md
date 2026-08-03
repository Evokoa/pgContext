# Indexes

pgContext adds HNSW indexing. The pure Rust
`context-index` implementation stores typed point IDs, node IDs, and graph
layers, validates HNSW parameters, and performs deterministic hierarchical
insertion. A stable seed and insertion ordinal assign a bounded level; each
insertion descends existing upper layers, explores at most the configured
`ef_construction` candidate frontier per layer, and retains reciprocal
neighbor lists bounded by `m`.

Quantized traversal and mapped HNSW serving are available for pgContext HNSW
indexes. Approximate candidates are still resolved to live heap tuples by the
PostgreSQL executor, so MVCC visibility and source ACL/RLS remain authoritative.

`m` is limited to `2..=128`. The lower bound is required because a reciprocal
graph with more than two nodes cannot remain connected with degree one.
Construction and search budgets are also rejected above the shared policy
limits before they can drive allocation or work.

Duplicate vectors are stored as separate nodes, while duplicate stable point
IDs are rejected without mutating graph state. Equal-distance graph decisions
use node ID ordering so repeated builds remain deterministic for the same
insertion order.

The pure graph exposes a versioned full-hierarchy snapshot for deterministic
roundtrips and tests. Decoding checks counts before allocation and restoring
checks contiguous node IDs, unique point IDs, dimensions, entry level,
reciprocal links, and connectivity on every induced layer. This portable pure
Rust format is separate from the PostgreSQL page codec.

The PostgreSQL access method uses a version-three metapage and typed data-page
envelopes. The metapage records dimensions, node count, entry point, stored
metric identity, `m`, and `ef_construction`. Bulk build and incremental insert
persist complete bounded hierarchy records and publish the new graph identity
only after the page writes finish. Readers reject corrupt, unsupported, or
operator-class-incompatible metadata instead of guessing a configuration.
The shared data-page envelope is continuously exercised by the bounded
`hnsw_page` fuzz target; the target only validates portable headers and never
claims live PostgreSQL page or WAL recovery coverage.

The pure graph searches from upper layers down to the base layer with the
configured `ef_search` frontier. Controlled search exposes bounded distance,
expansion, edge, and cancellation-check counters; its reusable mask is applied
only to returned points, so masked nodes can still preserve traversal
connectivity. Fixed exact-oracle fixtures verify deterministic recall and work
bounds. The experimental SQL access method remains outside the first stable
compatibility promise while indexed serving matures.

Candidate masks can restrict returned point IDs during pure HNSW search. Masked
points may still be visited as traversal connectors, but they are not returned.
An empty mask returns no results without panics. Distinct mask point IDs are
bounded by the same `10,000` point policy used by SQL recall checks; over-budget
masks fail with the `RecallBudgetExceeded` category before traversal so callers
do not silently receive partial or empty results.
HNSW search also accepts adaptive candidate pre-filters that store sparse point
sets as sorted IDs and dense point ranges as packed bitmaps. The packed
pre-filter path uses the same traversal and budget checks as ordinary candidate
masks, but avoids carrying broad dense candidate sets as transient SQL arrays.

The pure graph is covered by hierarchy/state-machine properties for arbitrary
levels and insertions, bounded and reciprocal neighbor lists, per-layer
connectivity, reusable masks including missing or logically deleted points,
duplicate handling, cancellation atomicity, deterministic snapshots, and hard
construction-work bounds.

The pure index layer also defines incremental graph read/write ports with an
owned per-node and per-layer return contract. This prevents storage adapters
from leaking PostgreSQL buffer pins or mapped-file borrows and avoids requiring
whole-graph reconstruction for traversal. The current in-memory adapter is a
contract fixture, not evidence that PostgreSQL page writes are WAL-safe or that
the experimental SQL access method is ready to serve production queries.
Both in-memory and persisted-port traversal use the selected ascending-distance
kernel for L2, negative inner product, cosine, L1, Hamming, or Jaccard. Raw inner product is
rejected before traversal because nearest-first HNSW requires its negative
ascending form. Result ties are ordered by stable point ID, and only the
bounded traversal candidate set is sorted.

Every PostgreSQL HNSW index stores its metric identity plus `m` and
`ef_construction` on the versioned metapage. Build, later inserts/rewiring, and
page traversal validate the operator class against that identity instead of
substituting L2. Session `ef_search` remains a query-time traversal budget;
changing build-shape GUCs after index creation does not change the persisted
graph configuration used for maintenance.

`context-index` also exposes a deterministic memory estimate for stored vector
payload bytes and graph-link bytes. The estimate excludes allocator-dependent
container overhead.

The shared codec crate includes binary quantization for dense vectors and the
HNSW access method serves those codes directly during candidate traversal. It
emits a core `BitVector` sign code where non-negative dimensions become `1`
and negative dimensions become `0`. The SQL API exposes
`pgcontext.binary_quantize` for the same sign-code transformation. SQL exposes
`pgcontext.rerank_quantized_candidates` as the final quantized-candidate gate:
approximate candidate order is ignored, every surviving point must supply its
original dense vector, and final SQL scores are exact metric scores against
those originals. The pure crate uses the same rerank primitive and rejects
candidates whose original vector data is missing.
Fixed recall fixtures compare binary-quantized candidate selection plus exact
rerank against exact top-k ordering.

Scalar quantization uses uniform codebooks with 2 to 256 reconstruction levels.
Values are mapped to nearest byte codes, values outside the codebook range are
clamped to the nearest endpoint, and reconstruction rejects codes that do not
fit the codebook. Product quantization uses fixed-size subvectors, one trained
centroid codebook per subvector, nearest-centroid encoding, and reconstruction
by concatenating coded centroids. HNSW trains both serving artifacts from a
bounded deterministic sample; the standalone SQL helpers remain available for
encode/reconstruct inspection and tests.

`pgcontext_hnsw` accepts validated quantization index options so operators can
record the intended candidate-encoding mode at index creation:

```sql
CREATE INDEX docs_embedding_scalar_idx
    ON docs USING pgcontext_hnsw (embedding)
  WITH (
      quantization = 'scalar',
      scalar_min = -1.0,
      scalar_max = 1.0,
      scalar_levels = 256
  );

CREATE INDEX docs_embedding_pq_idx
    ON docs USING pgcontext_hnsw (embedding)
  WITH (
      quantization = 'pq',
      pq_subvector_dimensions = 2
  );

CREATE INDEX docs_embedding_binary_idx
    ON docs USING pgcontext_hnsw (embedding)
  WITH (quantization = 'binary');
```

`quantization` accepts `none`, `scalar`, `sq8`, `pq`, or `binary`. Scalar and SQ8 modes
validate finite `scalar_min`/`scalar_max` bounds and `scalar_levels` in the
`2..=256` byte-code range. PQ mode validates a positive
`pq_subvector_dimensions` value; pgContext trains its codebooks deterministically
from authoritative index rows. Invalid options
fail `CREATE INDEX` with SQLSTATE `22023` (`invalid_parameter_value`).
These reloptions are catalog-visible configuration. Storing quantized
metadata in the HNSW metapage records the selected mode, metadata version,
scalar bounds/levels, and PQ subvector width. Packed, shared-memory, and mapped
generations use one index-bound codec-spec revision and one deterministic,
revision-bound, checksummed codec artifact per immutable segment. Every segment
artifact uses a 16-byte-aligned, fixed-stride code layout for candidate
traversal. Segment-local codebooks let bounded rotation and compaction replace
one generation without mixing artifacts inside a traversal adapter.
PostgreSQL's exact order-by recheck reads the
authoritative source value before final ranking; encoded bytes are never source
data.

Codec training uses at most 4,096 deterministic, evenly distributed segment
rows. Encoding streams through one reusable scratch row into the aligned code
section; it does not allocate one retained byte vector per indexed row. Before
reading an entire segment for first-use packing, pgContext conservatively
projects page-item copies, decoded maps/sets/records and nested adjacency,
packed topology/vectors, publication-image scratch, code rows, duplicate
codebook/artifact state, prepared scorer, and training sample against
`pgcontext.hnsw_shared_serving_budget_mb`. An
over-budget generation fails before complete-segment allocation. `bitvec`
Hamming and Jaccard opclasses currently reject every quantization reloption
with SQLSTATE `22023`; their native full-precision HNSW paths are unchanged.

`CREATE INDEX` and `REINDEX` publish only PostgreSQL pages and metapage state.
They do not eagerly create mapped files or shared-registry entries inside the
building transaction. The first committed scan trains/packs and may publish
those derived artifacts, so an aborted build cannot leak external generations.

Changing `quantization`, scalar bounds/levels, or PQ subvector width requires a
fresh index build. Use `REINDEX` (or build a replacement index and switch to it)
to train, validate, and publish one complete revision. Existing mapped/shared
readers keep their generation pinned while a replacement is published; a scan
never combines code rows from different codebook revisions. `ALTER INDEX ...
SET` records requested reloptions but does not rewrite existing code rows by
itself.

The SQL extension registers the `pgcontext_hnsw` index access method and can
create HNSW indexes on empty or populated `vector` columns. Static builds scan
the heap table, decode vector datums, use heap TIDs as graph point IDs, and
materialize the pure Rust HNSW graph during `CREATE INDEX`. Inserts publish
complete replacement topology, and VACUUM records idempotent tombstones for
callback-confirmed dead heap TIDs. Ordered scans rebuild bounded traversal state
from persisted pages, skip unpublished or tombstoned records, and never replace
a selected HNSW scan with exact record scoring. PostgreSQL heap visibility and
the public table-search recheck remain authoritative for returned rows.

The access method remains `experimental` because the V1 release does not
promise a long-term on-disk compatibility window or broad workload
certification. Bounded tests cover insert, update, delete, abort, HOT/TID reuse,
VACUUM, REINDEX, restart, forced index plans, exact-oracle ordering, and all four
dense metrics.

Non-dense operator-class names and metric bindings are stable within the PG17
SQL contract even while the access method's on-disk compatibility remains
experimental:

| Input | Metrics and opclasses |
| --- | --- |
| `halfvec` | L2 `halfvec_hnsw_ops`; inner product `halfvec_hnsw_ip_ops`; cosine `halfvec_hnsw_cosine_ops`; L1 `halfvec_hnsw_l1_ops` |
| `sparsevec` | L2 `sparsevec_hnsw_ops`; inner product `sparsevec_hnsw_ip_ops`; cosine `sparsevec_hnsw_cosine_ops`; L1 `sparsevec_hnsw_l1_ops` |
| `bitvec` | Hamming `bitvec_hnsw_hamming_ops`; Jaccard `bitvec_hnsw_jaccard_ops` |
| `int8vec` | L2 `int8vec_hnsw_ops`; inner product `int8vec_hnsw_ip_ops`; cosine `int8vec_hnsw_cosine_ops`; L1 `int8vec_hnsw_l1_ops` |
| `uint8vec` | L2 `uint8vec_hnsw_ops`; inner product `uint8vec_hnsw_ip_ops`; cosine `uint8vec_hnsw_cosine_ops`; L1 `uint8vec_hnsw_l1_ops` |

The bit opclasses use bit-aware graph metrics. In particular, Jaccard never
substitutes L2 over densified coordinates because that does not preserve result
ordering. Jaccard graph navigation remains `real` precision, but its ordered
scan value is a conservative lower bound and PostgreSQL rechecks the visible
heap value with the exact `double precision` operator before final ordering.
End-to-end tests compare every pair with a forced exact oracle, assert the
metric-specific index plan, and require candidate work below collection
cardinality.

Integer opclasses densify coordinates losslessly only for graph navigation.
That floating-point navigation score is never final authority: ordered scans
request PostgreSQL heap recheck for every candidate and compute the exact
`int8vec` or `uint8vec` operator score from the live source value. Immutable
embedding profiles bind the provider revision to one fixed-typmod source column
and its live metric-matched HNSW index. Registration rejects a mismatched type,
dimension, column, access method, opclass, expression/partial index, or stale
index state. `pgcontext.embedding_profile_explain` reports the binding and
revalidates it against PostgreSQL catalogs; profile-aware query/import
constructors reject stale bindings before accepting provider values.

The SQL vector types accept up to 16,000 dimensions, but this experimental
HNSW format stores each densified node and its graph links in a single page.
The encoded record ceiling is 8,064 bytes, so the effective indexable dimension
also depends on `hnsw_m` and the node's layers. Oversized builds fail with
SQLSTATE `54000`; reduce dimensions or `pgcontext.hnsw_m`. A future bit-native
or multi-page record format may raise this index-specific ceiling.

## IVFFlat

`pgcontext_ivfflat` is an experimental native access method with PostgreSQL
page, WAL, MVCC, ACL/RLS, VACUUM, REINDEX, concurrent-build, partition,
backup/restore, and replication behavior. Its centroids, list directory,
postings, and optional codec pages are rebuildable; the source table remains
authoritative and PostgreSQL rechecks every order-by value from that source.

```sql
CREATE INDEX documents_embedding_ivf
    ON documents USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_cosine_ops)
    WITH (lists = 100);

SET pgcontext.ivfflat_probes = 10;
SELECT id
FROM documents
ORDER BY embedding OPERATOR(pgcontext.<=>) $1
LIMIT 20;
```

Continuous vector types (`vector`, `halfvec`, `int8vec`, and `uint8vec`) expose
L2, inner-product, cosine, and L1 opclasses. `bitvec` exposes explicit Hamming
and Jaccard opclasses. Sparse IVFFlat is not advertised. `lists` is in
`1..=32768`; a populated build uses at most the number of indexed rows.

Quantized posting codes are selected per index:

```sql
CREATE INDEX documents_embedding_ivf_sq8
    ON documents USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_cosine_ops)
    WITH (lists = 100, quantization = sq8);

CREATE INDEX documents_embedding_ivf_pq
    ON documents USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_cosine_ops)
    WITH (lists = 100, quantization = pq, pq_subvector_dimensions = 8);
```

`quantization` accepts `none`, `sq8`, or `pq`. PQ subvector dimensions must
divide the source dimensions. SQ8/PQ use the shared deterministic codec and
checksummed codebook format; they narrow candidates but never replace exact
source scoring. Hamming and Jaccard reject SQ8/PQ with SQLSTATE `22023`.

Search policy is bounded by `pgcontext.ivfflat_probes`,
`pgcontext.ivfflat_max_probes`, and
`pgcontext.ivfflat_candidate_budget`. Iterative mode `off` reads the requested
probe count. `strict_order` materializes and globally orders the complete
bounded frontier through `max_probes` before returning its first tuple;
`relaxed_order` starts at the requested probes and widens lazily only when the
executor asks for more tuples. It exact-rechecks every returned source row, but
uses approximate queue keys so ordering may differ from exact source order both
within a batch and across later batches, especially for SQ8/PQ or stale
foreground state. Materialize and re-sort by the distance expression when final
global order is required. Relaxed widening visits each list and foreground delta
record at most once and charges every visit to one scan-global hard candidate
budget. `ivfflat_probes` must not exceed `ivfflat_max_probes`.

Use `pgcontext.ivfflat_index_info(index_regclass)` to validate page checksums,
extents, codec revision, list occupancy, and generation state. Use
`pgcontext.ivfflat_last_scan_work()` immediately after a scan to inspect lists,
postings, deltas, candidates, exact reranks, widening rounds, completion reason,
codec, and generation. `pgcontext.compact_ivfflat(index_regclass)` retrains and
atomically publishes a generation from live source rows, folds foreground
deltas, and retires reusable superseded pages. Collection owners can enqueue
the same operation through `pgcontext.enqueue_ivfflat_compaction`; registered
indexes enqueue debt automatically at 10,000 delta records. Unknown or earlier
experimental formats fail closed; `REINDEX` rebuilds the clean v4 format and is
still required after reloption changes, format changes, or corruption.

The retained [one-million-row v3 correctness run](../benchmarks/ivfflat_1m.md)
documents historical bounded-work evidence only. It does not certify the v4
format or provide release-performance evidence.

## HNSW Settings

HNSW tuning uses PostgreSQL GUCs with defaults checked against shared
`context-core` policy constants:

| Setting | Default | Purpose |
| --- | ---: | --- |
| `pgcontext.hnsw_m` | `16` | Maximum retained HNSW neighbors per node. |
| `pgcontext.hnsw_ef_construction` | `64` | Build-time candidate budget. |
| `pgcontext.hnsw_ef_search` | `32` | Search visit budget for pure HNSW traversal. |
| `pgcontext.hnsw_candidate_budget` | `32` | Default candidate budget for filtered or iterative HNSW search. |
| `pgcontext.hnsw_iterative_expansion_limit` | `10000` | Maximum candidate batch size for iterative HNSW recheck. |
| `pgcontext.hnsw_recall_threshold` | `0.95` | Default minimum recall target for approximate HNSW health checks. |
| `pgcontext.hnsw_mmap_serving` | `on` | Publish full-layer packed generations as immutable, physical-index-bound files and attach them before falling back to the shared registry or PostgreSQL pages. Corrupt, stale, missing, or over-budget files fail closed to the fallback ladder. |
| `pgcontext.hnsw_mmap_serving_budget_mb` | `512` | Maximum encoded bytes for one mapped generation. Publication and attachment above this limit are skipped without failing the query. |
| `pgcontext.hnsw_shared_serving` | `on` | Publish packed HNSW graph generations to a shared registry so other backends attach instead of rebuilding. |
| `pgcontext.hnsw_shared_serving_budget_mb` | `512` | Total shared-registry bytes across all indexes; a publish that would exceed this is skipped. |
| `pgcontext.hnsw_pack_on_first_use` | `on` | When off and no pack is available anywhere, serve queries from unpacked directory reads instead of paying a full pack inline. |
| `pgcontext.hnsw_mask_candidate_limit` | `10000` | Maximum distinct point IDs a visibility/filter-aware HNSW scan (`pgcontext._hnsw_masked_candidates` and named sparse collection search) accepts as its candidate mask. Independent of `pgcontext.hnsw_iterative_expansion_limit`; raise it to serve larger caller-visible or filtered result sets through the masked scan instead of falling back to exact search, or set it to `0` to select exact fallback for masked named retrieval. |
| `pgcontext.hnsw_build_parallel_workers` | `1` | Threads used to construct the in-memory HNSW graph during `CREATE INDEX`/`REINDEX`. `1` (default) builds single-threaded and deterministic. Raising this parallelizes graph construction across threads in the building backend using per-node locking; the resulting graph is structurally valid but not bit-identical to a sequential build of the same rows. Measured 2-3.5x faster at 2-8 workers on a 20k-row/384-dim corpus. |
| `pgcontext.pgvector_compat_warnings` | `on` | With the main-extension pgvector binding enabled, emit one advisory `NOTICE` per backend and index serving a pgvector-owned column, recommending `pgcontext.migration_report()`. See [pgvector_coexist.md](pgvector_coexist.md). |
| `pgcontext.hnsw_delta_segment_limit` | `10000` | Maximum records in the active segmented-write delta; valid range `1..10000`. Inserts append with the page item and metapage cursor in one WAL record. A full delta rotates into a generation-stamped immutable graph segment; at the 16-segment directory bound, only the smallest adjacent pair is memory-admitted and compacted. VACUUM tombstones use the same bounded chunking. |

Mapped generations live below the physical database directory in one numeric
directory per index. PostgreSQL therefore removes them with `DROP DATABASE`;
committed `DROP INDEX` and cascading `DROP TABLE` reclaim the exact index
directory after transaction commit, while rolled-back DDL leaves it intact.
Each drop first publishes a small fsynced transaction marker through a durably
linked directory hierarchy. Later HNSW scans reconcile one of 16 marker buckets
and at most 16 entries apiece. Per-bucket pending and retry lanes let terminal
work drain before unresolved prepared transactions are retried, so a large
prepared queue or a stale pre-rename temp cannot starve committed cleanup.
Committed drops are retried after backend crashes or transient filesystem
errors, and aborted drops preserve the live index generation. Temporary indexes
use only the backend-local packed cache and never publish a filesystem
generation.

Set these with `SET LOCAL` inside controlled build or validation sessions, then
validate approximate paths with `pgcontext.recall_check` before any controlled
rollout. Do not route production traffic to HNSW until approximate serving is
validated for your workload's recall, latency, and correctness targets.

## Index Status

Use `pgcontext.index_status(index_name)` to inspect PostgreSQL catalog status
for an index:

```sql
SELECT index_schema,
       index_name,
       table_schema,
       table_name,
       access_method,
       is_valid,
       is_ready,
       is_live,
       status
FROM pgcontext.index_status('public.docs_embedding_idx');
```

The `status` column is the typed enum `pgcontext."IndexLifecycleStatus"` with `Ready`,
`Building`, or `Invalid`. Missing indexes use SQLSTATE `42704`
(`undefined_object`).

Use `pgcontext.index_diagnostics(index_name)` when an operator needs a typed
serving decision plus repair advice:

```sql
SELECT status,
       context_error,
       sqlstate,
       repair_advice
FROM pgcontext.index_diagnostics('public.docs_embedding_idx');
```

The diagnostic `status` is `Ready`, `IndexNotReady`, `IndexCorrupt`, or
`UnsupportedAccessMethod`. Not-ready and corrupt pgContext serving indexes
include the stable context error name and SQLSTATE.

Use `pgcontext.index_advisor(collection)` to inspect ordinary PostgreSQL index
gaps for registered filters:

```sql
SELECT filter_key,
       column_name,
       recommendation,
       detail,
       suggested_sql
FROM pgcontext.index_advisor('docs');
```

The advisor can recommend B-tree indexes for ordinary filter columns, GIN
indexes for JSONB filter columns, `ANALYZE` for stale statistics, and HNSW
settings review when no pgContext HNSW index is present.

## Index Memory Estimate

Use `pgcontext.estimate_index_memory(index_name)` to inspect the projected
in-memory search payload for a pgContext index:

```sql
SELECT index_schema,
       index_name,
       table_schema,
       table_name,
       access_method,
       estimated_rows,
       dimensions,
       vector_bytes,
       link_bytes,
       total_bytes,
       status
FROM pgcontext.estimate_index_memory('public.docs_embedding_idx');
```

For `pgcontext_hnsw`, `vector_bytes` projects dense `f32` payload bytes and
`link_bytes` projects retained graph-neighbor identifier bytes from PostgreSQL
index row estimates and an observed non-null indexed vector. The estimate
excludes allocator-dependent container overhead and is not the on-disk relation
size. `status` is the typed enum `pgcontext."IndexMemoryEstimateStatus"` with `Projected`,
`UnsupportedAccessMethod`, or `UnavailableStatistics`. Missing indexes use
SQLSTATE `42704` (`undefined_object`).

## Build Memory Budget

`CREATE INDEX ... USING pgcontext_hnsw` builds the graph in backend memory and
enforces PostgreSQL's `maintenance_work_mem` as a hard budget. When the
estimated build memory exceeds the budget, the build stops with SQLSTATE
`22023` and a `HINT` carrying a suggested setting instead of silently
spilling:

```text
ERROR:  HNSW build estimated memory 67475472 bytes exceeds maintenance_work_mem budget 67108864 bytes after 98304 indexed vectors
HINT:  Raise the build budget for this session, for example SET maintenance_work_mem = '97MB', then retry CREATE INDEX.
```

Size the budget before building: roughly
`rows × (dimensions × 4 bytes + m × 16 bytes)` plus headroom, or run
`pgcontext.estimate_index_memory` against an existing index of the same shape.
PostgreSQL's default 64MB budget caps out near 100,000 384-dimensional
vectors. Set the budget for the build session only:

```sql
SET maintenance_work_mem = '2GB';
CREATE INDEX docs_embedding_hnsw ON docs
    USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
RESET maintenance_work_mem;
```

pgvector degrades to slower incremental insertion when its in-memory phase
fills; pgContext currently refuses instead so the budget stays honest. A
within-budget streaming build path is tracked on the roadmap.

## Vacuum Advice

Use `pgcontext.vacuum_advice(index_name)` to inspect PostgreSQL-visible
maintenance signals for an index and its owning table:

```sql
SELECT index_schema,
       index_name,
       table_schema,
       table_name,
       access_method,
       estimated_index_tuples,
       index_pages,
       dead_table_tuples,
       status
FROM pgcontext.vacuum_advice('public.docs_embedding_idx');
```

For `pgcontext_hnsw`, the advice uses catalog tuple/page estimates and dead heap
tuple statistics, so maintenance recommendations follow PostgreSQL statistics
collection timing. Callback-confirmed tombstone revisions are excluded from
query candidates immediately; delayed statistics affect recommendation timing,
not result correctness. `status` is the typed enum `pgcontext."VacuumAdviceStatus"` with
`Healthy`, `VacuumRecommended`, `AnalyzeRecommended`, or
`UnsupportedAccessMethod`. Missing indexes use SQLSTATE `42704`
(`undefined_object`).

## Recall Check

Use `pgcontext.recall_check(exact_point_ids, candidate_point_ids, min_recall)`
to compare an approximate or filtered candidate set with exact point IDs:

```sql
SELECT exact_count,
       candidate_count,
       intersection_count,
       recall,
       status
FROM pgcontext.recall_check(
  ARRAY[10,20,30]::bigint[],
  ARRAY[20,30,40]::bigint[],
  0.95
);
```

Duplicate point IDs are counted once. `status` is the typed enum
`pgcontext."RecallCheckStatus"` with `Passing`, `Failing`, or `EmptyExact`. Invalid
`min_recall` values and negative point IDs use SQLSTATE `22023`
(`invalid_parameter_value`). Each input array is limited by the core
`MAX_RECALL_CHECK_POINT_IDS` policy, currently `10,000`; larger recall checks
fail with SQLSTATE `54000` (`program_limit_exceeded`) before set construction.
The measured counters are the returned `exact_count`, `candidate_count`, and
`intersection_count`. `pgcontext.explain` exposes the current recall-check
budget as a `recall_budget` stage, and candidate counts can be recorded through
`pgcontext.record_query_stat` for query-cohort diagnostics.
