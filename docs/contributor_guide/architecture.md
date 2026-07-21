# Architecture

pgContext is a Rust workspace with pure domain crates and a PostgreSQL
integration layer. Reusable parsing, filtering, vector math, indexing, hybrid
ranking, and storage validation stay outside pgrx so they can be tested without
a database.

## Crate Map

- `context-core`: vector types, distance metrics, exact search, catalog
  newtypes (including logical `PointId`), policy constants, cursors, and stable
  error taxonomy.
- `context-build`: pure artifact/projection generation kinds, lifecycle state,
  monotonic checkpoints, cancellation, and retry transitions; it depends only
  on `context-core`.
- `context-filter`: filter AST parsing, field registration, JSONB path
  validation, and SQL predicate rendering.
- `context-hybrid`: reciprocal rank fusion and hybrid result diagnostics.
- `context-index`: pure Rust HNSW, candidate masks, quantization, reranking,
  and memory estimates.
- `context-query`: pure query IR, hard execution budgets, owned candidate and
  source-recheck ports, application strategy selection, query-plan validation,
  readiness/cancellation states, bounded diagnostics, and deterministic
  execution outcomes.
- `context-storage`: rebuildable segment artifacts, headers, checksums, mmap
  validation, atomic writes, import/export, and loader fuzz targets.
- `context-pg`: pgrx SQL facade, catalogs, PostgreSQL access checks, SPI
  integration, SQL-visible diagnostics, and custom access-method hooks.
- `context-test`: shared test fixtures and recall helpers.

`context-pg` is intentionally the largest crate because PostgreSQL SQL objects,
SPI hydration, catalogs, access-method callbacks, pgrx tests, and SQLSTATE
translation must remain at that boundary. Large production files are split by
domain responsibility into submodules or private include fragments where that
improves reviewability. File length alone does not justify another crate: a new
crate requires a different dependency surface, ownership boundary, or public
contract. `scripts/check-source-hygiene.sh` prevents reviewed large files from
growing silently.

## SQL Facade Boundary

PostgreSQL-specific code belongs in `context-pg`. External SQL arguments should
be converted into typed domain values at the boundary, and reusable crates should
return framework-free errors that the adapter maps to SQLSTATEs.
`context-pg::error` is the single mapping table for semantic `ContextError` and
`QueryError` values; pure crates contain neither PostgreSQL error types nor
SQLSTATE literals.

`scripts/check-crate-boundaries.sh` enforces these dependency, import,
transport, and filesystem boundaries from Cargo package metadata and all owned
Rust sources. Its shell smoke suite must add a failing fixture whenever a new
boundary rule is introduced.

Security-definer functions must set a safe `search_path`, fully qualify
extension catalog access, and keep source-table ACL and RLS checks in SQL paths
that expose or mutate user data.

Hybrid retrieval adapters hydrate SQL branch rows into
`context_hybrid::BranchCandidate` values before fusion. `context-pg` owns typed
hydration adapters for dense exact, dense ANN, full-text, exact sparse, and
user-provided candidate batches. Those adapters reject negative point IDs,
non-finite branch scores, missing scored-branch scores, and conflicting
source-key mappings before the batch crosses into `context-hybrid`. Source
keys, ACL/RLS checks, and final predicate rechecks stay in `context-pg`;
`context-hybrid` receives only framework-free candidate batches and deterministic
branch order.

## Query Execution Boundary

`context-query` owns synchronous application ports for candidate generation,
filter-derived logical IDs, authoritative source hydration/recheck, bounded
telemetry, and cooperative cancellation. Port values are owned so PostgreSQL
buffer pins and mmap lifetimes cannot escape an adapter. The executor checks
cancellation before and after every port call, rejects over-budget port output,
and orders final ties by logical `PointId`.

`PointId` identifies a catalog point mapping only. A heap TID, HNSW node ID,
artifact record ID, or byte offset must remain a distinct adapter type and be
translated explicitly. PostgreSQL readiness and source visibility checks remain
in `context-pg`; pure outcomes distinguish ready, rebuild-required, not-ready,
cancelled, and budget-exhausted states without embedding SQLSTATEs.

## Filter Compiler Safety

The filter compiler is a SQL-injection boundary. Field names and JSONB paths are
resolved from catalog metadata, identifiers are quoted through helper APIs, and
predicate values are passed as SPI parameters instead of interpolated into SQL
text.

## Storage and Memory

Storage artifacts are rebuildable and validated before use. Header versions,
offsets, lengths, and checksums are checked before mmap views are exposed.
Externally controlled dimensions, counts, and byte lengths must pass policy
budget checks before allocation or iteration.

## HNSW Graph Ports

`context-index` exposes synchronous `GraphRead` and `GraphWrite` contracts for
incremental, storage-agnostic HNSW access. Reads return owned metadata, node
payloads, and one requested adjacency layer at a time; no borrow into a
PostgreSQL buffer or mapped generation crosses the adapter boundary. Layer and
neighbor counts are policy-bounded before an in-memory adapter accepts them.
The adapter-owned `GraphRecordId` token is deliberately distinct from
`context_core::PointId`, a physical heap TID, `HnswNodeId`, and the legacy
`HnswPointId`. PostgreSQL pairs a graph record token with a separately validated
block/offset `HnswHeapTid`; collection adapters obtain logical `PointId` only
from authoritative source mapping under the active statement snapshot.

The in-memory adapter proves deterministic contiguous node assignment,
arbitrary-layer reads and rewires, explicit root publication, and
non-mutating rejection of invalid topology. The pure tombstone state machine
keeps unpublished nodes invisible, ready nodes traversable but recheck-only,
and tombstones traversal-only. It publishes a versioned tombstone locator and
node revision before atomically advancing complete metadata; structural node
count and tombstone count remain separate. These types do not claim live page
mutation, callback consumption, rollback cleanup, or production serving
readiness, and HNSW stays experimental until those later gates pass.

The pure `HnswGraph` constructs the hierarchy independently of those
storage ports. Platform-independent seeded level assignment is bounded to 64
layers; insertion descends upper layers and uses `ef_construction` for bounded
candidate exploration before reciprocal selection and pruning. A derived
point-ID set prevents linear duplicate scans. Full-hierarchy snapshots use a
separate versioned binary DTO and fail closed on over-policy counts, invalid
vectors or IDs, non-reciprocal edges, and disconnected induced layers. The
snapshot DTO must not be reused as a PostgreSQL page layout: the durability
phase owns page codecs, Generic WAL records, and recovery compatibility.

The exact version-two logical page and persisted mutation-descriptor roles,
root-last insertion prefixes, repair/rebuild classification, buffer lock order,
and PostgreSQL Generic-WAL atomic-unit matrix are recorded in
[HNSW Storage Mutation Contract](./hnsw_storage_contract.md). The context-pg
planner enforces PG17's four-page record ceiling with fixed-capacity,
registration-ordered actions and binds versioned node/adjacency writes to their
exact target generation and insert-only locator keys. Physical codecs, live WAL
calls, and crash replay remain later gates.

The PostgreSQL-local MVCC planner admits a tombstone only from the
`ambulkdelete` callback's dead-to-all-snapshots result. Callback collection and
locked apply are separate fixed-capacity phases. A ready node left by an abort
may remain a topology connector but cannot become an answer without a visible
source row and a finite exact source score. Reused heap TIDs are safe only after
every older binding is tombstoned; logical query results are translated to
`PointId` by the authoritative source recheck rather than by casting a TID.

## Custom Access Method

The HNSW access method is the only layer that talks directly to PostgreSQL index
AM callbacks. Keep unsafe blocks small, document every local invariant with a
`SAFETY:` comment, and prefer pure Rust graph behavior in `context-index`
before adding PostgreSQL callback wiring.

Every installed callback has a guarded raw entrypoint and a safe function; the
complete ownership, retention, and callback inventory is recorded in the
[HNSW Callback Boundary Contract](./hnsw_callback_contract.md). Callback-local
pointer capabilities are private and non-clonable, and no PostgreSQL borrow may
escape its callback.

Filtered and multi-vector strategy selection belongs in `context-query`.
PostgreSQL adapters provide row counts, filter estimates, candidate budgets,
and availability hints, then consume a typed strategy plus structured reasons
for SQL `EXPLAIN` and telemetry. `context-index` owns ANN algorithms and their
configuration, but it does not choose the application execution path.
