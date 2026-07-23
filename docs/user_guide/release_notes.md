# pgContext 0.2.0 — Composable Retrieval for PostgreSQL

Today we are releasing pgContext 0.2.0, an Apache-2.0 PostgreSQL extension for
vector search, metadata filtering, HNSW indexing, and hybrid retrieval over the
data you already keep in PostgreSQL.

This release is for prototypes, evaluation, and controlled pilots on
PostgreSQL 17. It already provides a broad retrieval surface, including exact
and approximate vector search, filtered search, collections, hybrid full-text
retrieval, recommendation, discovery, grouping, facets, and operational
diagnostics. Some advanced index and vector-representation features are
explicitly experimental; those boundaries are described below.

## Our Thesis

Application data should not have to leave PostgreSQL just because an
application needs semantic retrieval.

The common alternative is to copy vectors and metadata into a separate vector
service. That creates another database to provision, another copy of the data
to synchronize, another authorization boundary to reproduce, and another
backup and recovery story to operate. The relational database remains the
source of truth, but retrieval runs against a delayed projection of it.

PostgreSQL source tables remain authoritative throughout pgContext's query and
index lifecycle.

pgContext takes the opposite approach:

- ordinary PostgreSQL tables remain authoritative;
- vectors and filterable metadata stay beside the application rows they
  describe;
- PostgreSQL continues to own transactions, MVCC, roles, ACLs, row-level
  security, WAL, backups, recovery, and replication;
- exact search remains the correctness oracle;
- HNSW indexes and future generated artifacts are rebuildable acceleration
  state, not a second source of truth;
- every retrieved candidate is resolved back to a visible source row and
  rechecked before it is returned.

Our goal is not to disguise a remote vector database behind SQL. Our goal is to
make PostgreSQL itself a capable retrieval engine while preserving the reasons
teams chose PostgreSQL in the first place.

## What Ships in 0.2.0

### Dense vector SQL

pgContext includes a first-class dense `vector` type with dimension typmods,
checked input, casts, accessors, arithmetic, normalization, aggregates, and
distance operators.

The dense metric surface includes:

- Euclidean/L2 distance;
- inner product and negative inner product ordering;
- cosine distance;
- L1/taxicab distance.

Vectors can be searched directly as arrays or through registered PostgreSQL
tables. Exact top-k search is deterministic and acts as the reference result
for approximate search and recall measurement.

### Collections over existing tables

A pgContext collection is metadata over an application-owned PostgreSQL table.
It does not move or duplicate that table's rows.

The collection layer includes:

- collection creation, inspection, aliases, ownership, and removal;
- named dense-vector registration with dimensions and metric selection;
- stable point IDs mapped to application source keys;
- idempotent point upsert and deletion mappings;
- bounded bulk backfill from an existing source table;
- configurable collection limits and operational status;
- model-version registration;
- embedding-migration records with bounded progress tracking;
- registered payload fields backed by ordinary columns and JSONB paths;
- checked set, delete, and clear operations for registered payload fields.

Source rows remain normal rows. Applications can continue to use SQL, foreign
keys, triggers, transactions, partitions, views, and their existing data model.

### Metadata filtering

Search, count, facets, and related collection APIs share a typed JSON filter
language over explicitly registered fields.

Filters support:

- ordinary PostgreSQL columns;
- nested JSONB paths;
- `must`, `should`, and `must_not` boolean composition;
- exact matches and match-any conditions;
- numeric and ordered ranges;
- empty/null-aware predicates;
- bounded nesting and condition counts;
- typed SQL parameters rather than interpolated values.

PostgreSQL can use ordinary indexes while materializing filter candidates.
Final results still pass PostgreSQL visibility, ACL/RLS, active-point,
predicate, and exact-score rechecks.

### Exact search and collection navigation

The stable table-backed search surface includes:

- nearest-neighbor search with optional metadata filters;
- named-vector selection;
- stable cursor-based scrolling;
- filtered and unfiltered counts;
- facet counts over registered fields;
- grouped search with deterministic per-group limits;
- recommendation from positive and negative point examples or raw vectors;
- discovery/explore search for results that are diverse relative to visible
  context examples;
- deleted-point and source-row visibility checks throughout.

Validated query constructors are also available for nearest, recommend,
discover, lookup, prefetch, weighting, thresholds, formulas, and final-rerank
plans. These constructors provide a typed client-facing plan format; complete
execution of every composite plan is part of the roadmap.

### Persisted HNSW indexing

pgContext ships a real PostgreSQL HNSW access method for dense vectors. HNSW
candidates come from graph records persisted on PostgreSQL index pages; the
implementation does not substitute fixtures or silently scan the complete
collection when an HNSW plan is selected.

The dense HNSW surface includes:

- L2, inner-product, cosine, and L1 operator classes;
- metric-bound index construction and ordered scans;
- inserts and graph rewiring;
- updates and logical deletion handling;
- VACUUM, REINDEX, restart, and WAL-aware lifecycle behavior;
- bounded candidate traversal and cancellation checks;
- scan-work counters and recall comparison against exact search;
- index memory estimates, lifecycle status, diagnostics, and vacuum advice.

Dense HNSW is implemented and usable, but remains experimental in 0.2.0. We do
not yet promise a stable on-disk HNSW format across extension versions or a
broad production workload envelope. Plan to rebuild HNSW indexes when moving
between early releases.

### Metadata-filtered approximate search

Filtered ANN combines registered PostgreSQL filters with persisted HNSW
traversal. The query path materializes a bounded set of matching heap TIDs,
uses that set as an HNSW candidate mask, keeps masked-out graph nodes available
as traversal connectors, and then rechecks the source rows and exact distances.

This gives applications one public filter language across exact search and ANN
without making a copied payload store authoritative. Filtered ANN is also
experimental in 0.2.0 while we expand workload certification and tune the
strategy across selective and broad filters.

### Hybrid retrieval with PostgreSQL full-text search

`pgcontext.query` combines dense vector retrieval with PostgreSQL full-text
search and fuses the branches using deterministic reciprocal-rank fusion.

The hybrid surface includes:

- dense semantic candidates;
- PostgreSQL `tsvector`/full-text candidates;
- branch limits and fusion configuration;
- reciprocal-rank fusion with deterministic tie handling;
- deleted-point and source-row rechecks;
- query explanation and optimization status;
- cohort and latency summaries that avoid storing vectors, payload values,
  filters, or literal query text.

Because the lexical branch is PostgreSQL full-text search, applications keep
their language configuration, text indexes, visibility rules, and transaction
semantics in the same database.

### Half, sparse, and bit vectors

0.2.0 also exposes experimental SQL types for additional vector
representations:

- `halfvec` with dimensions, typmods, numeric-array casts, exact metrics,
  aggregates, ordering, and explicit L2, inner-product, cosine, and L1 HNSW
  operator classes;
- `sparsevec` with canonical sparse input, structured construction, dense
  conversions, exact L2/inner-product/cosine/L1 metrics, aggregates, exact
  top-k, named sparse registration/search, and explicit L2, inner-product,
  cosine, and L1 HNSW classes;
- `bitvec` with dimensions, boolean-array and PostgreSQL `bit` casts,
  Hamming/Jaccard metrics, bitwise aggregates, ordering, and explicit Hamming
  and Jaccard HNSW operator classes.

These types are useful for experimentation and migration work. Their HNSW
opclass names and metric bindings are stable, while the SQL types and HNSW
on-disk format remain experimental.

### Quantization building blocks

Experimental binary, scalar/SQ8-style, and product-quantization functions are
available for encoding, reconstruction, and exact reranking of quantized
candidates against source vectors.

These are real SQL-visible algorithms, not configuration placeholders. What
does not ship yet is a production serving path that builds and traverses a
quantized HNSW index end to end.

### Sparse, multi-vector, and late-interaction experiments

The release includes several advanced retrieval building blocks:

- named sparse-vector registration and exact table-backed sparse search;
- exact dense+sparse reciprocal-rank fusion;
- exact MaxSim reranking over multi-vector inputs;
- exact table-backed `vector[]` late-interaction search;
- an experimental HNSW token-candidate path with authoritative source-vector
  reranking and deleted-point checks;
- planner diagnostics and explicit work budgets.

The late-interaction ANN path currently requires a user-maintained token
companion table. Internal transactional maintenance of that index is roadmap
work.

### Operations and observability

pgContext provides SQL-visible operational tools for understanding collections
and indexes:

- typed index and collection lifecycle status;
- query and collection telemetry counters;
- recall checks against exact results;
- index memory estimation;
- vacuum and rebuild advice;
- cohort summaries;
- automatic executor strategy, visit, candidate, recheck, budget, completion,
  lifecycle, and latency rollups;
- backend-local build progress, cancellation, retry, and stale-owner metadata;
- versioned acceleration-artifact metadata and readiness validation;
- snapshot, export, import, retirement, and rebuild primitives for generated
  artifacts.

Telemetry is intentionally bounded and designed not to retain vectors, payload
values, filters, secrets, tenant identifiers, or literal query text.

### PostgreSQL-native security and lifecycle semantics

pgContext operates inside PostgreSQL's authority boundary:

- source-table ownership and privileges remain authoritative;
- row-level security is applied when source rows are resolved;
- collection and point metadata participate in transactions and savepoints;
- rollback does not leave successful-looking catalog state behind;
- PostgreSQL visibility and deletion state are checked before returning rows;
- normal PostgreSQL backup, restore, WAL, and replication remain the recovery
  mechanisms for authoritative data;
- acceleration indexes and generated segments can be dropped and rebuilt.

### Installation and distribution

The 0.2.0 release supports PostgreSQL 17 through:

- a versioned GitHub source archive (PGXN publication to follow);
- manual source installation with `cargo-pgrx`;
- a prebuilt PostgreSQL 17 container for `linux/amd64` and `linux/arm64`;
- a local Docker Compose playground with a runnable dense HNSW and metadata
  filtering example.

See the [installation guide](installation.md) and [playground](playground.md)
for commands and a complete example.

## Feature Maturity

We use maturity labels deliberately:

- **Stable** means the SQL behavior is part of the 0.1 compatibility promise.
- **Experimental** means the implementation exists and is usable, but its
  compatibility or operational envelope may change.
- **Intentionally different** means pgContext relies on PostgreSQL or has made
  a deliberate product choice instead of copying another system's behavior.

| Capability | 0.1 status |
|---|---|
| Dense vector SQL, exact metrics, casts, and aggregates | Stable |
| Collections, point mappings, payload fields, and bulk backfill | Stable |
| Exact table search and metadata filtering | Stable |
| Scroll, count, facets, grouping, recommendation, and discovery | Stable |
| Dense plus PostgreSQL full-text hybrid retrieval | Stable |
| Named dense-vector registration and search | Stable |
| Model versions and embedding-migration tracking | Stable |
| Operational status and bounded telemetry | Stable |
| Dense HNSW access method | Experimental |
| Metadata-filtered ANN | Experimental |
| `halfvec`, `sparsevec`, and `bitvec` SQL/selected indexes | Experimental |
| Quantization helpers and exact reranking | Experimental |
| Named sparse and late-interaction advanced paths | Experimental |

## Parity Matrix Alignment

The pgvector/Qdrant parity matrix remains the source of truth for parity claims.
Every non-stable capability is repeated here so that an experimental or
deliberately different feature cannot be mistaken for stable parity.

| Capability | Parity status | What that means in 0.2.0 |
|---|---|---|
| HNSW access method | `experimental` | Dense, half, sparse, and bit metric-bound HNSW are implemented; format stability, the single-page node envelope, and broad certification remain open. |
| Filtered ANN serving | `experimental` | Persisted HNSW, candidate masks, and authoritative source rechecks are implemented; broader workload tuning remains open. |
| SQL halfvec | `experimental` | Exact SQL and stable explicit L2, inner-product, cosine, and L1 HNSW opclass names exist. |
| SQL sparsevec | `experimental` | Exact SQL and stable explicit L2, inner-product, cosine, and L1 HNSW opclass names exist; named sparse search can attach those indexes for bounded candidates and authoritative exact rerank. |
| SQL bit vectors | `experimental` | Exact Hamming/Jaccard SQL and stable explicit Hamming/Jaccard HNSW opclass names exist; Jaccard ordering is heap-rechecked exactly. |
| SQL quantization APIs | `experimental` | Binary, scalar, and product helpers plus revision-bound mapped-HNSW encoded traversal and exact source rerank are available; index-AM pages remain full precision. |
| Per-vector dense index and quantization metadata | `experimental` | Validated configuration metadata exists; complete build-and-scan consumption is planned. |
| Named sparse vectors per collection | `experimental` | Registration, exact fallback, validated HNSW binding, filters, bounded-work explain counters, exact rerank, and exact fusion exist. |
| Multi-vector and late-interaction query | `experimental` | Exact MaxSim and experimental token candidates exist; internal token-index maintenance is planned. |
| IVFFlat | `intentionally different` | IVFFlat is not implemented; retain pgvector IVFFlat, use exact search, or rebuild as HNSW. |
| PostgreSQL-native ACL, RLS, transactions, and backups | `intentionally different` | pgContext uses PostgreSQL's authority instead of recreating it in another service. |
| Rebuildable acceleration artifacts | `intentionally different` | PostgreSQL tables are authoritative; indexes and generated segments are disposable acceleration state. |

## What pgContext Is Not Yet

pgContext 0.2.0 is not a drop-in replacement for pgvector and is not claiming
broad production certification.

Important current limits:

- PostgreSQL 17 is the only supported V1 major.
- Dense HNSW and filtered ANN remain experimental.
- IVFFlat is not implemented.
- Non-dense SQL types and the HNSW on-disk format remain experimental, and
  densified node records must fit the documented 8,064-byte page envelope.
- Quantized and mapped HNSW serving remain experimental and require
  revision-bound generated artifacts plus exact source reranking.
- Named sparse ANN is experimental, explicitly attached, and densifies graph
  traversal while retaining exact sparse source rerank and exact fallback.
- Internally maintained late-interaction and typed composite execution are
  experimental and retain the documented lifecycle and budget limits.
- Automatic execution telemetry is bounded and fail-open, not an audit log;
  pending events can be lost or a committed event duplicated at documented
  worker and postmaster failure boundaries.
- Full pgvector helper, expression-index, subvector, iterative-scan/GUC,
  parallel-build, and progress-reporting compatibility is not implemented.

The expanded automatic-observability columns, visibility view, and queue-health
function ship in the 0.2.0 base install and in the supported standalone
`0.1.0 -> 0.2.0` extension update. Both fresh installation and this update
require a PostgreSQL superuser because pgContext installs an access method and
the version-pinned update repairs PostgreSQL extension-namespace catalogs. The
update preserves catalog rows, moves
the four pgContext-owned physical vector type OIDs and support functions into
the fixed extension schema without rewriting user tables, repairs the extension
namespace for dump/restore, and reclassifies historical client-written
`automatic` cohorts as `legacy_automatic` before reserving `automatic` for
internal observations. After the move, standalone applications must either use
qualified types such as `pgcontext.vector(1536)` or explicitly add `pgcontext`
to their session/role/database `search_path`; unqualified `vector` no longer
resolves under PostgreSQL's default `"$user", public` path.

A pgvector-first 0.1.0 coexist install is detected before mutation and refused
with SQLSTATE `0A000`: its public vector types belong to pgvector and must never
be moved by pgContext. Before using `DROP EXTENSION pgcontext CASCADE`, export
collection registrations and inventory every dependent view, function, and
index because CASCADE can remove all of them. Install 0.2.0 plus
`pgcontext_pgvector`, recreate registrations and application dependents, then
rebuild pgContext indexes over the unchanged pgvector columns. The upgrade
matrix proves the refusal is atomic and preserves a real pgvector-typed user
value and type OID.

See [Known Limitations](limitations.md) and the
[pgvector migration guide](pgvector_migration.md) for the detailed boundary.

## Our Vision

We want PostgreSQL to be the place where an application can combine relational
constraints, semantic similarity, lexical relevance, structured filters, and
application-specific ranking without exporting its operational truth into a
second database.

That means more than adding another distance operator. The long-term product
needs:

- capable approximate indexes across dense, sparse, half, bit, and quantized
  representations;
- one composable query model for dense, sparse, full-text, filtered,
  recommendation, discovery, and late-interaction retrieval;
- predictable work budgets and observable execution strategies;
- transactional maintenance of every derived retrieval structure;
- explicit migration and coexistence paths for existing pgvector databases;
- acceleration artifacts that can be generated, mapped, replaced, and rebuilt
  without becoming authoritative;
- PostgreSQL-native security and recovery semantics from ingestion through
  final reranking.

The intended outcome is a retrieval layer that feels native to PostgreSQL:
inspectable with SQL, governed by database permissions, recoverable with
database tools, and composable with the rest of an application's relational
model.

## Roadmap

The roadmap is dependency-ordered. Listing a feature here means it is planned,
not that it is partially promised by 0.2.0.

### Completed foundation: non-dense ANN coverage

Half-vector and sparse-vector HNSW now cover L2, inner product, cosine, and L1;
bit-vector HNSW covers Hamming and exact-rechecked Jaccard ordering. Bounded
insert, update, delete, VACUUM, REINDEX, restart, dimension, cast, plan, work,
and exact-oracle gates own the promoted operator classes.

### 2. Quantized HNSW serving

Connect binary, scalar, and product quantization to real index generation and
bounded traversal, then rerank against authoritative source vectors. The work
also includes codebook/configuration revisions, corruption handling,
replacement, recall, and serving diagnostics.

### Completed: named sparse ANN

Registered sparse vectors can bind validated metric-matched HNSW indexes, use
filtered candidate masks, and expose bounded scored/candidate/recheck work.
Authoritative exact sparse rerank, exact fallback, ACL/RLS, DML, VACUUM,
REINDEX, configuration-change, and restart gates own the experimental path.
Raw dense and sparse candidate helpers are backend-capability guarded, HOT
successors resolve through PostgreSQL's table AM before source recheck, and
scored-work counters include exact live-delta scans.

### 4. Internally maintained late interaction

Replace the user-maintained token companion table with pgContext-owned token
indexes maintained transactionally from the authoritative source `vector[]`.
This includes rollback, repair, schema-change, crash/rebuild, MaxSim, memory,
and cancellation behavior.

### 5. Composite query execution

Execute the complete typed query model across dense, sparse, full-text,
filtered, quantized, and late-interaction candidate sources. The target is one
bounded pipeline with weighted fusion, reciprocal-rank fusion, deduplication,
thresholds, formulas, reranking, deterministic ties, cancellation, and typed
errors.

### 6. Mapped HNSW serving

Serve immutable generated HNSW generations through validated OS mappings
without reading whole artifacts into memory. Generation pinning, replacement,
retirement, corruption detection, source rechecks, and bounded-copy traversal
are part of this work.

### 7. Automatic observability

Implemented with a bounded nonblocking shared-memory queue and an independent
database worker. Executor-backed retrieval records the strategy that actually
ran—visits, candidates, filters, rechecks, quantization, fallback, latency,
cancellation, and budget outcome—while excluding application data and secrets.
Delivery health is visible to `pg_monitor`; the queue is explicitly fail-open
and best-effort/may-duplicate rather than an audit log.

### 8. pgvector coexistence and migration

Provide inventory, coexistence, conversion, validation, and rollback tooling
for real pgvector databases. Planned compatibility work includes:

- dense, half, sparse, and bit representation conversion;
- HNSW and expression-index migration;
- normalization, subvectors, concatenation, and vector arithmetic helpers;
- subvector and binary-quantization expression indexes with exact reranking;
- explicit handling of iterative-scan settings and GUCs;
- parallel HNSW construction and PostgreSQL progress reporting where needed;
- dependent views, functions, prepared statements, and application-query
  inventories;
- detection of IVFFlat with retain, exact-search, or rebuild-as-HNSW plans.

IVFFlat itself remains an intentional non-goal unless measured user demand
justifies maintaining a second ANN index lifecycle.

### 9. Broader certification and distribution

Future releases will expand production certification, collection-size and
performance envelopes, long-running recovery and fuzz campaigns, PostgreSQL
15/16/18 support, operating-system coverage, native packages, additional
images, artifact signing, and release-maintenance policy.

The complete roadmap, including dependencies and promotion criteria, is in the
[product roadmap](roadmap.md).

## Compatibility and Support

PostgreSQL 17 is the only supported V1 major. PostgreSQL 15, 16, and 18 remain
future certification targets; PostgreSQL 14 remains legacy best-effort and is
not a supported 0.2.0 release target.

Source builds require Rust `1.96.0`, `cargo-pgrx` `0.19.1`, PostgreSQL 17 server
development headers, and a matching `pg_config`.

Stable SQL functions, types, operators, casts, result columns, status values,
and documented SQLSTATE categories follow semantic extension-version
compatibility. Experimental and internal objects may change while their
contracts mature.

Use [GitHub issues](https://github.com/evokoa/pgcontext/issues) for public bugs,
feature requests, and questions. Report security issues privately to
[team@evokoa.com](mailto:team@evokoa.com).

## Related Links

- [Quickstart](quickstart.md)
- [Installation](installation.md)
- [Playground](playground.md)
- [Collections](collections.md)
- [Vector search](vector_search.md)
- [Filters](filters.md)
- [Indexes](indexes.md)
- [Hybrid retrieval](hybrid_retrieval.md)
- [Operations](operations.md)
- [API reference](api_reference.md)
- [Known limitations](limitations.md)
- [Parity matrix](parity_matrix.md)
- [pgvector migration](pgvector_migration.md)
- [Product roadmap](roadmap.md)
- [Support policy](support_policy.md)
- [Rollback and repair](rollback.md)
