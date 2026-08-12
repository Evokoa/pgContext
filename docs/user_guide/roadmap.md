# pgContext Product Roadmap

This document describes pgContext's product direction and release-engineering
plans following the installable GitHub V1 launch.

It outlines broad product direction, not an implementation checklist. Items
marked complete are implemented for 0.2.0; remaining items describe later work.

A feature listed here remains "planned" until its public capability row and
release notes announce its arrival. The document began as a post-V1 plan and
now records completed 0.2 work and remaining direction. A roadmap item only becomes release-blocking once it is
selected into a future dependency-ordered build or release plan.

The product goal is a PostgreSQL-native AI search engine into which an
application can load ordinary rows, text, dense/sparse/multi-vectors, and
metadata, query them immediately through an exact path, and gain faster
retrieval as derived indexes become ready. Source rows and their registered
vector representation remain authoritative. A provider-native integer or bit
embedding may itself be that source representation; separately generated
index codes remain derived. Index choice, compression, and background
optimization must never create a second user-visible truth.

Published research and public mathematical specifications may inform the
algorithms, but pgContext implementations are written independently in
pgContext-owned Rust. The project will not copy, vendor, or depend on another
database engine's index, quantization, or query implementation. Other engines
remain useful behavioral and benchmark references.

PostgreSQL owns transactions, MVCC, ACL/RLS, SQL planning, source storage,
WAL/recovery, replication, and index lifecycle. Rust owns validated vector
representations, distance and quantization kernels, ANN algorithms, bounded
query execution, and versioned derived formats. Model inference stays outside
the backend. This boundary lets pgContext use each layer for what it does well
without building a second database inside the extension.

## Frequently asked since 0.1.0

For transparency, here is where the capabilities most often requested after
0.1.0 stand in 0.2.0 and where the remaining work is headed:

- **Faster index builds** — pgvector currently builds HNSW indexes faster.
  Closing that gap is planned through parallel-build efficiency and
  construction-throughput work under
  [Delivery Phases](#delivery-phases-post-v1-overview); a concurrent builder
  already scales to roughly 3.3–3.5× at eight workers.
- **IVFFlat** — implemented as the experimental native
  `pgcontext_ivfflat` access method with deterministic external construction,
  source-authoritative reranking, DML/VACUUM/REINDEX/CIC/partition lifecycle,
  PG17/18 dump/restore, crash replay, physical replication, and SQ8/PQ posting
  codecs. Remaining work is drop-in pgvector conversion/naming, PostgreSQL
  progress-view integration, and matched production-scale frontier evidence. See
  [Full IVFFlat Support](#full-ivfflat-support).
- **PostgreSQL lexical search** — full `tsvector` and `tsquery` support is a
  first-class retrieval track: stored/generated vectors, caller-supplied
  queries, language configurations, weighted fields, phrase/prefix/web-search
  forms, GIN/GiST candidates, ranking, highlighting, and hybrid fusion. See
  [PostgreSQL-Native Lexical Retrieval](#postgresql-native-lexical-retrieval).
- **Quantized vectors** — scalar, product, and binary encoded candidates exist
  experimentally for mapped HNSW generations, but complete index-AM,
  configuration, lifecycle, and certification work remains. The roadmap now
  includes first-class asymmetric scalar/binary/product serving, RaBitQ, and
  independent implementations of TurboQuant-MSE and TurboQuant-PROD. QJL and
  PolarQuant are evaluated in their proper roles rather than presented as
  interchangeable codecs. Every approximate path retains authoritative
  full-precision reranking. See
  [Full Quantized Serving and TurboQuant](#full-quantized-serving-and-turboquant).
- **Provider-native `int8` and `uint8` embeddings** — planned as first-class
  source representations, not as derived quantization. Exact distance kernels
  and representation-specific opclasses will score the stored integer values;
  a float copy is optional rather than invented as an authority. This does not
  weaken the rule that codes derived from a float source remain rebuildable
  acceleration artifacts. See
  [Model-Native Integer and Binary Source Vectors](#model-native-integer-and-binary-source-vectors).
- **Ten-million-vector proof** — 10M is now an explicit certification tier,
  not just an unspecified "larger corpus." It covers HNSW, IVFFlat,
  quantization, filtering, lexical/hybrid retrieval, concurrent writes, build
  cost, memory, WAL/recovery, and matched comparisons. See
  [Reproducible Public Benchmarks](#reproducible-public-benchmarks).
- **`halfvec`, `sparsevec`, and `bitvec` maturity** — their SQL types remain
  experimental, while the complete metric-bound HNSW opclass names are now
  stable as recorded in
  [Non-Dense ANN Opclasses](#non-dense-ann-opclasses). Named sparse ANN is now
  implemented experimentally under [Named Sparse ANN](#named-sparse-ann).
  Existing pgvector sparsevec columns can be indexed and converted when their
  dimensions fit pgContext's current vector policy and HNSW record envelope.
  Full pgvector sparse coordinate-range compatibility is planned under
  [Large-Dimension Sparse Vectors](#large-dimension-sparse-vectors).
- **x86-64 performance** — the AVX2+FMA kernels are implemented and
  correctness-verified, but no x86 speed claim is made until measured on real
  x86 hardware. Every published benchmark is Apple Silicon (NEON). See the
  x86-64 SIMD kernels item under
  [Delivery Phases](#delivery-phases-post-v1-overview).
- **PostgreSQL majors** — PostgreSQL 17 and 18 are the supported release
  targets; PostgreSQL 17 remains the primary deep-lifecycle and benchmark
  target. PostgreSQL 15 and 16 are not current roadmap targets. Adding another
  major requires an explicit support-policy change and its own compile, schema,
  install, upgrade, recovery, packaging, and platform evidence. A
  non-supporting PostgreSQL 19 beta/RC readiness lane may run once the pinned
  pgrx toolchain supports the selected prerelease; that lane produces no
  package, compatibility, or support claim.
- **Drop-in pgvector name compatibility** — the certified companion bridge now
  builds pgContext indexes on existing `vector`, `halfvec`, and bounded
  `sparsevec` columns without data movement; full unqualified name
  compatibility is sequenced later. See
  [pgvector Migration and Compatibility](#pgvector-migration-and-compatibility).
- **Changing embedding models without re-embedding everything** — planned as
  mixed-profile late fusion rather than vector-space translation. Old and new
  model profiles keep separate named vectors and indexes, each query is embedded
  with every selected profile, and candidates are merged by stable chunk
  occurrence ID with weighted reciprocal rank fusion. A full backfill remains
  optional. See
  [Multi-Model Retrieval, Semantic Reranking, and Automatic Chunking](#multi-model-retrieval-semantic-reranking-and-automatic-chunking).
- **Automatic chunking and model-based reranking** — planned as a bounded,
  restartable ingestion and retrieval workflow. PostgreSQL owns source versions,
  chunk lineage, permissions, jobs, and atomic publication; provider-neutral
  external workers own parsing/OCR, tokenizer-specific chunking, embedding
  calls, and cross-encoder inference. See
  [Multi-Model Retrieval, Semantic Reranking, and Automatic Chunking](#multi-model-retrieval-semantic-reranking-and-automatic-chunking).
- **Replacing a dedicated RAG database** — that is the product goal for the
  retrieval and evidence layer: PostgreSQL remains the source of truth while
  pgContext supplies dense, sparse, lexical, multi-vector, graph-assisted, and
  hybrid retrieval. Embedding, reranking, and generation models remain
  provider-neutral external workers; the extension will not put model calls or
  credentials in a PostgreSQL backend. See
  [Evidence Assembly, Provenance, and RAG Evaluation](#evidence-assembly-provenance-and-rag-evaluation).

## Delivery Phases (post-V1 overview)

The detailed sections below are grouped into broad delivery phases, in
order:

1. **pgvector interoperability (implemented in 0.2).** Install
   pgContext alongside an existing pgvector database and build pgContext
   indexes directly on existing `vector`, `halfvec`, and bounded `sparsevec`
   columns — no data movement — with
   a side-by-side comparison function and a migration report/adopt
   toolkit. Queries over pgvector-typed columns always return full
   results; an advisory notice (optional, on by default) recommends
   migration.
2. **Write-path scalability.** pgContext uses a segmented index design:
   constant-time WAL-logged inserts into a small delta segment, merged at
   query time with the main graph, crash-safe at every WAL boundary, and
   bounded compaction from the index's own pages. Full deltas rotate into
   immutable graph segments; directory saturation compacts only the smallest
   adjacent pair after `maintenance_work_mem` admission.

   Source-table `INSERT`, `UPDATE`, and `DELETE` already maintain HNSW
   incrementally; ordinary writes do not require a full index rebuild. A
   separate planned improvement is **set-based bulk mutation execution**.
   Array and batch APIs avoid client network loops today, but some point
   mapping, source-validation, and payload paths still issue work per key or
   per key/field pair inside the extension. Set-oriented PostgreSQL statements
   will remove that internal overhead without changing point IDs, transaction
   semantics, source authority, or index maintenance. See
   [Set-Based Bulk Mutations](#set-based-bulk-mutations).

   The planned next step is **background-worker compaction**: moving the
   rebuild off the write path so writes are accepted at full speed while a
   separate process maintains the index, improving both sustained
   throughput and tail latency. It is sequenced after the multi-segment
   work below, because segmentation changes the unit of compaction from a
   whole graph to a single bounded segment, which is the right unit for a
   background worker to operate on.
   The complete lifecycle and promotion gates are in
   [Segmented Serving and Background Compaction](#segmented-serving-and-background-compaction).
3. **Large-corpus quality, build throughput, and memory.** The goal is
   competitive quality and latency at million-vector scale while keeping
   the small-corpus latency pgContext already delivers. Planned work:

   1. **Scale-aware search-effort defaults.** The search effort a graph
      needs to hit a recall target grows with corpus size, so a fixed
      default degrades recall as collections grow. This adds defaults
      that scale with corpus size, including a higher `ef_construction`
      for large builds.
   2. **x86-64 SIMD kernels.** The dense distance kernels dispatch
      hand-written AVX2+FMA on x86-64 behind runtime feature detection,
      falling back to a scalar path on CPUs without those features.
      Performance measurement on x86 hardware and AVX-512 kernels are
      planned; no x86 speed claim is made until measured on real x86.
   3. **Parallel-build efficiency.** Profile-guided improvement of the
      parallel HNSW builder's scaling.
   4. **Segmented serving for per-query parallelism.** Split a large
      index into bounded segments, each with its own graph, and search
      them in parallel per query, merging the results — the mechanism
      that lets a segmented engine deliver lower latency at high recall.
      It also makes builds embarrassingly parallel and bounds compaction
      to a single segment. Small collections stay a single segment, so
      their behaviour is unchanged.
   5. **Memory and quality features.** A target-recall setting that
      auto-tunes search effort per index, quantized in-graph traversal
      with exact reranking, statistics-driven filtered search, and
      memory-budgeted external index builds for very large tables. Quantized
      traversal with exact rerank is implemented; the remaining tuning and
      external-build work is planned.
4. **Index portfolio and compressed serving.** Establish one typed,
   versioned quantization domain shared by every index; implement full IVFFlat
   as a complementary low-memory, fast-build ANN index; finish native
   quantized HNSW/IVF candidate serving; and evaluate RaBitQ,
   TurboQuant-MSE, TurboQuant-PROD/QJL, and PolarQuant against scalar, binary,
   and product quantization on the same recall/latency/memory frontier.
5. **PostgreSQL-native lexical and universal query execution.** Promote
   `tsvector`/`tsquery`, GIN/GiST-backed lexical candidates, configurable
   ranking, fuzzy matching, and richer nested fusion/reranking while preserving
   PostgreSQL MVCC, ACL/RLS, and exact source rechecks.
6. **Broader pgvector compatibility.** The 0.2 bridge and resumable ownership
   conversion cover the certified PG17 profile. When pgvector is not installed,
   pgvector-spelled SQL (types, operators, opclasses, `USING hnsw`,
   `USING ivfflat`, familiar settings) runs unmodified, validated by running
   the pgvector regression suite in CI, plus an in-place adoption tool that
   converts columns without rewriting tables.
7. **Advanced retrieval.** Named sparse ANN with exact recheck, typed
   composite fusion, quantized candidates, internally maintained late
   interaction, mapped HNSW, and automatic observability have experimental
   bounded serving paths. Planned research adds MUVERA-style multi-vector
   candidate generation, Matryoshka coarse-to-fine search, learned sparse
   retrieval, multimodal document retrieval, mixed-profile embedding migration,
   weighted rank fusion, and externally executed semantic reranking without
   moving model inference into PostgreSQL. A corpus-level lane adds global
   query statistics and coverage profiles, extractive corpus maps, and
   stratified evidence sampling as deterministic engine-owned surfaces. An
   explicitly opt-in multi-source lazy beam begins as an HNSW-only experiment
   and later admits the optional topology provider described under
   [Graph-Augmented Retrieval](#graph-augmented-retrieval).
8. **Document ingestion, evidence quality, production, and 10M certification.**
   Add bounded automatic document chunking, source-linked evidence assembly,
   citation/provenance output, answerability signals, source-linked summary
   artifacts — deterministic-partition rollups and eager/lazy hierarchical
   corpus summaries — and held-out RAG evaluation. In parallel, run model-checked and fuzz-tested concurrency,
   crash-recovery and replication test matrices, deeper independent PostgreSQL
   17 and 18 certification, bounded erasure and operator-invocable structural
   verification, PostgreSQL-native wait/statistics/EXPLAIN integration,
   progress reporting, 10M-vector performance and quality gates, and removal
   of experimental labels surface by surface as certification rows go green.
9. **Ecosystem.** Reproducible public benchmarks—including a corpus-size
   scaling benchmark, 10M-vector tier, and third-party-harness/x86 coverage (see
   [Reproducible Public Benchmarks](#reproducible-public-benchmarks)) —
   framework integrations (see
   [Ecosystem and Framework Integrations](#ecosystem-and-framework-integrations)),
   broader packaging, and scale-out deployment playbooks using standard
   PostgreSQL replication and partitioning.

## Dependency Order

~~~text
PG17 V1 freeze
├── write-path and large-corpus performance
│   ├── set-based point, validation, and payload mutations
│   ├── scale-aware search/build defaults
│   ├── measured x86-64 SIMD + parallel-build efficiency
│   └── bounded multi-segment index and serving
│       ├── per-query parallel segment search + global top-k merge
│       └── per-segment compaction
│           ├── supervised background-worker compaction
│           └── bounded deletion propagation + physical artifact erasure
├── non-dense ANN opclasses
│   └── named sparse ANN
│       └── large-dimension sparse vectors
├── shared retrieval architecture, source authority, and typed contracts
│   ├── model-native int8/uint8 source types + exact kernels/opclasses
│   ├── HNSW storage, WAL, exact-recheck, and build contracts
│   │   ├── mapped HNSW
│   │   ├── operator-invocable structural verification
│   │   └── full IVFFlat lifecycle
│   │       ├── pgvector IVFFlat compatibility
│   │       └── SSD-resident and streaming ANN evaluation
│   └── pgContext-owned typed quantization domain
│       ├── first-class scalar/SQ8, binary, and product quantization
│       ├── RaBitQ + TurboQuant-MSE prototypes
│       │   └── QJL estimator + TurboQuant-PROD prototype
│       ├── standalone PolarQuant evaluation
│       └── HNSW + IVFFlat candidate-serving integration
├── non-dense ANN + HNSW + IVFFlat compatibility
│   └── full pgvector migration and name compatibility
├── internally maintained late interaction + model/version metadata
│   ├── MUVERA-style fixed-dimensional candidate generation
│   ├── multimodal document retrieval
│   └── Matryoshka coarse-to-fine retrieval
├── non-dense ANN + quantized serving + named sparse ANN + late interaction
│   └── composite query execution
│       ├── PostgreSQL-native lexical retrieval
│       └── query execution and operations governance
│           └── typed result-grouping, facet, and aggregation stages
│               over arbitrary child queries
└── composite query execution + mapped serving
    └── expanded automatic observability
        ├── PostgreSQL 17 custom wait events
        └── PostgreSQL 18 cumulative statistics + EXPLAIN options

stable source identifiers + composite execution + mapped serving
├── experimental HNSW-only lazy beam cursor
│   └── bounded virtual state arena + exact source rerank
└── pgGraph topology-boundary audit + provenance manifest
    └── pgContext-owned `context-topology` kernel
        └── versioned projection + optional residency
            └── query-owned topology-expansion port
                └── mixed vector/topology beam + graph-augmented retrieval

exact retrieval + composite execution
└── global query statistics and coverage profiles

stable source identifiers + clustering primitives + lexical term statistics
└── extractive corpus maps
    └── stratified global evidence sampling

stable source identifiers + model/profile metadata + composite execution
├── mixed-profile weighted RRF
│   └── external semantic-reranker envelope + final source revalidation
└── registered document sources + provider-neutral worker contract
    └── bounded automatic chunking + versioned embedding generations
        ├── optional contextual chunk enrichment
        └── atomic profile/chunk publication and rollback

worker contract + deletion propagation + aggregate-visibility contract
└── source-linked summary artifacts
    └── deterministic-partition rollup summaries

source-linked summary artifacts + extractive corpus maps
└── hierarchical corpus summaries (eager and lazy generation)

composite execution + lexical retrieval + source/model provenance
├── multi-model/chunking workflow
│   └── evidence assembly, citation records, and RAG evaluation
└── optional graph-augmented evidence expansion

collection registration + exact retrieval + advisor
└── load data and query immediately
    ├── exact-first readiness and explicit lifecycle states
    └── background HNSW/IVFFlat/lexical builds
        └── validated generation publication and automatic recommendations

10M harness + exact ground truth starts after PG17 V1 freeze
└── write-path track + segmented serving + HNSW + IVFFlat
    + quantization + lexical/query + multi-vector/multimodal paths
    └── 10M performance, quality, write-path, and recovery certification

stable query contracts + load/query workflow + certified packages
└── reference framework integrations + compatibility matrix
~~~

## Shared Retrieval Architecture

Status: planned cross-cutting consolidation before the index and codec
portfolio expands. This is an incremental refactor, not a rewrite.

Depends on: the stable SQL type/operator behavior, exact distance oracles,
existing query IR, and versioned storage/generation contracts.

Goal: each retrieval concept has one canonical definition and validation path
even though PostgreSQL adapters, Rust algorithms, storage formats, and public
SQL have different responsibilities. The implementation audit will determine
where duplication exists; the roadmap does not assume every current boundary
needs to move.

The current workspace already has useful boundaries: `context-core` owns the
canonical `DistanceMetric`, `context-query` owns candidate/readiness ports, and
PostgreSQL integration is isolated in `context-pg`. The clearest consolidation
pressure is quantization: algorithm/training types live in `context-index`,
HNSW-specific persisted codebooks and approximate scoring live in
`context-storage`, SQL/catalog policy is adapted in `context-pg`, and benchmark
code performs another conversion. Query and fusion also carry separate
score-direction types. That evidence calls for a shared domain and adapters,
not a workspace rewrite.

Scope:

- centralize typed definitions for vector representation, metric and score
  direction, index kind, codec specification/version, candidate source, source
  identity, model/configuration revision, and generation/readiness state;
- parse SQL, JSON, GUC, catalog, and on-disk values at adapter boundaries, then
  pass validated domain values internally instead of propagating raw strings,
  loosely related booleans, or per-module copies of the same enum;
- keep exact distance, metric compatibility, score normalization, deterministic
  ordering, candidate deduplication, and source-recheck rules in shared pure
  functions with property tests used by every index and query path;
- separate domain algorithms from PostgreSQL callbacks, WAL/page persistence,
  mapped I/O, background workers, and telemetry through narrow ports; use
  static enum dispatch or specialized kernels in performance-critical loops
  where dynamic dispatch would be measurable;
- distinguish a persisted format descriptor from a mutable runtime policy so a
  setting change cannot reinterpret old bytes or silently mix generations;
- add compile-time exhaustive matching and cross-adapter contract tests so a
  new metric, codec, vector type, or candidate source cannot be registered in
  one layer and forgotten in another;
- preserve SQL and storage compatibility through explicit adapters,
  backward-read fixtures, rebuild/cutover plans, and deprecation windows;
- baseline build time, binary size, allocations, query latency, and index
  throughput before each consolidation step so architectural cleanup cannot
  hide a performance regression.

This track is complete when adding a new codec or candidate source requires one
domain registration plus explicit supported adapters, and unsupported
combinations fail in a single validation layer. Internal Rust type names and
crate boundaries remain implementation details until an implementation plan is
selected.

## Set-Based Bulk Mutations

Status: planned write-path optimization. pgContext accepts array and batch
inputs today, but some point-mapping, source-validation, and payload-mutation
paths still execute per key or per key/field pair through SPI.

Depends on: stable source-key and point-ID semantics, collection resource
limits, authoritative source rows, ACL/RLS enforcement, and PostgreSQL index
maintenance.

Goal: process a bounded batch with a bounded number of PostgreSQL statements.
Set-based execution removes extension-level row loops; it does not imply that
PostgreSQL heap and index access methods physically process an entire batch as
one tuple.

Scope:

- convert validated batch input into typed rowsets with ordinality using
  arrays, `unnest`, or recordsets;
- replace per-key point mapping calls with set-oriented `INSERT ... ON
  CONFLICT ... RETURNING` and `UPDATE ... FROM`; point deletion remains logical
  and preserves stable point IDs rather than becoming a physical delete;
- validate active point mappings and source rows with joins and anti-joins,
  returning deterministic missing/invalid-key errors without an existence
  query for every key;
- set, delete, or clear payloads with one bounded statement per operation or
  compatible field group, never one statement per key/field pair;
- support heterogeneous recordsets where each row may carry different payload
  or vector values, while ordinary source-table `COPY` and multi-row SQL remain
  the primary ingestion path;
- bound parameters, statement size, memory, and lock duration by chunking large
  calls without weakening transaction-level atomicity;
- preserve stable point IDs, input ordering, duplicate-key behavior,
  inserted/reactivated/deleted classification, per-row results, rollback,
  source authority, ACL/RLS, triggers, generated columns, and SQLSTATEs;
- quote dynamic identifiers safely while keeping values typed and
  parameterized;
- certify `COPY`, multi-row inserts, point upserts/deletes, payload
  set/delete/clear operations, and heterogeneous updates with live HNSW and
  ordinary PostgreSQL indexes.

Promotion requires real PostgreSQL tests for duplicate and missing keys,
validation failure before mutation, rollback, concurrent overlapping batches,
deadlock/cancellation behavior, ACL/RLS, triggers, and exact per-row result
parity. Benchmarks cover 1, 100, 1,000, and 10,000 rows and report statement
count, throughput, p50/p95/p99 latency, CPU, memory, WAL per row, lock
contention, and live-index maintenance cost against the pre-refactor and
direct-SQL baselines.

This work and background compaction solve different write-path costs.
Set-based execution removes repeated SPI statements inside one user mutation.
Incremental HNSW maintenance already makes each changed source row searchable;
segmented background compaction later prevents accumulated index maintenance
from creating periodic write-path stalls.

## Segmented Serving and Background Compaction

Status: partially implemented for the PostgreSQL 17 profile. The WAL-logged
delta segment, bounded threshold, explicit `pgcontext.compact(index)`, and
synchronous threshold compaction exist. Bounded multi-segment serving,
per-segment compaction, and supervised background-worker execution are planned.
Incremental index writes are therefore current functionality; moving
maintenance out of the foreground write and bounding it per segment are the
remaining roadmap work.

Depends on: page-native HNSW and its WAL/recovery contract, authoritative source
rechecks, immutable generation publication, bounded worker/resource policy, and
automatic observability.

Goal: sustained writes append at predictable cost while queries search a
bounded set of immutable graph segments plus the active delta. Maintenance
rewrites one bounded segment at a time outside the source write statement.
Small/read-mostly indexes remain a single segment and do not pay parallelism or
coordination overhead.

Architecture and dependency boundaries:

- keep segment selection, compaction eligibility, debt calculation, and state
  transitions in a deterministic policy layer that can be property-tested
  without PostgreSQL; page/WAL work and background-worker scheduling remain
  PostgreSQL adapters behind explicit ports;
- represent each index as a versioned manifest over immutable searchable
  segments plus one bounded active delta, with checked segment identity,
  source/configuration revision, tuple range, metric, checksum, publication
  state, and retirement pins;
- append inserts and VACUUM tombstones only to the active delta under a bounded
  WAL record; rotate a full delta into an immutable segment without rebuilding
  the entire index or changing source-row visibility;
- search relevant segments with a bounded per-query parallelism budget, merge
  candidates through one deterministic global top-k, then resolve and score
  survivors from authoritative rows under the statement snapshot, filters,
  ACL, and RLS;
- prevent worker multiplication from turning query concurrency into CPU or
  memory oversubscription: segment fan-out, distance work, candidate memory, and
  participating workers share one query budget and degrade to serial search
  when the budget or collection size does not justify parallelism;
- compact one or a small bounded set of segments into a new immutable
  generation, validate checksums/counts/recall, WAL-log or otherwise durably
  record publication, atomically switch the manifest, retain old reader-pinned
  segments, and reclaim them only after all pins and recovery obligations end;
- store durable, idempotent maintenance jobs keyed by database/index,
  configuration generation, and input segment set. A database-scoped supervised
  worker claims bounded leases, heartbeats progress, retries only safe stages,
  and recognizes already-published output after restart;
- track every worker from registration through clean shutdown. PostgreSQL
  postmaster death, extension update/drop, database drop, cancellation, SIGTERM,
  statement timeout, worker crash, and server restart must leave either the old
  valid manifest or one fully validated new manifest—never an unowned partial
  publication;
- define an explicit overload policy for compaction debt, temporary disk, WAL,
  memory, and worker exhaustion. Operators choose bounded inline compaction,
  write backpressure/error, or scheduled maintenance; the engine must not hide
  an unbounded pause inside an ordinary write;
- support inserts, updates, deletes, HOT/non-HOT changes, VACUUM, REINDEX,
  partition attach/detach, concurrent index operations, backup/restore, WAL
  replay, standby promotion, and old-format rejection/rebuild across segment
  rotation and compaction;
- report active/immutable segment counts, rows and bytes per segment, delta
  occupancy, tombstones, compaction debt, queue/lease state, last progress,
  worker/retry/failure reason, query fan-out, merge candidates, pinned/retired
  generations, temporary bytes, WAL bytes, and estimated time to threshold.

Test-first implementation order:

1. model and property-test the manifest/segment/job state machines, including
   arbitrary rotation, compaction, retry, reader-pin, and crash sequences;
2. implement serial multi-segment search and deterministic global merge against
   an exact oracle before adding parallel execution;
3. implement one bounded foreground per-segment compaction using the same job
   contract the worker will later consume;
4. add the supervised worker adapter, durable leases, cancellation/shutdown,
   retry, failpoints at every publication boundary, and restart recovery;
5. add bounded parallel query/build execution, then tune locality, segment
   sizing, and scheduling from the 1M/10M harness rather than fixed assumptions.

Promotion requires:

- at least 500 sustained source updates/second on the owned G4 workload and a
  24-hour mixed read/write run without unbounded relation growth, compaction
  debt, lost work, incomplete results, or a periodic write-latency cliff;
- recall/latency frontiers no worse than the single-segment baseline at matched
  work, plus a demonstrated p95/p99 or throughput win where parallel segmented
  serving is selected;
- bounded RSS/shared-memory/temp-disk/WAL at 1, 16, 32, and 64 clients, including
  proof that nested query and worker parallelism cannot oversubscribe the host;
- deterministic crash/restart, failpoint, corruption, backup/restore, WAL
  replay, replica catch-up, and standby-promotion matrices over every job and
  publication state;
- explicit tests with the worker disabled, saturated, cancelled, crashed, and
  restarted, proving that exact/query correctness remains available and the
  configured overload policy—not an implicit fallback—controls writes.

## Deletion Propagation and Bounded Erasure

Status: planned cross-cutting lifecycle and enterprise-readiness track. Existing
source rechecks, tombstones, generation retirement, reader pins, artifact
diagnostics, and cleanup establish query correctness and safe reclamation, but
they do not yet provide a complete physical-erasure contract.

Depends on: stable source/chunk occurrence identity, segmented compaction,
immutable generation publication and retirement, token/chunk projection
ownership, automatic observability, and backup/replication documentation.

Goal: make deletion observable and bounded across every pgContext-owned
representation without claiming control over storage that PostgreSQL or the
operator owns. The contract has three separately reported clocks:

1. **Logical invisibility:** a snapshot established after the source deletion
   commits may not return the row through exact, ANN, lexical, token, chunk,
   fusion, reranking, or evidence paths. A transaction retaining an older
   snapshot keeps PostgreSQL's documented MVCC view and is a physical
   reclamation blocker.
2. **Live pgContext storage erasure:** a configured deadline covers compaction
   or rebuild without the deleted item, retirement of old generations, unlink
   of pgContext-owned mapped/segment files, and removal of owned token, chunk,
   cache, job, and observability references once declared blockers clear.
3. **Recoverability horizon:** WAL archives, base/incremental backups, replicas,
   snapshots, and operator-owned source/projection tables follow explicit
   PostgreSQL and operator retention policies. pgContext reports this boundary
   but cannot truthfully certify deletion from external backup media.

Every advertised duration names whether its clock starts at the purge request
or after the final declared blocker clears. A request-relative bound is offered
only by a strict managed-retention mode whose snapshot, reader, replica, and
backup preconditions can actually be enforced.

This is an operational privacy control informed by
[GDPR Article 17](https://eur-lex.europa.eu/eli/reg/2016/679/2016-05-04),
not a blanket legal-compliance claim. PostgreSQL documents that `DELETE` does
not immediately remove an old row version and that ordinary `VACUUM` normally
makes its space reusable rather than returning it to the operating system:
[routine vacuuming](https://www.postgresql.org/docs/current/routine-vacuuming.html).
Its [backup and PITR](https://www.postgresql.org/docs/current/backup.html)
contracts also make archive retention an operator concern.

Scope:

- define an idempotent explicit purge operation, with the final SQL spelling
  frozen during API design, that accepts a registered source occurrence,
  document, collection, or validated predicate plus a retention class and
  request identifier;
- persist a crash-safe purge state machine with requested/deadline/completed
  times, representation inventory, retry progress, and typed blockers such as
  an old snapshot, reader pin, replication slot, failed worker, unavailable
  archive policy, or operator-owned table;
- traverse registered lineage rather than searching by vector similarity:
  source occurrence, document/chunk generations, embeddings and named vectors,
  HNSW/IVF segments, mapped generations, late-interaction token relations,
  lexical projections, caches, evaluation records, jobs, and privacy-bounded
  observability records must each report deleted, not-applicable,
  externally-owned, or blocked;
- force or schedule bounded compaction/rebuild that excludes purged identities,
  validate and atomically publish the replacement, wait for legitimate reader
  pins, then retire and unlink obsolete pgContext-owned files without
  resurrecting data after crash, replay, retry, or standby promotion;
- distinguish deadline policies that may wait for readers from a stricter mode
  that rejects new work, cancels eligible pgContext jobs, or takes a documented
  maintenance lock. If a blocker outlives the deadline, status becomes
  `blocked`/`breached`; the system never records a false completion;
- keep audit evidence content-free: store request state, counts, timestamps,
  policy revision, and opaque/pseudonymous correlation where required, but not
  the erased text, vector, source key, query, tenant identifier, or reversible
  content hash;
- document the user-owned-table boundary. pgContext can delete registered
  projection rows and advise on `VACUUM`, table rewrite, partition drop, or
  encryption-key retirement, but cannot promise secure byte overwrite for an
  ordinary PostgreSQL heap or storage device;
- expose live-storage completion and backup recoverability as different fields,
  including configured WAL/archive/base-backup/replica retention, and provide a
  checklist for backup expiration or cryptographic erasure outside the
  extension;
- make collection/drop, profile retirement, source supersession, and ordinary
  deletion reuse the same lineage and reclamation machinery so the explicit
  purge path is not a separate, untested deletion engine.

Promotion requires failpoint and restart tests at every purge state; old
snapshot, live/stale reader-pin, replication-slot, replica-lag, worker-failure,
and deadline-breach fixtures; verification that no query branch or evidence
surface returns the item under a post-delete snapshot; byte-level absence
checks across every pgContext-owned retired generation and orphan path after
completion; and
documented restore tests showing exactly when older backups can and cannot
reintroduce the erased content.

## Experimental Promotion Queue

Status: ordered certification work. This section does not reclassify a
capability; the parity matrix and release notes change only after the listed
exit gates pass on the same release candidate.

Promotion is intentionally split by contract. A stable exact SQL type or query
surface does not need to wait for a stable ANN page format, and a stable opclass
name does not imply that every index lifecycle using it is stable.

1. **Exact `halfvec` SQL surface.** Nearest promotion candidate. Freeze
   text/binary I/O, typmods, rounding/casts, operators, exact metrics,
   aggregates, ordering, NULL/non-finite errors, and dump/restore; run the
   selected pgvector compatibility fixtures on PostgreSQL 17 and 18. Keep the
   HNSW storage row experimental independently.
2. **Exact `bitvec` SQL surface and explicit opclass names.** Promote after
   Hamming/Jaccard property tests, cast/typmod boundaries, zero-length and
   all-zero/all-one fixtures, planner/operator ordering, dump/restore, and
   cross-major tests. Bit HNSW pages remain separately lifecycle-gated.
3. **Bounded `sparsevec` SQL plus named sparse exact/fusion.** Promote the
   documented bounded profile after sparse-native arithmetic/overflow tests,
   source-authority and RLS gates, DML/restart/dump coverage, and stable limit
   errors. Full pgvector coordinate-range parity remains owned by
   [Large-Dimension Sparse Vectors](#large-dimension-sparse-vectors), and sparse
   ANN remains a separate row until its page/serving gates pass.
4. **Model-native integer source types.** Promote signed and unsigned 8-bit
   exact SQL types independently after I/O, typmod, cast, metric,
   wide-accumulator, SIMD-oracle, provider-fixture, planner, DML,
   dump/restore, and cross-major gates. Promote each integer HNSW/IVF
   representation/metric opclass separately; an exact type does not freeze an
   ANN storage format.
5. **Dense HNSW and filtered ANN.** Promote together only after the remaining
   serving-memory envelope is dispositioned, sustained mutation reaches the
   existing G4 target of at least 500 updates/second without unbounded index
   growth or a tail-latency cliff, the second-environment reproducibility gate
   is green, and the on-page compatibility/rebuild policy is frozen. Existing
   1M matched-recall, filtered-recall, and build-time results are necessary but
   not sufficient.
6. **Non-dense HNSW.** After dense HNSW certification, promote each
   representation/metric tuple independently when it passes the same
   build/DML/VACUUM/WAL/recovery/replica/corruption, exact-oracle, bounded-work,
   memory, and second-environment gates.
7. **Baseline quantized serving and per-vector configuration.** Promote scalar,
   SQ8, binary, and product codecs independently once configuration is consumed
   end to end, codec/index generations cut over atomically, full-precision
   reranking is deterministic, and the 1M/10M
   recall/latency/memory/lifecycle matrix passes per codec. These codecs do not
   wait for TurboQuant, QJL, PolarQuant, or RaBitQ research.
8. **Research quantization codecs.** RaBitQ, TurboQuant-MSE,
   TurboQuant-PROD/QJL, and standalone PolarQuant remain planned until a real
   prototype exists, then enter the same per-codec promotion gate without
   inheriting maturity from the baseline codecs or one another.
9. **Multi-vector and late interaction.** Keep experimental until owned token
   maintenance, backfill/repair, memory and comparison budgets, write
   amplification, cancellation, 10M-document latency, and recovery/replication
   gates pass without requiring a user-maintained shadow table.

The first three entries are the best candidates for earlier, narrow promotion
because their exact SQL contracts can be certified without freezing an ANN
storage format. Dense HNSW/filtered ANN is the highest-impact promotion, but its
open operational gates make it riskier than the type-only surfaces.

## Non-Dense ANN Opclasses

Status: complete.

Depends on: PG17 V1 freeze, dense pgvector HNSW, and the shared metric
semantics.

Scope:

- promote half-vector L2, inner-product, cosine, and L1 HNSW opclasses;
- promote sparse-vector metrics supported by the exact kernel;
- promote bit-vector Hamming and Jaccard where the graph metric satisfies the
  required ordering and pruning contract;
- remove non-dense storage-boundary aborts while keeping representation
  conversion explicit;
- cover create, scan, insert, update, delete, VACUUM, REINDEX, restart,
  dimensions, casts, NULL/non-finite rules, SQLSTATEs, and exact oracles.

Validated by end-to-end serving tests for every representation/metric pair with
exact-oracle and bounded-work assertions.

## Model-Native Integer and Binary Source Vectors

Status: implemented experimentally as a first-class source-type track,
separate from derived quantization. The Rust/SQL representation set now
includes dense `f32`, half-precision, sparse, bit, signed 8-bit, and unsigned
8-bit vectors. Promotion remains per representation/metric tuple and does not
promote the HNSW storage lifecycle.

Depends on: shared metric semantics, typed model/profile metadata, the vector
type and operator-family compatibility policy, architecture-dispatched exact
kernels, and the HNSW/IVFFlat lifecycle.

Providers including
[Cohere](https://docs.cohere.com/v2/reference/embed) and
[Voyage AI](https://docs.voyageai.com/docs/flexible-dimensions-and-quantization)
can return signed 8-bit, unsigned 8-bit, and packed binary embeddings directly.
When an application stores one of those model outputs without a float vector,
the integer or bit value is the authoritative source representation. It must
not be mislabeled as a rebuildable code derived from unavailable floats.

Scope:

- define first-class signed and unsigned 8-bit dense SQL/Rust source types
  (working names `int8vec` and `uint8vec`) with typmods, canonical text and
  binary I/O, array support, checked dimensions, casts, aggregates where
  meaningful, dump/restore, and explicit compatibility rules;
- treat the existing `bitvec` as a model-native binary source only when its
  logical dimensions, packing order, padding, and metric contract match the
  registered profile. Provider bytes are decoded through a checked boundary;
  incompatible signedness, bit order, or padding is never silently reinterpreted;
- add scalar reference kernels and architecture-dispatched SIMD for squared
  L2, inner product, cosine, and any later advertised metric, using sufficiently
  wide accumulation and explicit overflow behavior. “Exact” means exact
  scoring of the stored integer/bit source under the declared metric, not
  reconstruction of a float vector that was never retained;
- add representation-specific operators, operator families, and exact and ANN
  opclasses rather than routing integer values through hidden float conversion;
  planner costing, score direction, zero-norm behavior, and deterministic ties
  remain aligned with the shared retrieval contracts;
- extend immutable embedding profiles with output representation, signedness,
  dimensions, normalization, metric, provider/model/revision, query/document
  templates, packing/bit order, and any provider-declared scale or zero point.
  Absence of dequantization metadata is explicit and does not block exact
  integer-space search;
- require query vectors to use the same registered profile and representation
  that produced the stored source. Checked explicit conversions may be offered,
  but no implicit cross-model or signed/unsigned cast may change retrieval
  semantics;
- if both float and integer embeddings are stored, require one declared
  authority and expose whether final exact rescoring used float or integer
  source values. If only integer values exist, candidate traversal and exact
  reranking use them; optional semantic reranking can still judge authorized
  source text;
- allow HNSW, IVFFlat, mapped, segmented, and SSD-resident indexes to consume
  the source type directly while keeping their graph/list layout and any
  additional compression derived, versioned, checksummed, and rebuildable;
- separate source-type lifecycle labels and promotion gates from derived
  scalar/PQ/binary codec labels so maturity in one track cannot imply maturity
  in the other.

Promotion is per representation/metric tuple. It requires exhaustive I/O and
boundary fixtures; scalar/SIMD oracle equality on arm64 and x86-64; wide
accumulator and zero-norm tests; PostgreSQL planner/operator/opclass, DML,
VACUUM, WAL/recovery, replication, REINDEX, upgrade, and dump/restore matrices;
provider-produced fixture interoperability; and matched quality, latency,
memory, and storage results against float/halfvec baselines. The public
source-authority documentation must be updated before the first integer source
type is marked stable.

## Quantized HNSW

Status: stable for scalar/SQ8, product, and binary HNSW serving on PostgreSQL
17 and 18. The retained one-million-row certification is documented in
[Quantized HNSW one-million-row certification](../benchmarks/quantized_hnsw_1m.md).

Depends on: PG17 V1 freeze, dense HNSW, and resumable generation
infrastructure.

Scope:

- property-test scalar, product, and binary encoding, dimensions, codebooks,
  error bounds, and corrupted codes;
- add the quantized_codes fuzz target and bounded corpus smoke;
- consume quantization policy during real build/generation;
- traverse encoded candidates with bounded work and rerank from authoritative
  source vectors;
- bind configuration revisions from registration through build and scan;
- cover concurrent configuration/source changes, restart, invalidation,
  replacement, corruption, deterministic recall, and serving diagnostics.

Validated by an end-to-end serving test with exact-oracle and bounded-work assertions before promotion.

## Full Quantized Serving and TurboQuant

Status: baseline scalar/SQ8, product, and binary HNSW serving is stable. Native
IVF-SQ8 and IVF-PQ serving is experimental and lifecycle-certified with exact
source rerank. RaBitQ, TurboQuant, QJL, PolarQuant, and residual-PQ research
remain planned and are promoted independently.

Depends on: versioned vector-codec and configuration contracts, mapped HNSW
serving, authoritative full-precision source vectors, exact reranking, and the
shared index build/generation lifecycle. Quantized IVF additionally depends on
the full IVFFlat lifecycle.

pgContext will implement the selected methods independently in pgContext-owned
Rust from their papers and public mathematical descriptions. It will not copy,
vendor, translate, or depend on another database engine's implementation or
storage format. The primary research inputs are
[TurboQuant](https://arxiv.org/abs/2504.19874),
[QJL](https://arxiv.org/abs/2406.03482),
[PolarQuant](https://arxiv.org/abs/2502.02617), and
[RaBitQ](https://arxiv.org/abs/2405.12497) and its
[multi-bit extension](https://arxiv.org/abs/2409.09913), together with the
[Google Research TurboQuant overview](https://research.google/blog/turboquant-redefining-ai-efficiency-with-extreme-compression/).
External engines remain comparison targets only.

The algorithms are not treated as interchangeable labels. Scalar, product, and
binary quantization are codec families. RaBitQ is a competitive binary/multi-bit
candidate. TurboQuant-MSE and TurboQuant-PROD are evaluated separately because
they optimize different scoring paths. QJL is primarily an inner-product
estimation method and may be used directly or as a TurboQuant-PROD component.
PolarQuant is evaluated both as the quantization component described by the
TurboQuant work and, if its standalone path proves useful, as an independent
experimental codec. Public names will follow the implemented mathematics, not
collapse all of these methods into a single `turboquant` switch.

First-class support means a documented typed SQL/catalog configuration,
automatic build and online replacement, HNSW/IVFFlat integration, planner and
advisor support, explain/observability, upgrade behavior, and a published
support matrix. Applications do not manage hidden code columns or codebooks.
Quantized bytes remain derived acceleration artifacts; the registered
full-precision PostgreSQL vector remains the correctness and rebuild source.
This rule applies to codecs derived from a float source; it does not reclassify
provider-native integer or bit values covered by
[Model-Native Integer and Binary Source Vectors](#model-native-integer-and-binary-source-vectors).

Scope:

- centralize quantization in one pure-Rust domain shared by HNSW, IVFFlat,
  exact candidate scans, mapped generations, and benchmark tooling; PostgreSQL
  adapters parse SQL/catalog values into that domain, storage adapters persist
  its versioned artifacts, and index implementations consume it without
  redefining codec choices or metric rules;
- use one canonical typed configuration for codec family, format version, bit
  depth, metric, transform, training state, oversampling, and exact-rescore
  policy. Raw strings and duplicated per-index enums may exist only at external
  parsing boundaries and must map into the same validated representation;
- define shared operations for validation, training, encoding, preparing a
  query scorer, batched approximate scoring, reconstruction where applicable,
  byte accounting, corruption detection, and upgrade/rebuild behavior. Prefer
  enum/static dispatch in the hot scoring loop and narrow trait boundaries at
  storage, build, and testing edges;
- store fixed-width codes in contiguous, aligned buffers with checked
  stride/offset arithmetic rather than one allocation per vector; use the same
  code views for page-native, mapped, and benchmark paths, with scalar reference
  kernels as the oracle for every SIMD implementation;
- make the generic codec format index-independent, explicitly versioned,
  checksummed, and rebuildable from source vectors; incompatible older
  acceleration formats fail closed and require rebuilding;
- record the exact paper/version, independent derivation notes, generated test
  vectors, dependency/license inventory, and required project intellectual
  property review before exposing a research method as a public codec;
- finish scalar/SQ8, product (including separately evaluated OPQ/residual
  variants), and binary 1-, 1.5-, and 2-bit serving as real candidate sources
  rather than helper functions or metadata;
- prototype RaBitQ and TurboQuant-MSE before TurboQuant-PROD so each has an
  independent exact-oracle, build-cost, memory, and recall baseline;
- prototype the current published TurboQuant family at 4-, 2-, 1.5-, and 1-bit
  targets with deterministic seeded transforms, independently test the QJL
  estimator and PolarQuant component, and retain separate feature flags and
  benchmark rows until their failure modes are understood;
- implement full-precision query preparation and architecture-dispatched
  kernels for cosine, inner product, and L2 only where the algorithm supports a
  defensible estimator; L1 must publish its reconstruction cost and may not
  inherit a fast-path claim from another metric;
- keep full-precision source vectors as the correctness and rebuild authority;
  quantized codes may narrow candidates but never become the only copy or the
  final score used for returned ordering;
- expose per-named-vector codec, bit depth, memory placement, oversampling,
  candidate budget, and exact-rescore settings, and consume the same revision
  atomically from registration through build and scan;
- support online codec changes through build-new, validate, publish, reader-pin,
  and retire-old generation semantics, without blocking source writes or
  silently mixing codebooks/configuration revisions;
- implement architecture-dispatched scalar and SIMD kernels on arm64 and
  x86-64, with byte-for-byte reference tests and explicit fallback paths;
- evaluate quantized traversal in HNSW, quantized residual/distance tables in
  IVFFlat, and hybrid arrangements such as IVF-PQ or IVF-TurboQuant; only
  combinations that improve a measured Pareto frontier become public APIs;
- auto-tune oversampling and search effort against a caller-selected recall,
  memory, or latency objective, while allowing reproducible manual settings;
- report codec, encoded bytes/vector, approximate comparisons, candidates,
  oversampling, source reranks, reconstruction count, cache/page behavior,
  fallback, and configuration revision through explain and observability.

Promotion is per codec/index/metric/architecture tuple. It requires property and
fuzz tests for the encoding, end-to-end DML/VACUUM/REINDEX/restart/WAL/replica
coverage, corruption and upgrade tests, deterministic exact reranking, and
published recall/latency/index-size/build-cost curves at 1M and 10M. A codec
does not become the default merely because it compresses more: it must beat an
existing option on a declared, reproducible operating frontier. QJL,
PolarQuant, RaBitQ, and the two TurboQuant modes graduate independently; success
for one name or bit depth does not certify the others.

## Full IVFFlat Support

Status: implemented and experimental. The native clean v4 format, advertised
opclasses, SQ8/PQ codecs, bounded scan controls, diagnostics, native PostgreSQL
parallel assignment, supervised compaction, and PostgreSQL lifecycle are
functionally exercised on PG17 and PG18. Release-scale 1M/10M comparative
certification, automatic pgvector conversion, and full
`pg_stat_progress_create_index` phase reporting remain open and are not implied
by this status.

Depends on: the stable metric/operator contract, authoritative exact scoring,
the PostgreSQL index-AM/WAL lifecycle, parallel/external build infrastructure,
filter selectivity statistics, and versioned index storage.

HNSW remains the high-recall, low-latency graph index. IVFFlat adds a
complementary choice with faster builds, lower memory overhead, predictable
list/probe control, and a natural home for product/residual quantization.
"Full" means owning the complete lifecycle and pgvector-compatible behavior,
not only producing centroids and returning a nearest-list demo.

Scope:

- add a page-native `pgcontext_ivfflat` access method with L2, inner-product,
  cosine, and the representation/metric pairs supported by pgvector IVFFlat;
  additional L1, sparse, or quantized pairs require algorithm-specific quality
  evidence and explicit opclasses rather than implied support;
- support trained list centroids, deterministic sampling, empty/small-table
  behavior, configurable or advised list counts, list occupancy/skew
  diagnostics, and retraining/rebuild guidance as distributions drift;
- implement serial, parallel, and memory-budgeted external builds; use
  PostgreSQL progress reporting for sampling, k-means, tuple assignment, and
  page publication phases;
- implement inserts, HOT/non-HOT updates, deletes, concurrent readers/writers,
  VACUUM, `CREATE INDEX CONCURRENTLY`, REINDEX, partitioned tables,
  dump/restore, WAL replay, physical replication, standby promotion, crash at
  every publication boundary, and explicit version rejection/rebuild;
- provide `lists`, `probes`, `max_probes`, strict/relaxed iterative scans,
  cancellation, work/memory budgets, and planner costing compatible with
  familiar pgvector workflows where semantics can be preserved;
- combine probes with PostgreSQL predicates and registered filter masks,
  widening into additional lists when post-filter candidates are insufficient;
  every survivor is resolved under MVCC/ACL/RLS and scored exactly from the
  source row;
- expose list visits, candidate tuples, filtered survivors, exact reranks,
  centroid/list skew, fallback reason, and budget termination through
  `EXPLAIN`, progress views, and automatic observability;
- make migration recognize existing pgvector IVFFlat definitions, preserve
  their type/metric/options where supported, validate exact and ANN results,
  and provide resumable rebuild/cutover/rollback rather than defaulting every
  case to HNSW;
- evaluate IVF-Flat, IVF-SQ, IVF-PQ/residual, and IVF-TurboQuant using the same
  codec boundary and 10M harness; expose only the variants that materially add
  to the HNSW/IVFFlat speed/recall/build/memory frontier.

The behavioral reference is
[pgvector's IVFFlat contract](https://github.com/pgvector/pgvector#ivfflat),
including its supported representations, probe controls, iterative scans,
parallel builds, and progress phases. Promotion requires exact-oracle and
bounded-work tests for every advertised opclass plus the full write,
maintenance, recovery, replication, and migration matrix. Performance
promotion additionally requires matched 1M and 10M curves against HNSW and
pgvector IVFFlat.

## SSD-Resident and Streaming ANN

Status: planned research track after segmented and mapped serving. No new public
access method is committed until it beats the simpler HNSW/IVFFlat choices at a
declared operating point.

Depends on: segmented generations, mapped serving, background compaction,
versioned codec storage, memory-budgeted builds, and the 10M harness.

The goal is to keep a 10M-vector collection useful when the complete graph,
codes, and source vectors do not fit in RAM. Published DiskANN,
FreshDiskANN, and SPANN design ideas are inputs, including Microsoft's
[Project Akupara](https://www.microsoft.com/en-us/research/project/project-akupara-approximate-nearest-neighbor-search-for-large-scale-semantic-search/);
any resulting pgContext implementation and storage format remain independently
written and PostgreSQL-native.

Scope:

- benchmark a tiered layout in which routing metadata and the hottest
  quantized codes stay memory-resident while colder graph/list pages and source
  vectors remain on SSD;
- compare PostgreSQL buffer-managed pages, immutable mapped generations, and
  bounded direct/batched reads before selecting an I/O design. On PostgreSQL
  18, explicitly benchmark a buffer-managed path that can benefit from the
  server's [asynchronous I/O subsystem](https://www.postgresql.org/docs/18/release-18.html)
  and `pg_aios` observability; do not assume a custom access method or mapped
  file automatically uses it. No path may bypass PostgreSQL visibility,
  permissions, recovery, cancellation, or source rechecks;
- use an immutable base generation plus small WAL-logged mutable segments so
  inserts are queryable immediately, then merge or compact in the background
  without stopping source writes;
- coalesce reads, prefetch only within explicit I/O and memory budgets, and
  expose page reads, bytes, faults, queue depth, cache hit rate, amplification,
  and tail latency rather than hiding storage cost behind average QPS;
- support streaming/external builds whose peak memory is configured
  independently of corpus size and whose intermediate artifacts are resumable,
  checksummed, and safe to discard;
- compare the result with mapped HNSW, full IVFFlat, and quantized HNSW/IVF at
  identical recall, build window, update rate, RAM, SSD, and source-rerank
  budgets.

Promotion requires the ordinary DML/VACUUM/REINDEX/WAL/replica/crash matrix,
bounded degradation under cold-cache and sustained-write workloads, and
matched 10M evidence showing a material frontier improvement. A billion-scale
claim requires its own later public dataset and certification tier; it is not
implied by an SSD-oriented design.

## pgvector Migration and Compatibility

> pgContext is positioned as dedicated-engine-grade retrieval inside
> PostgreSQL, led by the registered-collection API. pgvector
> interoperability comes in two stages: **coexist mode** (install alongside
> pgvector and index existing `vector` columns directly) is the first
> deliverable, while full drop-in name compatibility is sequenced later as
> described below.

Status: the bounded PostgreSQL 17 coexistence and migration profile is
implemented and certified; native pgContext IVFFlat is now available, while
existing pgvector IVFFlat remains an explicit detect-and-plan input until the
P7 conversion contract maps and validates it.

Depends on: PG17 V1 freeze, non-dense ANN opclasses, and quantized HNSW. Full
drop-in coverage also depends on the native IVFFlat lifecycle.

Scope:

- define and test whether pgvector and pgContext can coexist in one database;
  use explicit schemas and PostgreSQL type OIDs rather than assuming types with
  the same SQL spelling are interchangeable;
- accept real pgvector database fixtures and provide a preflight inventory of
  vector columns, dimensions, operators, functions, HNSW/IVFFlat indexes,
  expression indexes, GUC use, and dependent views/functions;
- provide lossless, resumable copy-based or in-place conversions for supported
  dense, half, sparse, and bit representations, with source-row counts,
  checksums, exact-distance fixtures, and rollback before destructive changes;
- cover the pgvector helper and operator surface selected for compatibility,
  including normalization, subvectors, concatenation, vector arithmetic, and
  expression indexes, without duplicating metric semantics outside the shared
  core;
- support subvector and binary-quantization expression-index migration with
  exact reranking against authoritative source vectors;
- expose a compatibility facade for pgvector iterative-scan settings where the
  semantics can be preserved, and fail explicitly for settings or ordering
  modes pgContext cannot honor rather than silently accepting them;
- add parallel HNSW construction and PostgreSQL progress reporting where
  benchmarks demonstrate that serial construction is an operational migration
  bottleneck;
- detect pgvector IVFFlat and generate an explicit retain, exact-search, or
  rebuild-as-HNSW plan; add a resumable rebuild-as-pgContext-IVFFlat path that preserves supported types, metrics,
  list/probe settings, dependent queries, validation, rollback, and cutover;
- test application queries and prepared statements against both extensions and
  publish a precise compatible, translated, and unsupported SQL inventory;
- prove rollback to the untouched pgvector source objects and data without
  depending on pgContext index pages or catalogs.

Validated by an end-to-end serving test with exact-oracle and bounded-work assertions before promotion.

## Large-Dimension Sparse Vectors

Status: planned after the 0.2 bounded sparse compatibility profile; not a 0.2
release blocker.

Depends on: non-dense ANN opclasses, named sparse ANN, versioned storage, and
bounded HNSW serving.

Goal: support pgvector's sparsevec coordinate profile—up to 1,000,000,000
logical dimensions and 16,000 nonzero entries—without allocating memory or
performing work proportional to the logical dimension count. This enables
lossless direct indexing and ownership conversion for the full certified
pgvector sparsevec range rather than only values within pgContext's current
16,000-dimension policy.

Scope:

- split the shared dense-vector dimension ceiling into representation-specific
  policies, including a large sparse coordinate limit and a separately bounded
  nonzero-entry limit;
- replace every sparse-to-dense HNSW build, insert, query, mapped-serving, and
  rerank boundary with sparse-native storage and distance/traversal, so a
  billion-dimensional vector with a handful of entries never creates a
  billion-element allocation;
- define a versioned sparse graph/payload format with checked coordinate and
  offset arithmetic, corruption detection, upgrade behavior, and no silent
  reinterpretation of existing 0.1/0.2 pgContext sparse values;
- carry large sparse typmods and dimensions through catalogs, registration,
  query IR, filters, telemetry, dump/restore, bridge preflight, and resumable
  ownership conversion without narrowing or truncation;
- preserve exact scoring as the oracle and enforce explicit budgets for
  nonzero entries, candidate visits, decoded bytes, build memory, and mapped
  generations;
- add boundary fixtures at dimensions 16,000, 16,001, and 1,000,000,000,
  malformed/overflow/corruption cases, and end-to-end pgvector direct-index and
  ownership-conversion tests covering DML, rollback, VACUUM, REINDEX, restart,
  and dump/restore.

Promotion requires exact-oracle parity for every sparse metric, bounded work
proportional to nonzero entries and visited candidates rather than logical
dimensions, and a live pgvector compatibility gate at the maximum coordinate
range.

## Named Sparse ANN

Status: implemented experimentally.

Depends on: non-dense ANN opclasses and metadata-filtered ANN.

Implemented scope:

- add a real sparse ANN candidate source through the query-owned port;
- retain exact sparse scoring as the correctness oracle and final recheck;
- expose counters proving the default path does not score the full collection;
- cover update, delete, VACUUM, REINDEX, restart, filter masks, and
  configuration changes.

Validated by an end-to-end serving test with exact-oracle and bounded-work assertions before promotion.

## Internally Maintained Late Interaction

Status: implemented experimentally for the PostgreSQL 17 profile.

Depends on: execution ports, resumable generations, and persisted HNSW
serving.

Scope:

- make the registered source vector array authoritative;
- replace the user-maintained companion table with a pgContext-owned token
  relation maintained in the same source DML transaction;
- cover savepoint/rollback, insert, update, delete, bulk repair, schema change,
  stale/not-ready state, and crash/rebuild behavior;
- property-test MaxSim, deduplication, token ordering, and exact rerank;
- enforce token, comparison, hydration, memory, result, and cancellation
  budgets without per-query prerequisite scans;
- make candidate generation replaceable so pooled-vector, token-ANN, and
  MUVERA-style fixed-dimensional encodings can share the same authoritative
  MaxSim rerank and lifecycle contract;
- preserve ACL/RLS, drift, NULL/invalid-token, deleted-point, and source
  rechecks.

Validated by end-to-end serving tests before promotion.

## MUVERA Multi-Vector Acceleration

Status: planned research and implementation after the late-interaction source
and exact-MaxSim contracts are stable.

Depends on: internally maintained late interaction, named vector/model
metadata, a single-vector maximum-inner-product candidate source, versioned
generation storage, and the composite executor.

[MUVERA](https://research.google/blog/muvera-making-multi-vector-retrieval-as-fast-as-single-vector-search/)
maps a set of vectors to a fixed-dimensional encoding so an ordinary
single-vector search can find candidates for an expensive multi-vector score.
pgContext will independently implement and validate that method from its
published description, then exactly rerank candidates with the authoritative
source token vectors and MaxSim.

Scope:

- create deterministic, versioned fixed-dimensional document and query
  encodings with explicit model, dimension, seed, repetition, and normalization
  metadata;
- use existing HNSW/IVFFlat and quantization ports for the fixed-dimensional
  candidate stage instead of creating a second ANN stack;
- support variable token counts, masks, named multi-vector fields, filters,
  partitions, and model-version aliases without changing exact MaxSim
  semantics;
- compare MUVERA candidates with pooled-vector prefetch and token-level ANN at
  the same candidate, memory, build, and exact-rerank budgets;
- expose encoding work, ANN candidates, source token hydration, MaxSim
  comparisons, recall, fallback, and stale-generation state.

Promotion requires held-out multi-vector quality data, exact-MaxSim oracle
parity, DML/rebuild/restart coverage, and a reproducible advantage over at
least one simpler candidate strategy. The fixed-dimensional score is never the
final returned score.

## Adaptive-Dimension and Coarse-to-Fine Retrieval

Status: implemented as an experimental correctness-preserving path; scan-based
promotion is a performance no-go.

Depends on: registered model/version metadata, typed vector dimensions, exact
source reranking, versioned codecs, and the query advisor.

[Matryoshka Representation Learning](https://proceedings.neurips.cc/paper_files/paper/2022/hash/c32319f4868da7613d78af9993100e42-Abstract-Conference.html)
trains embeddings whose useful information is nested in prefixes. pgContext
uses this property only when the registered model contract explicitly
guarantees it; arbitrary embeddings are never silently truncated.

Scope:

- register the full dimension, supported prefix dimensions, model/version,
  normalization, and compatibility hash alongside each named vector;
- search a smaller prefix for candidates only after a bounded visible-corpus
  preflight proves an exhaustive one- or two-stage schedule fits, then score
  every admitted authoritative vector exactly; otherwise choose full-vector
  exact search before prefix work;
- let the advisor select a dimension, candidate budget, and rerank budget from
  a recall/latency/memory objective while preserving reproducible manual
  controls;
- evaluate prefix indexing, expression/subvector indexing, and prefix
  quantization independently before combining their gains;
- support online model and prefix-policy changes through build, validate,
  publish, alias cutover, and rollback rather than mixing incompatible
  embedding generations;
- expose stage dimensions, candidates, full-vector reranks, expansion count,
  and why a query completed or fell back. Byte-read and advisor-selected
  objectives remain research work.

The implemented scan-based path preserves exact ordered results on PostgreSQL
17 and 18 and proves that missing, incompatible, or over-budget policies select
the full-vector path. It is not promoted as a latency optimization: exhaustive
prefix coverage performs more scan work than full-vector exact search, and
larger corpora hit the 10,000-candidate ceiling and fall back before prefix
work. Future prefix indexing or compression must establish held-out 1M/10M
quality and latency curves before it can replace this no-go decision.

## Multimodal Document Retrieval

Status: planned research track after late-interaction and model-provenance
contracts.

Depends on: internally maintained late interaction, MUVERA evaluation,
lexical/hybrid fusion, registered source identifiers, and the external-worker
contract.

The target is page- and region-aware retrieval for documents whose meaning
depends on layout, charts, tables, or images. ColPali-style visual
late-interaction is an initial research direction based on the
[ColPali paper](https://arxiv.org/abs/2407.01449), not a commitment to run a
vision model inside PostgreSQL.

Scope:

- keep original documents, pages, extracted text, and metadata under ordinary
  PostgreSQL ownership while storing model-produced page/token embeddings as
  source-linked vector arrays;
- fuse visual multi-vector, dense text, learned sparse, native `tsvector`, and
  structured-filter candidates through the same typed query plan;
- retrieve stable page, region, and object references with offsets or bounding
  boxes so result hydration and citations can point to the evidence actually
  scored;
- run OCR, parsing, embedding, and optional reranking in provider-neutral
  external workers; record model/version/provenance and make stale or partial
  outputs visible;
- evaluate MUVERA-style prefetch and exact visual MaxSim under explicit token,
  image, candidate, hydration, and memory budgets;
- benchmark on legally redistributable document-retrieval data with separate
  text-only, visual-only, and fused results rather than presenting one aggregate
  score.

Promotion requires source-linked exact-oracle fixtures, held-out retrieval
quality, DML/backfill/restart/model-cutover coverage, RLS-safe hydration, and a
measured gain over strong text and dense baselines.

## Composite Query Execution

Status: shipped for PostgreSQL 17 and 18. The consolidated typed executor owns
the bounded composite contract; external rerank and topology providers remain
later-phase integrations.

Depends on: metadata-filtered ANN, quantized HNSW, named sparse ANN, and late
interaction.

Scope:

- execute the typed query IR rather than stopping at JSON constructors;
- compose dense, filtered, sparse, full-text, quantized, and late-interaction
  adapters without infrastructure dependencies in context-query;
- centralize candidate kind, metric/order direction, score normalization,
  source identity, and stage status in one typed contract shared by query
  parsing, execution, adapters, explain output, and observability;
- accept bounded multi-query, query-expansion, and decomposition branches
  produced by an application or external model, with explicit model/prompt
  provenance and no model call from the PostgreSQL backend;
- define a stable candidate envelope for optional external cross-encoder or
  late-interaction rerankers, then revalidate source version, permissions, and
  hydration before presenting the final evidence set;
- property-test weighted fusion, reciprocal-rank fusion, deduplication, ties,
  stage ordering, rerank, and exact oracles;
- deterministically cover empty/unavailable stages, malicious plans,
  cancellation, budget exhaustion, and semantic errors;
- add the query_plan fuzz target and bounded smoke.

Validated by end-to-end serving tests before promotion.

## Multi-Model Retrieval, Semantic Reranking, and Automatic Chunking

Status: mixed-profile retrieval shipped as a Stable SQL surface after the
frozen equal-weight held-out contract passed at one million rows on PostgreSQL
17 and 18.
Immutable profile lifecycle and versioned coverage, bounded weighted-RRF
execution, explicit degraded policy, equivalent filter/ACL/RLS rechecks, and
profile-backed migration records are implemented. Provider-neutral semantic
reranking is also shipped through a detached bounded prepare/finalize API and
a digest-verified no-network Rust worker. Worker output is untrusted ordering
input and PostgreSQL rechecks current source, filter, ACL, and RLS state before
returning any row. The private fixture adapter certifies that contract without
claiming broad transformer compatibility or bundling weights. Tokenizer-aware
chunking, SQL ingestion catalogs, durable jobs, and external chunk publication
remain incomplete. See [Multi-model retrieval](multi_model.md) and
[Semantic reranking](semantic_reranking.md).

Depends on: stable source and chunk occurrence identities, immutable
model/profile metadata bound to named vectors, composite query execution,
authoritative source rechecks, bounded work governance, and the
provider-neutral external-worker contract.

Goal: let an application adopt a new embedding model without translating old
vectors or immediately re-embedding its entire corpus. Historical chunks may
remain searchable through Model A, new or selectively refreshed chunks may use
Model B, and a query may search both spaces before one shared semantic reranker
judges the best current source text. The same workflow should turn large
documents into versioned, source-linked chunks automatically without putting
file parsing, model calls, credentials, or provider retries in a PostgreSQL
backend.

Scope:

- evolve model-version metadata into immutable embedding profiles that record
  provider/model/revision, dimensions, metric, normalization, query/document
  templates or prefixes, tokenizer revision, configuration hash, lifecycle
  state, and the bound named vector; credentials remain external;
- allow different dimensions and metrics to coexist as separate nullable named
  vector columns or profile partitions while preserving one logical chunk
  occurrence ID across profiles;
- make embeddings eligible only when their recorded source/chunk content
  version matches the current published chunk, so an old profile never returns
  stale text after a source edit;
- embed an incoming query independently with every selected profile, query only
  that profile's vector/index, and apply equivalent authorization and metadata
  filters plus authoritative source rechecks to every branch;
- add parameterized and weighted reciprocal rank fusion with per-profile
  candidate limits, configurable `k`, stable-ID deduplication, deterministic
  ties, contribution diagnostics, global work/result budgets, and explicit
  all-profile versus degraded/partial completion policies;
- keep candidate fusion distinct from semantic reranking: raw cosine or
  distance scores from different profiles are never averaged, and RRF is never
  presented as a cross-encoder judgment;
- define a bounded provider-neutral candidate envelope for external
  cross-encoder, sequence-classification, or listwise rerankers containing only
  authorized current text/references, stable candidate IDs, source-version
  tokens, fused rank, branch provenance, and allow-listed metadata;
- provide an optional, independently implemented Rust reference reranker worker
  behind that envelope, isolated from PostgreSQL and built around immutable
  model manifests, native tokenization, digest-verified artifacts, bounded
  token batches, declared score semantics, and deterministic stable-ID output;
  select a Rust-native or FFI-backed inference runtime only after a published
  compatibility, parity, latency, memory, and packaging spike;
- treat every model and tokenizer artifact as separately licensed from
  pgContext source, record its exact revision, digest, provenance, and
  redistribution policy, and bundle no weights until a release review passes;
- treat external reranker output as untrusted ordering input, restrict it to
  candidates in the issued envelope, and reapply source version, content hash,
  deletion state, filters, ACL, and RLS before returning final evidence;
- support caller-selected `require_reranker` versus visibly degraded
  fused-order fallback; permission, source-version, and profile-mismatch
  failures never use permissive fallback;
- register ordinary user-owned document tables, immutable chunking profiles,
  and user-owned chunk projection tables while keeping the original document
  row authoritative and every chunk/embedding/index rebuildable;
- add a durable, idempotent ingestion outbox with bounded leases, retries,
  cancellation, supersession, typed states, and atomic publication of one
  validated chunk/embedding generation while the prior ready generation keeps
  serving;
- run parsing, HTML cleanup, PDF/layout extraction, OCR, model-specific
  tokenization, embeddings, and semantic reranking in scoped external workers;
  start with deterministic plain-text, Markdown, and HTML adapters before
  separately certifying binary/layout parsers;
- chunk structurally by page/region, heading, section, paragraph, list/table/code
  block, and sentence before tokenizer-safe windows; record byte, character,
  token, page/region, structure-path, parent, and neighbor provenance;
- support optional
  [contextual chunk enrichment](https://www.anthropic.com/engineering/contextual-retrieval)
  in the external worker: generate a bounded document- or section-specific
  prefix for embedding and lexical inputs, store it separately from the
  original chunk, and version its model/prompt/revision/content hash. The
  enriched input is a rebuildable retrieval aid; citations, display, and final
  source validation always use the original authoritative span;
- keep stable chunk occurrence identity separate from `content_hash`: exact
  hashes may enable cache reuse or requested duplicate collapse, but repeated
  text at two source locations remains two citable occurrences;
- enforce configured input-byte, extracted-text/token, page/region, nesting,
  chunk-count, overlap, parser/tokenizer/model-time, staging, statement-size,
  batch, retry, and lease budgets; oversized or unsupported input fails
  explicitly and is never silently truncated;
- publish profile and chunk-generation aliases atomically so operators can
  shadow, promote, drain, roll back, and optionally retire old profiles without
  an all-row rewrite or query-visibility gap;
- expose privacy-bounded profile coverage, stale embeddings, chunk/job state,
  branch completion, fusion contributions, rerank drops/fallback, generation
  readiness, worker attempts, limiting budgets, latency, and storage cost
  without logging query text, chunk text, source keys, tenant IDs, or
  credentials by default.

Promotion is staged. Mixed-profile retrieval first requires known-answer and
property tests for weighted RRF; different-dimension named-vector, filter,
ACL/RLS, source-edit, missing-profile, and ANN exact-recheck gates; and held-out
A-only, B-only, fused, and degraded quality results. The frozen equal-weight,
eight-query workload passes at one million rows on PostgreSQL 17 and 18: fused
recall is no worse than the stronger single-profile baseline under the same
declared global budget, so mixed-profile retrieval is Stable. Semantic
reranking then requires
bounded-envelope, arbitrary-ID injection, concurrent source/permission change,
final-revalidation, timeout/fallback, and held-out lift/latency/cost evidence.
Automatic chunking requires deterministic span/citation fixtures,
malformed and oversized input handling, worker crash/lease/retry/supersession,
atomic publish/rollback/delete, backup/recovery, large-document bounded-memory,
and mixed-profile end-to-end tests. The combined workflow graduates only after
1M/10M quality, latency, storage, mutation, restart, and security evidence shows
that historical Model A and new Model B coverage can coexist without a mandatory
full-corpus re-embedding.

## PostgreSQL-Native Lexical Retrieval

Status: shipped. See [Lexical retrieval](lexical_retrieval.md).

Depends on: composite query execution, the shared fusion layer, registered
field/type metadata, and bounded candidate-source execution.

Registered lexical sources bind ordered weighted text or JSON-path fields, or a
stored/generated `tsvector` column, to a resolved text-search configuration,
ranker, normalization, rank weights, and optional per-row `tsquery`. The typed
`plain`, `structured`, `phrase`, `web_search`, `prefix`, `distance`, `boolean`,
`weight_restricted`, and `registered_tsquery` forms compile to native
constructors with bound values, attached GIN/GiST indexes serve bounded
candidate probes that are authoritatively rechecked, and optional `pg_trgm`
sources add typo tolerance. The legacy arbitrary-column `simple` full-text
branch has been replaced.

Delivered scope:

- register raw text, stored or generated `tsvector`, and caller-provided
  `tsquery` fields without copying them; validate type OIDs, ownership,
  configuration, expression stability, and schema drift;
- select a text-search configuration per field instead of hardcoding `simple`,
  including language dictionaries, stemming, stopwords, thesauri, and
  application-defined configurations already installed in PostgreSQL;
- support `to_tsquery`, `plainto_tsquery`, `phraseto_tsquery`, and
  `websearch_to_tsquery`, plus native Boolean, phrase/distance, prefix, and
  weight restrictions; malformed structured input must fail as a typed query
  error while the web-search form remains safe for raw user text;
- support multi-column document construction with `setweight`, `tsvector`
  concatenation, JSON/JSONB text extraction, configurable A/B/C/D field
  weights, and source-aware null handling;
- use existing GIN and GiST indexes for candidate generation, expose whether the
  chosen plan was indexed, lossy/rechecked, or exact-scanned, and preserve
  PostgreSQL's row recheck semantics rather than maintaining a second inverted
  index;
- support `ts_rank` and `ts_rank_cd`, normalization choices, positional
  proximity, and typed rank output for fusion; add bounded `ts_headline`
  generation as optional result hydration and document that callers must still
  HTML-sanitize untrusted output;
- **still open:** corpus-statistics-aware BM25/BM25F-style ranking as an
  optional PostgreSQL-native ranker with explicit statistics/version semantics,
  deferred because it did not beat the native `ts_rank`/`ts_rank_cd` suite
  without adding an unproven statistics/storage contract;
- **still open:** accept learned sparse outputs such as
  [SPLADE](https://arxiv.org/abs/2109.10086) through the first-class
  `sparsevec` path and fuse them with `tsvector`; model inference remains in the
  external worker and learned sparse scoring is benchmarked separately from
  PostgreSQL lexical ranking;
- trigram fuzzy matching: add a `pg_trgm` similarity candidate source
  (`word_similarity`/`%`/`similarity`, backed by a GIN or GiST trigram index) so
  typo-tolerant and partial-token lexical retrieval can be fused alongside the
  dense, sparse, and full-text branches;
- expose lexical and fuzzy branches through the typed query IR, composable with
  dense, sparse, quantized, recommendation, and late-interaction retrieval by
  RRF, weighted RRF, DBSF, and bounded formula reranking;
- preserve PostgreSQL collation, MVCC, ACL/RLS, partition pruning, cancellation,
  `statement_timeout`, and source-row semantics on every path; never accept a
  lexical candidate merely because an index reported it;
- expose parsed query/configuration, index identity/type, lexemes/candidates,
  heap rechecks, ranking work, source reranks, fallback, and budget termination
  through explain and observability.

PostgreSQL documents `tsvector` and `tsquery` as its native document/query
types, provides the four query constructors and relevance-ranking functions,
and recommends GIN as the preferred text-search index while also supporting
GiST. The compatibility target is that native contract, not merely a separate
structured text-filter API:
[types](https://www.postgresql.org/docs/current/datatype-textsearch.html),
[querying and ranking](https://www.postgresql.org/docs/current/textsearch-controls.html),
and [GIN/GiST indexes](https://www.postgresql.org/docs/current/textsearch-indexes.html).

Promotion requires result/rank fixtures against direct PostgreSQL SQL for every
configuration/query form, proof that GIN/GiST branches avoid full-corpus work
when selected, mutation and generated-column coverage, partition and RLS tests,
and 1M/10M lexical-plus-hybrid quality and latency gates.

## Query and Operations Priorities

Status: planned gap-closing after the typed composite executor. Existing stable
or experimental pgContext features are not re-listed as new work.

Depends on: composite query execution, PostgreSQL-native lexical retrieval,
automatic observability, and collection resource limits.

Dedicated retrieval engines have established a broad query and operations
baseline: nested prefetch and multi-stage queries, RRF and DBSF fusion, formula
scoring and decay, random sampling and ordering, discovery, result grouping,
metadata and geographic filters, strict resource controls, tenant-aware
locality, on-disk storage, and distributed shards. This track closes useful
gaps while retaining PostgreSQL-native joins, security, durability,
partitioning, replication, and backup.

Priority gaps:

1. **Universal query execution.** Generalize prefetch/rerank into bounded nested
   stages; add distribution-based score fusion (DBSF), parameterized and
   weighted RRF, score-threshold stages, and typed formula expressions over
   score plus registered numeric, timestamp, categorical, and PostGIS-derived
   features. Include linear, Gaussian, and exponential time/geo decay with
   deterministic null and unit semantics.
2. **Additional query sources.** Add dedicated random sampling and
   registered-field order-by branches where they make client APIs simpler;
   keep general cross-collection lookup as a safe PostgreSQL join rather than a
   restricted proprietary lookup mechanism.
3. **Composable result grouping, facets, and aggregations.** Generalize the
   existing stable exact `grouped_search` API into a typed `group_by` stage
   that can consume an arbitrary dense, sparse, lexical, fused,
   formula-scored, or semantically reranked child query. Preserve stable
   per-group limits and deterministic group order by the best child result;
   add an explicit maximum group count; define null/missing, multi-valued
   field, pagination, and tie semantics; and require stage ordering so later
   reranking cannot silently violate the requested one-result-per-document or
   per-field cap. Group fields remain registered, authorized PostgreSQL
   columns or JSON paths, and all child provenance survives grouping.
   Alongside grouping, add facet value counts over registered filter columns
   and JSONB paths and typed aggregation stages (count, min/max, sum, and
   mean over registered numeric and timestamp fields) computed over a query's
   matched or candidate set, with an explicit maximum distinct-value budget,
   deterministic value ordering, declared null/missing/multi-valued
   semantics, and results computed only from rows visible under the caller's
   snapshot, ACL, and RLS. A candidate-bounded facet or aggregate is labeled
   as such and never presented as a corpus total; corpus-scoped measurement
   belongs to
   [Global Query Statistics and Coverage Profiles](#global-query-statistics-and-coverage-profiles).
4. **Richer filters.** Add registered full-text, value-count/cardinality,
   UUID, array/nested-element, and optional PostGIS radius/bounding-box/polygon
   conditions with precise missing/null/array semantics and ordinary
   B-tree/GIN/GiST/BRIN/PostGIS index use.
5. **Strict query governance.** Finish enforcement—not only storage—of
   per-collection result, candidate, vector-comparison, filter-complexity,
   nesting-depth, timeout, memory, exact-search, oversampling, batch, and
   concurrent-query budgets. Offer a fail-fast mode for production workloads
   that require an indexed predicate or bounded plan.
6. **Tenant-aware locality.** Teach the advisor and planner to recommend or use
   PostgreSQL list/hash/range partitions, partial indexes, and tenant-leading
   indexes so tenant queries read local pages; test shared-small-tenant and
   dedicated-large-tenant layouts without weakening RLS.
7. **Online optimization and storage tiers.** Finish background per-segment
   build/compaction, immutable generation cutover, hot quantized codes in memory,
   cold full vectors/index pages on disk, and explicit degraded/exact-only
   behavior while an index is not ready. Queries must remain complete unless a
   caller explicitly selects an indexed-only policy.
8. **PostgreSQL-native scale-out.** Publish and test partition fan-out/global
   top-k merge, read-replica routing, physical/logical replication, failover,
   and Citus/FDW-style deployment patterns. This is operational parity, not a
   plan to add a competing consensus or snapshot subsystem.

Public dedicated-engine contracts and production workloads inform this gap
analysis. Promotion remains per capability; there is no single parity
checkbox.

## Load Data and Query Immediately

Status: planned product workflow built from PostgreSQL-native ingestion and the
exact-first collection lifecycle.

Depends on: collection registration, schema introspection, exact retrieval,
background builds, the index advisor, and explicit resource budgets.

Goal: an application can load data with ordinary `INSERT`, `COPY`, or its
existing migration/ETL tool, register the searchable columns once, and issue
correct queries immediately. Derived vector and lexical indexes then improve
the same query without a second ingest API or a correctness-changing cutover.

Scope:

- provide one idempotent registration/advisor workflow that discovers eligible
  `vector`, `halfvec`, `sparsevec`, `bitvec`, vector-array, text, `tsvector`,
  scalar, JSONB, timestamp, UUID, and optional PostGIS columns;
- validate primary/source keys, dimensions, metrics, text configuration,
  nullable/invalid values, model and normalization metadata, permissions, and
  existing useful PostgreSQL indexes before recommending changes;
- make exact vector, lexical, filtered, and hybrid queries available as soon as
  registration commits; expose `ExactOnly`, `Building`, `Indexed`, `Stale`, and
  `Degraded` states without returning incomplete results by default;
- select HNSW versus IVFFlat and full-precision versus quantized serving from
  corpus size, update rate, filter shape, memory, build window, and requested
  recall/latency objectives; show the evidence and generated DDL, and require an
  explicit policy before applying recommendations automatically;
- build and publish indexes in the background with bounded CPU, memory, I/O,
  WAL, and temporary-disk use; preserve writes, cancellation, restart, and
  resumability throughout;
- use set-based point mapping, validation, and payload mutation during bounded
  backfill and bulk workflows rather than moving client-side row loops into
  extension-side SPI loops;
- support multiple embedding generations/named vectors and atomic aliases so a
  new model can backfill and validate before traffic switches;
- define a provider-neutral external-worker contract for optional embedding
  generation. Model calls, credentials, retries, and rate limits stay outside
  the PostgreSQL backend query path; rows record model/version/provenance so
  stale embeddings can be found and rebuilt;
- report ingestion/backfill progress, invalid-row samples, index readiness,
  recall checks, drift/skew, recommended maintenance, and storage cost through
  SQL views suitable for automation.

Promotion requires restartable 10M-row `COPY`/backfill tests, concurrent query
and source-DML tests throughout every state transition, bounded-resource
failure injection, and proof that index publication changes performance rather
than visibility, permissions, exact-score semantics, or ordering among the
returned candidates.

## Mapped HNSW Serving

Status: implemented experimentally and lifecycle-gated for the PostgreSQL 17
profile.

Depends on: resumable generation publication, metadata-filtered ANN, and the
shared graph-read port.

Scope:

- own a real OS mapping with an immutable generation lifetime;
- prohibit normal serving from reading the whole file into a vector or copying
  it through SQL bytea;
- validate checksum, version, dimensions, offsets, alignment, architecture,
  truncation, and corruption;
- implement graph traversal over a mapped wrapper with bounded-copy decoding;
- reuse candidate-mask and authoritative source-recheck contracts;
- cover generation replacement, reader pins, retirement, cleanup, crash
  recovery, corruption, and source changes;
- run targeted Miri for validated pure views and sanitizer-backed subprocess
  tests for real mappings.

Validated by an end-to-end serving test with exact-oracle and bounded-work assertions before promotion.

## Operator-Invocable Index Verification

Status: planned operations track. pgContext already validates supplied artifact
bytes and exposes mapped-segment diagnostics and cleanup, but it has no single
DBA-invocable verifier for a live index and all of its owned generations.

Depends on: frozen page/manifest/segment/codebook formats, safe relation and
artifact resolution, mapped-generation lifetime rules, corruption taxonomy,
and bounded maintenance privileges.

Goal: provide an amcheck-style, read-only-by-default verification surface
(working name `pgcontext.verify(index, mode)`) that distinguishes structural
integrity from retrieval-quality/recall checks. PostgreSQL's
[`amcheck`](https://www.postgresql.org/docs/current/amcheck.html) is the
operational model: verify the invariants that scans rely on, report corruption
precisely, and do not pretend that verification itself repairs it.

Scope:

- resolve the target by relation OID/regclass under a safe search path and
  verify that its access method, owner, database, source registration, and
  artifact roots match before reading any extension-owned path;
- offer bounded `quick`, `standard`, and `full` modes. Quick checks metadata,
  headers, sizes, checksums, and generation references; standard walks pages
  and graph/segment invariants; full may cross-check every live source/index
  membership and therefore has separately documented locks, memory, I/O, and
  runtime;
- verify PostgreSQL page headers and every pgContext page envelope/checksum
  carried by the format; metapage/version/configuration identity; tuple/node
  IDs and bounds; level, neighbor, reciprocity and reachability where required
  by the format, entry-point, and tombstone invariants; and source TID/key
  mappings where the selected mode permits a heap cross-check;
- verify manifest publication state, segment identity/ranges, active/delta/base
  overlap, row/count summaries, codec/codebook revision and checksum, mapped
  file size/hash/alignment, reader pins, retirement state, and missing,
  duplicate, or orphaned generation files;
- return stable typed findings with severity, code, relation/generation/segment,
  block or logical location, invariant, and operator guidance. Default output
  excludes source values, vectors, keys, payloads, and paths not safe for the
  caller;
- require owner/`MAINTAIN`-like privileges for ordinary checks and a stricter
  role for checks that can expose filesystem or cross-tenant structure; honor
  cancellation, `statement_timeout`, interrupts, and resource budgets;
- run safely against a declared snapshot and document what concurrent DML,
  VACUUM, compaction, publication, and retirement can change. A mode that
  cannot obtain a consistent view returns `inconclusive` or a typed blocker,
  never a false clean result;
- keep repair separate. Findings may recommend REINDEX, rebuild, generation
  retirement, or cleanup, but verification performs no mutation unless the
  operator invokes an independently authorized repair operation.

Promotion requires known-good fixtures for every certified format; targeted
corruption injection for page, graph, manifest, segment, codebook, mapped-file,
and orphan cases; concurrent DML/VACUUM/compaction and cancellation tests;
privilege/privacy checks; bounded quick/full resource measurements at 1M and
10M; crash/restart and replica verification; and proof that structural
verification and recall self-checks report their different guarantees clearly.

## Expanded Automatic Observability

Status: implemented and lifecycle-gated for the PostgreSQL 17 profile.

Depends on: executable query outcomes and every serving strategy that it
reports.

Scope:

- automatically persist actual strategy, visits, candidates, filters,
  rechecks, quantization, fallback, latency, cancellation, and budget outcome;
- on PostgreSQL 17 and later, register low-cardinality
  [extension wait events](https://www.postgresql.org/docs/17/xfunc-c.html)
  for real query-candidate I/O, generation publication, build, compaction,
  purge, and worker-queue waits so blocked backends are visible through
  `pg_stat_activity`/`pg_wait_events`; CPU work and ordinary execution states
  are not mislabeled as waits;
- on PostgreSQL 18, feed the server's
  [extension cumulative-statistics API](https://www.postgresql.org/docs/18/monitoring-stats.html)
  from the same typed counters used by the current bounded DSM pipeline, with
  defined reset/drop/persistence and no double counting. PostgreSQL 17 keeps a
  documented compatibility view over the existing collector;
- on PostgreSQL 18, compare and adopt the server's
  [custom EXPLAIN option support](https://www.postgresql.org/docs/18/release-18.html)
  for pgContext stage, budget, candidate, recheck, quantization, grouping,
  reranking, and degradation details while preserving stable text/JSON output
  and a version-gated PostgreSQL 17 fallback;
- bound cardinality and exclude vectors, payload values, secrets, and tenant
  identifiers;
- cover success, typed error, cancellation, concurrent updates,
  rebuild-required, not-ready, and corruption.

The query backend uses a bounded nonblocking named-DSM queue; a database-scoped
worker commits observations independently so aborted statements can be
reported without adding a synchronous catalog write to query latency. Queue
health is restricted to `pg_monitor`, and delivery limitations are documented
as best-effort, may-duplicate, and fail-open. The PostgreSQL 17 gate covers the
complete outcome matrix above, privacy, strategy/work accuracy,
disabled-vs-enabled latency, queue health, and worker reclamation. PostgreSQL
native adapters are compiled and certified per major around one shared pure
telemetry contract; their promotion additionally requires wait-state accuracy,
statistics reset/restart/drop behavior, EXPLAIN text/JSON stability,
privilege/privacy checks, and measured overhead with each surface independently
enabled and disabled.

## Global Query Statistics and Coverage Profiles

Status: planned engine-owned analytic surface. It involves no model inference
and produces no derived text artifacts.

Depends on: exact retrieval oracles, composite query execution, registered
filter columns and JSONB paths, bounded work governance, and automatic
observability.

Goal: answer the deterministic slice of corpus-level questions — how much of a
collection relates to a query, where that relevance sits, and how it
distributes across registered facets and time — exactly and reproducibly.
Coverage profiles also feed the answerability signals in
[evidence assembly](#evidence-assembly-provenance-and-rag-evaluation) and act
as the measurement harness for every later corpus-level tier: corpus maps,
sampling, and summary layers are graded against these statistics rather than
against impressions.

Scope:

- typed statistics requests over any retrieval branch: similarity-score
  histograms with declared binning, count-above-threshold, quantiles, and
  matched-row counts, computed on the exact path by default under bounded
  work, memory, and cancellation budgets;
- optional sampled estimation for large collections with a declared sample
  design, seed, and confidence labeling; an estimate is never presented as an
  exact count;
- joint distributions of relevance mass with registered filter columns, JSONB
  paths, and timestamp buckets, sharing facet semantics with
  [Query and Operations Priorities](#query-and-operations-priorities);
  a query-scoped facet count over a bounded candidate set and a corpus-scoped
  coverage profile are distinct outputs, and every result labels which one it
  reports;
- respect row visibility: statistics are computed only over rows visible under
  the caller's snapshot, ACL, and RLS, so a count or histogram can never
  disclose the existence of rows the caller cannot retrieve;
- deterministic output shapes with stable ordering and versioned semantics,
  exposed through the composite executor and explain/observability, without
  logging query text or row content by default;
- publish the coverage-profile suite used to grade later corpus-level tiers:
  held-out corpus-level questions with exact statistics as ground truth.

Promotion requires property tests for histogram and quantile math against the
exact oracle, ACL/RLS fixtures proving visibility-bounded counts, bounded-work
and cancellation coverage, deterministic reruns, and 1M/10M latency and memory
envelopes for both exact and sampled modes.

## Extractive Corpus Maps

Status: planned deterministic derived-structure track. Build and serving
involve no model inference; a map is rebuildable structure over authoritative
rows, never a second truth.

Depends on: stable source and chunk occurrence identities, the shared
clustering and codebook-training primitives from the
[typed quantization domain](#full-quantized-serving-and-turboquant) and
[IVFFlat](#full-ivfflat-support) work, term statistics from
[PostgreSQL-native lexical retrieval](#postgresql-native-lexical-retrieval),
versioned generation publication, and the background job machinery.

Goal: a versioned, inspectable map of a collection — bounded strata with
deterministic term labels and representative members — built from embeddings,
term statistics, and registered metadata alone. The map gives the corpus-level
surfaces (sampling, rollups, hierarchical summaries) their structure and gives
operators an inspectable answer to "what is in this collection."

Scope:

- cluster registered dense vectors with seeded, budget-bounded algorithms from
  the shared training domain (the k-means family first; hierarchical variants
  with declared linkage and depth limits), producing identical strata for
  identical inputs, configuration, and seed;
- reuse structure the engine already builds where evidence supports it:
  IVFFlat list centroids may seed or serve as strata, and HNSW layer sampling
  may provide uniform-sample baselines; both are evaluated options, not
  assumed equivalences;
- label each stratum deterministically with contrastive term weighting over
  the lexical projections (class-based TF-IDF as formulated by
  [BERTopic](https://arxiv.org/abs/2203.05794)) plus registered metadata
  distributions; labels record the term-statistics revision that produced
  them;
- select representative members per stratum by declared criteria — medoids or
  coverage/diversity objectives such as facility-location selection
  ([Lin & Bilmes](https://aclanthology.org/P11-1101/)) — with deterministic
  ties;
- persist maps as versioned generations with atomic publication, staleness
  tracking against source mutation, bounded incremental refresh, and a full
  rebuild path;
- bind each map generation to a declared visibility scope (whole collection,
  tenant/partition, or registered predicate): stratum labels and sizes are
  aggregates over member rows, so a map is served only within its scope, while
  representatives are ordinary rows that are additionally rechecked per
  caller;
- expose map inspection — strata, sizes, labels, representatives, staleness,
  and generation — as SQL surfaces usable by operators and by downstream
  corpus-level stages.

Promotion requires determinism fixtures (identical inputs reproduce identical
maps), labeled-dataset quality checks against random-partition baselines,
mutation/staleness/recovery matrices, visibility-scope enforcement fixtures,
and bounded build cost at 1M and 10M vectors.

## Stratified Global Evidence Sampling

Status: planned first corpus-level evidence surface. Engine-owned and
model-free; the final SQL spelling is frozen during API design.

Depends on: [extractive corpus maps](#extractive-corpus-maps) or declared
deterministic partitions, the
[evidence assembly](#evidence-assembly-provenance-and-rag-evaluation) budget
and diversity machinery, composite query execution, and authoritative source
rechecks.

Goal: one bounded call returns a corpus-level evidence pack — a diversified,
token-budgeted selection of authoritative rows or chunks that covers a
collection's strata, optionally conditioned on a query and filters — with
provenance describing the sampling design. An external model synthesizes
corpus-level answers from rows the caller is entitled to see; the extension
generates no text.

Scope:

- declared sampling designs: proportional, equal-per-stratum, and
  relevance-weighted allocation; per-stratum selection by representative rank
  or query-conditioned top-k; deterministic output for the same map
  generation, design, and seed;
- resolve every returned row from the authoritative source under the statement
  snapshot, filters, ACL, and RLS. A pack contains only individually visible
  rows, and stratum metadata inside a pack reports visible membership only;
- apply evidence-assembly machinery within and across strata: deduplication,
  near-duplicate collapse, MMR-style diversity, per-source caps, and token
  budgets, with deterministic ordering;
- record per-row provenance — basis (`sampled` or `extractive`), map
  generation, stratum identity and label, allocation rule, and selection
  criterion — so external synthesis can cite rows and evaluation can attribute
  misses;
- integrate as a composite-query stage so global-over-a-filtered-subset — the
  most common production shape, such as one tenant's documents over one time
  range — is the same call with a filter;
- define degraded modes: a collection without a published map falls back to a
  declared seeded-uniform design or a typed refusal per caller policy, and a
  stale map is served with an explicit staleness label, never silently;
- grade packs against an exhaustive partition-synthesis oracle on held-out
  corpus-level questions — every stratum's full content mapped through the
  evaluation model and hierarchically reduced — which defines the
  evidence-recall ceiling a bounded pack is measured against.

Promotion requires determinism and coverage property tests, ACL/RLS fixtures
proving packs and stratum metadata never disclose invisible rows, bounded work
at 1M/10M, and held-out evidence-coverage results against both the exhaustive
oracle and naive top-k retrieval.

## Summary Artifacts and Deterministic-Partition Rollups

Status: planned first model-derived corpus-level tier. Summarization runs in
provider-neutral external workers; the PostgreSQL backend never performs model
inference.

Depends on: registered document sources and the
[provider-neutral worker contract](#multi-model-retrieval-semantic-reranking-and-automatic-chunking),
versioned generation publication,
[deletion-propagation lineage](#deletion-propagation-and-bounded-erasure),
evidence assembly, and the aggregate-visibility contract defined here.

Goal: one shared contract for any stored text derived from many source rows —
the summary artifact — plus its simplest producer: rollup summaries over
deterministic partitions such as time windows, tenants, or registered facets.
Deterministic partitioning avoids clustering risk, keeps staleness
append-mostly, and lets visibility scope align with the partition key, which
is why this tier is sequenced before hierarchical summaries.

Summary-artifact contract:

- every artifact records its producing profile (provider/model/prompt/revision
  and parameter hash), an explicit member set of stable source/chunk
  occurrence IDs, a content hash, a generation, a staleness state, and a
  declared visibility scope; artifacts are derived and rebuildable, never
  authoritative;
- aggregate visibility: an artifact is served only to callers whose row
  visibility covers its declared scope. Scope-aligned builds — per tenant or
  per RLS partition — are the primary strategy; the engine never synthesizes
  per-caller partial summaries, and a failed scope check degrades to a
  model-free basis or a typed refusal per caller policy;
- artifacts join deletion-propagation lineage: purging a member marks
  dependent artifacts for rebuild-without-member under the same
  deadline/state machinery, and audit evidence stays content-free;
- artifacts may be embedded and indexed as registered derived chunks with full
  lineage, so summary retrieval is ordinary retrieval and citations chain
  answer → artifact → members → source rows;
- the worker envelope carries only authorized member text references, the
  profile, and budgets; the engine validates returned text against the issued
  member set, versions it, and publishes atomically.

Rollup scope:

- registered rollup definitions: a partition key (temporal bucket, tenant
  column, or registered facet), a summary profile, a resolution chain (for
  example day → week → month as chained artifacts with cross-level lineage),
  and a refresh policy;
- incremental maintenance: only open or dirtied buckets re-summarize; closed
  buckets stay immutable unless member mutation or purge invalidates them;
- serving: rollup artifacts join evidence packs with basis `rollup`,
  per-bucket provenance and staleness labels, and a declared fallback to the
  sampling basis when a bucket summary is missing or stale beyond policy;
- observability: bucket coverage and staleness, job and lease states, and
  worker-reported token cost per bucket, without logging summary or source
  text.

Promotion requires visibility-scope and RLS fixtures, purge-lineage
rebuild-without-member tests, atomic publication and crash/restart coverage,
worker crash/lease/retry matrices, append and mutation staleness fixtures, and
held-out temporal and corpus-level question results demonstrating lift over
the model-free sampling basis at reported cost.

## Hierarchical Corpus Summaries

Status: planned full-strength abstractive tier for corpus-level questions.
Research inputs are
[RAPTOR](https://proceedings.iclr.cc/paper_files/paper/2024/hash/8a2acd174940dbca361a6398a4f9df91-Abstract-Conference.html)
for recursive cluster summarization and Microsoft's
[GraphRAG](https://www.microsoft.com/en-us/research/project/graphrag/) and
[LazyGraphRAG](https://www.microsoft.com/en-us/research/blog/lazygraphrag-setting-a-new-standard-for-quality-and-cost/)
for eager versus deferred summary generation; per the roadmap's research
policy, implementations remain independently written pgContext Rust and worker
code.

Depends on: the
[summary-artifact contract](#summary-artifacts-and-deterministic-partition-rollups)
and rollup machinery, [extractive corpus maps](#extractive-corpus-maps) for
cluster structure, bounded automatic chunking and versioned embedding
generations, and evidence assembly. The graph-community variant of this
capability lives in [Graph-Augmented Retrieval](#graph-augmented-retrieval)
and additionally requires the pgContext-owned topology projection.

Goal: multi-level abstractive summaries over corpus-map strata — recursively
up to a declared depth — so thematic corpus-level questions retrieve from
summary levels while every summary stays source-linked and rebuildable. Two
generation policies share every other contract: eager builds materialize the
hierarchy at index time; lazy builds materialize a stratum's summary the first
time demand touches it and cache the artifact.

Scope:

- produce level-N+1 summary artifacts over corpus-map strata and recurse under
  declared depth, fan-in, and token budgets; each level is embedded and
  indexed as derived chunks so collapsed multi-level retrieval and
  level-targeted branches are ordinary composite stages;
- eager policy: resumable, budgeted background jobs build the full hierarchy;
  source mutations dirty only affected ancestor paths, and re-summarization is
  bounded to dirty strata rather than the corpus;
- lazy policy: the map itself is built eagerly because it is cheap and
  model-free, but a request touching an unsummarized stratum immediately
  returns the declared degraded basis (`sampled`, `extractive`, or `rollup`)
  and enqueues a bounded summarization job; later requests serve the cached
  artifact. A backend query never blocks on model inference; callers choose a
  require-summary policy (typed not-ready outcome) or degrade-now per call;
- cache and cost governance: per-collection budgets on artifact count, bytes,
  and worker tokens; a retirement/eviction policy for cold strata; profile
  hashes so regenerated summaries are comparable across time;
- visibility: hierarchies are built per declared scope under the
  summary-artifact contract; mixed-visibility strata are handled by
  scope-partitioned builds, never per-caller synthesis;
- map-side query surface: a bounded, ranked, token-budgeted selection of
  summary artifacts at a caller-chosen level — ranked by embedding similarity,
  coverage statistics, or both — with full member lineage; the reduce step,
  final synthesis across selected summaries, stays outside the backend with
  the caller or its workers;
- grade each level against the exhaustive partition-synthesis oracle, the
  sampling basis, and the rollup basis on held-out corpus-level suites,
  reporting evidence recall, citation precision through summary lineage,
  staleness, and cost per query and per build.

Promotion requires every summary-artifact gate plus collapsed-retrieval
correctness (a summary hit always resolves to visible members with an intact
citation chain), eager/lazy equivalence fixtures for identical profiles,
invalidation matrices under mutation and purge, deterministic degraded modes,
and 1M/10M cost and quality curves. A summary level that cannot demonstrate
lift over the model-free sampling basis on its declared question class does
not promote.

## Evidence Assembly, Provenance, and RAG Evaluation

Status: planned product and quality track after the core retrieval branches are
stable.

Depends on: composite query execution, PostgreSQL-native lexical retrieval,
stable source/model identifiers, the mixed-profile and automatic-chunking
workflow, bounded hydration, and automatic observability. Graph expansion and
multimodal evidence are optional inputs, not prerequisites for the first
evidence contract.

Goal: make PostgreSQL sufficient as the retrieval and evidence authority for a
RAG application. pgContext should return a compact, inspectable evidence set
that an external model can use and cite. It does not claim that retrieval alone
eliminates hallucinations, and it does not move generator inference,
credentials, or provider retry logic into a PostgreSQL backend.

Scope:

- define a typed evidence result containing stable source table/key identity,
  content version or hash, field/chunk/page/region location, model and
  configuration version, candidate branch, approximate and exact scores,
  filter/rerank decisions, and index generation where applicable;
- assemble context with bounded deduplication, near-duplicate collapse,
  diversity/MMR, parent and neighbor expansion, per-source caps, token budgets,
  and deterministic ordering while retaining the provenance of every included
  span;
- support parent/child documents and derived hierarchical summaries as
  rebuildable, source-linked artifacts. Evaluate
  [late chunking](https://arxiv.org/abs/2409.04701) and
  [contextual retrieval](https://www.anthropic.com/engineering/contextual-retrieval)
  prefixes generated by the authorized external worker, plus
  [RAPTOR-style retrieval trees](https://proceedings.iclr.cc/paper_files/paper/2024/hash/8a2acd174940dbca361a6398a4f9df91-Abstract-Conference.html)
  without allowing an enrichment or summary to replace or lose its source
  rows. The storage contract and generation policies for those summaries are
  specified in
  [Summary Artifacts and Deterministic-Partition Rollups](#summary-artifacts-and-deterministic-partition-rollups)
  and [Hierarchical Corpus Summaries](#hierarchical-corpus-summaries);
  corpus-level evidence packs declare their basis — `sampled`, `extractive`,
  `rollup`, or `summary` — in provenance;
- allow bounded graph expansion, multi-vector evidence, temporal/freshness
  rules, and structured joins as explicit query stages with their own budgets
  and provenance rather than hidden post-processing;
- return answerability and evidence-coverage signals, including an explicit
  insufficient-evidence outcome, while leaving the application's generator
  free to abstain or request a wider query;
- expose a provider-neutral evaluation record that joins a versioned query,
  judged relevance/evidence, retrieved sources, assembled context, citations,
  and optional external answer scores without logging source text or tenant
  identifiers by default;
- maintain held-out suites for single-hop, multi-hop, temporal, filtered,
  conflicting, duplicate, unanswerable, multi-turn, and permission-sensitive
  questions. The multi-turn lane will draw on the
  [mtRAG benchmark](https://aclanthology.org/2025.tacl-1.36/) where its data and
  license fit the public harness.

Promotion requires deterministic source/citation lineage, ACL/RLS-safe
hydration, bounded context work, mutation and model-version tests, and
published retrieval recall/nDCG/MRR plus citation precision/recall, evidence
coverage, abstention, freshness, and latency. Generator-specific answer scores
remain a separately versioned optional layer so a model change cannot silently
rewrite retrieval claims.

## Reproducible Public Benchmarks

Status: planned; the 10M harness and dataset work starts before all target
indexes are complete, while certification waits for the relevant serving
lifecycles.

Depends on: PG17 V1 freeze and the existing GloVe-100-angular comparison
harness. Full 10M certification additionally depends on segmented HNSW,
IVFFlat, the quantized codec matrix, background maintenance, and the lexical,
composite, multi-vector, and evidence paths being tested.

The published pgContext-versus-pgvector comparison is measured at a single
corpus size (GloVe-100-angular, roughly 1.18M vectors) on Apple Silicon. This
track turns that useful result into a multi-scale engineering and release
program.

### Dataset and ground-truth matrix

- retain GloVe-100-angular as the regression bridge, then add deterministic
  100k, 1M, and 10M corpus tiers; use an archived 10M subset from a recognized
  large-scale ANN corpus such as
  [Big ANN Benchmarks](https://github.com/harsha-simhadri/big-ann-benchmarks)
  plus at least one legally redistributable modern embedding corpus;
- cover representative dimensions (low-dimensional ANN plus 384, 768, and
  1,536 where datasets/models allow), cosine, inner product, and L2, with L1,
  bit, sparse, and late-interaction lanes reported separately rather than
  averaged into a misleading total;
- include clustered, skewed, duplicated, near-duplicate, normalized,
  non-normalized, and distribution-drift fixtures so an algorithm is not tuned
  only to one friendly embedding distribution;
- add 10M-row metadata with controlled selectivities from 0.001% through 100%,
  correlated and anti-correlated predicates, arrays/JSONB, timestamps, and
  shared/dedicated tenant layouts;
- add a 10M-document lexical/hybrid lane with stored `tsvector`, GIN candidates,
  dense and sparse branches, typo/phrase/prefix queries, and a versioned
  judged-query set for recall, nDCG, MRR, and fusion evaluation;
- add held-out multi-vector, Matryoshka, and multimodal lanes with
  exact-MaxSim/full-dimension oracles, model/version metadata, page/region
  judgments, and separate candidate-versus-final quality measurements;
- add a source-linked RAG evidence lane with answerable, unanswerable,
  multi-hop, temporal, contradictory, duplicate, and permission-sensitive
  queries; retrieval and citation quality are primary, while generator scores
  are optional and separately versioned;
- compute exact top-100 ground truth with a blockwise, checksum-recorded
  full-precision implementation independent of the ANN path; validate a sample
  through PostgreSQL exact SQL and a second reference implementation before
  accepting the corpus.

Dataset manifests record source/license, checksums, preprocessing,
normalization, model/version, train/base/query splits, dimensions, filters,
relevance judgments, and ground-truth generator version. Generated datasets
must be reproducible from a seed; downloaded datasets must be content-addressed
and may not silently change.

### Required workload lanes

1. **Search frontiers:** exact, HNSW, IVFFlat, and every promoted quantized
   variant at recall@10 targets including 0.90, 0.95, 0.99, and the highest
   attainable operating point; report the whole speed/recall frontier rather
   than a single preferred setting.
2. **Filtering and tenancy:** indexed and unindexed PostgreSQL predicates,
   adaptive exact/ANN crossover, ACORN-like graph expansion, IVFFlat probe
   widening, partition pruning, RLS, and global versus tenant-scoped queries
   across the controlled selectivity range.
3. **Hybrid quality:** dense + `tsvector`, dense + sparse, three-way fusion,
   DBSF/RRF/weighted RRF, formula/decay rerank, quantized-prefetch +
   full-precision rerank, learned sparse + native lexical, pooled-vector and
   MUVERA prefetch + exact late interaction, Matryoshka prefix + full-dimension
   rerank, and multimodal fusion.
4. **Concurrency:** cold and warm cache; single-client latency; 16-, 32-, and
   64-client throughput; read/write mixes; connection pooling; and cancellation
   storms under fixed CPU, memory, and I/O limits.
5. **Build and ingestion:** binary `COPY`, multi-row and heterogeneous batch
   mutations at 1, 100, 1,000, and 10,000 rows, concurrent source writes,
   serial/parallel/external builds, k-means training, codec generation, index
   publication, background compaction, restart/resume, and time until exact
   versus indexed queries are available. Batch runs also report SPI statement
   count, WAL per row, memory, and lock contention.
6. **Sustained mutation:** insert/update/delete churn at and above the existing
   500-updates/second G4 exit target, distribution drift, tenant promotion,
   overlapping batch writers, payload changes, compaction debt, VACUUM,
   REINDEX, and 24-hour steady-state behavior without unbounded index growth or
   p99 collapse.
7. **Durability and operations:** WAL volume, checkpoint pressure, replica lag,
   crash recovery, standby promotion, backup/restore, partition attach/detach,
   disk-full/low-memory behavior, and old-generation reclamation while the
   10M source remains authoritative.
8. **Evidence and RAG quality:** source/citation lineage, context
   deduplication/diversity, parent/neighbor/graph expansion, token-budget
   behavior, citation precision/recall, evidence coverage, freshness,
   abstention, and multi-turn stability on held-out judgments.

### Measurements and acceptance

Every run records recall@k, nDCG/MRR where applicable, citation and evidence
metrics for RAG lanes, p50/p95/p99/max latency, QPS, CPU
time/instructions/cycles/cache misses when available, RSS and
`shared_buffers`, mapped/resident bytes, index and code bytes/vector, temporary
disk, page faults and PostgreSQL buffer reads/hits, build/training wall time,
WAL bytes, replica lag, source-rerank count, and result completeness. It also
records hardware, NUMA/topology, storage, filesystem, PostgreSQL settings,
extension versions, container/native boundary, cache state, concurrency,
dataset checksum, commit, and date.

Acceptance thresholds are frozen in the benchmark manifest before an
optimization is tuned. At minimum:

- every result respects MVCC, ACL/RLS, filters, deletion state, exact source
  scores, and deterministic ordering of returned candidates; no latency or
  recall win can waive correctness;
- public ANN claims state the recall target and confidence interval, and public
  hybrid claims use a held-out relevance set rather than vector-neighbor recall
  as a proxy for search quality;
- the chosen 10M configuration fits its declared RAM and disk budget, completes
  build/recovery within the declared operational window, and remains within its
  p95/p99 and write-churn envelope for the full steady-state run;
- matched comparisons give pgContext, pgvector, and each declared external
  engine the same hardware, source/query vectors, concurrency, build wall-time
  or memory budget, and target recall. Differences such as in-process SQL
  versus a local service API, exact source rechecks, segment parallelism,
  durability, and full-vector retention stay visible rather than being
  normalized away;
- a result is publishable only after one repeat on x86-64 and one on arm64, with
  no architecture-specific correctness difference and all raw artifacts
  retained.

### Performance work driven by the 10M profile

The following are benchmark hypotheses, not promises. Each lands only if it
moves a measured frontier without weakening lifecycle or correctness:

- cache-aware page/node/list layout, neighbor/identifier compression,
  vector-code interleaving, software prefetch, batched distance evaluation, and
  fewer heap/source fetches before exact rerank;
- SSD-resident routing/code layouts, batched page reads, and streaming builds,
  benchmarked against mapped HNSW and IVFFlat under the same RAM and I/O
  budgets;
- AVX-512/VNNI and future arm64 SVE2/SME kernels behind runtime dispatch, while
  retaining scalar, AVX2/FMA, and NEON reference paths;
- per-query parallel segment/list search with a bounded global top-k merge,
  adaptive parallelism, NUMA-aware worker placement, and protection against
  concurrent-query oversubscription;
- better `ANALYZE` statistics and planner costing for filter cardinality,
  tenant/partition locality, centroid/list skew, HNSW selectivity crossover,
  and source-recheck I/O;
- streaming/external-memory HNSW and IVFFlat builds, parallel k-means, batched
  WAL/page publication, background compaction, and build-time quantization;
- optional GPU acceleration for offline exact-ground-truth generation,
  k-means, codec training, and index construction. CPU serving remains the
  baseline; GPU serving becomes a product dependency only if end-to-end
  deployment and fallback evidence justifies it. Published GPU indexing
  systems remain comparison points.

### Execution tiers and publication

- pull requests run small deterministic property/regression fixtures and a
  reduced performance smoke that catches gross regressions without noisy
  pass/fail claims;
- nightly lanes run the 1M matrix and compare against a pinned baseline with
  noise-aware thresholds;
- scheduled or manually approved dedicated runners execute the full 10M matrix,
  with serial workload lanes where resource contention would invalidate a
  comparison;
- release candidates repeat the relevant 10M certification lanes on both
  architectures and preserve one immutable report bundle.

The third-party lane uses a community-recognized harness such as
[ANN-Benchmarks](https://github.com/erikbern/ann-benchmarks) and/or Big ANN
Benchmarks in addition to the PostgreSQL-aware suite. Each result is archived
with raw samples, summaries, plans, logs, dataset and ground-truth manifests,
deployment/build budgets, and a one-command reproducer. Charts are generated
from those artifacts; no published number is manually transcribed.

## Post-V1 Release Engineering

Status: planned after the installable PostgreSQL 17 GitHub V1.

This track strengthens production claims and adds distribution choices. It is
not evidence that the corresponding platform, package, or operational envelope
is supported by V1.

### Extended PostgreSQL 17 production certification

- run targeted Miri on pure unsafe storage/page views and Linux ASan/TSan
  subprocess suites across pgrx/pg_sys, real mappings, callback containment,
  and concurrent readers;
- repeat crash-before-checkpoint, WAL replay and standby promotion, MVCC/HOT/TID
  reuse, VACUUM/REINDEX, generation replacement, backup/restore, and low-memory
  recovery matrices on one unchanged candidate;
- establish deterministic recall, latency, throughput, build-time, memory,
  page-fault, update-cost, cancellation, and filtered-search envelopes at
  useful collection sizes;
- run long fuzz campaigns and extended failpoint sweeps, minimize failures,
  and retain deterministic regressions;
- promote experimental capability labels only when the relevant evidence is
  complete. A GitHub V1 installation claim alone is not production
  certification.

### PostgreSQL-major and operating-system certification

The current support matrix is PostgreSQL 17 and 18 only. PostgreSQL 15 and 16
are not implicit future targets; adding either requires a separately approved
support-policy and pgrx/toolchain plan plus the complete evidence below.
PostgreSQL 19 is a forward-readiness target only: the
[PostgreSQL development roadmap](https://www.postgresql.org/developer/roadmap/)
currently plans its final release for September 2026, and pgrx has begun
publishing
[beta-era PG19 support](https://docs.rs/crate/pgrx/latest). Readiness evidence
is explicitly not a pgContext support, compatibility, package, or release
claim.

- verify real upgrade, rollback, dump/restore, `pg_upgrade`, and format
  rejection or migration paths from prior extension releases;
- deepen PostgreSQL 17 and 18 certification independently with exactly one pgrx
  feature and a real `pg_config` for each claimed major;
- once the exact pinned pgrx release supports the selected PostgreSQL 19
  beta/RC, run a non-blocking, allowed-to-fail readiness lane for compilation,
  generated-SQL/schema diff, extension install, exact/vector smoke, HNSW
  create/query/DML/VACUUM/REINDEX, crash restart, and artifact-format
  rejection. Pin the server prerelease precisely and advance it deliberately;
- after PostgreSQL 19 reaches RC, rehearse `pg_upgrade` from 18 and inventory
  changed extension, access-method, statistics, EXPLAIN, wait-event, AIO, and
  packaging APIs. Do not publish PG19 binaries/images or add it to the support
  matrix until GA plus a separately approved policy change and the same
  certification evidence required of 17 and 18;
- run full extension install and smoke gates on real Linux and macOS hosts;
- publish a support claim only for major/platform pairs backed by preserved
  evidence from the same candidate.

### Additional packages, images, and signatures

- produce reproducible native install packages only for certified
  major/platform pairs, then test install, uninstall, upgrade, and rollback from
  those packages rather than from a checkout;
- preserve per-major healthcheck, playground, provenance, and vulnerability
  evidence for the PostgreSQL 17 and 18 images on both supported architectures;
- add maintainer-controlled artifact and container signing with documented
  identity, timestamping, verification, rotation, and revocation procedures;
- attach signed checksums, SBOMs, provenance, and license inventories to future
  releases without committing credentials;
- maintain and release-test the shipped Evokoa Homebrew tap and PGXN
  distribution for every supported extension/PostgreSQL pair, including
  install, upgrade, uninstall, rollback, checksum, and source-build evidence;
- add APT/Yum repositories, native `.deb`/`.rpm` packages, or other registries
  only after their support, signing, upgrade, and rollback contracts are
  certified.

### Release cadence and maintenance

- define supported-version windows, security patch policy, deprecation policy,
  and a tested release rollback procedure;
- automate scheduled dependency and security-advisory review;
- keep expensive certification manual or infrequent, and retain bounded checks
  on ordinary pull requests;
- preserve complete benchmark and certification reports for each released
  version.

## Ecosystem and Framework Integrations

Status: planned after the relevant SQL/query surface is stable. Framework
support does not block core retrieval or 10M certification.

Depends on: stable collection/query contracts, the load-and-query workflow,
typed SQLSTATE/status behavior, versioned examples, and at least one certified
package usable in integration CI.

Scope:

- keep SQL and ordinary PostgreSQL drivers as the primary, complete integration
  contract; framework adapters remain thin translations and may not invent
  retrieval semantics unavailable through SQL;
- publish provider-neutral reference adapters and runnable examples for the
  leading Python and JavaScript AI retrieval ecosystems selected at
  implementation time, followed by other languages when maintained demand and
  contributor ownership justify them;
- cover registration, bulk backfill, named dense/sparse vectors, metadata
  filters, lexical/hybrid queries, recommendation/discovery, prefetch/rerank,
  streaming/keyset pagination, model-version aliases, recall checks, readiness,
  cancellation, and typed errors without requiring a second vector service;
- define one canonical request/result mapping and generate or mechanically test
  adapter-specific translations so score direction, filter missing/null/array
  semantics, limits, pagination, and source keys cannot drift by framework;
- provide minimal embedding-worker examples that use the provider-neutral
  external-worker contract; credentials, model calls, retries, and rate limits
  remain outside PostgreSQL and outside the core adapter;
- run a compatibility matrix against supported framework/client versions and
  PostgreSQL 17/18, with install-from-released-package tests rather than only
  workspace-path dependencies;
- document ownership, semantic-version policy, deprecation window, release
  cadence, and the behavior when an upstream framework changes or an adapter is
  no longer maintained.

Promotion is per adapter. It requires end-to-end tests against a real packaged
extension, exact/ANN/hybrid result fixtures shared with the SQL oracle, filter
and score-direction compatibility, cancellation/error tests, and a maintained
owner. An example repository or generated client without those gates remains
community/experimental rather than an official integration.

## Graph-Augmented Retrieval

Status: planned graph foundation with an experimental, explicitly opt-in beam
search track. It is dependency-ordered after stable source identifiers,
composite execution, mapped serving, and bounded stage budgets. Specific public
APIs and dates remain uncommitted.

Depends on: shared row/source identities, composite query execution, bounded
stage budgets, mapped-generation lifecycle, exact source reranking, and the
source/model provenance contract.

pgContext is built by Evokoa, which also maintains
[pgGraph](https://github.com/evokoa/pggraph), a graph extension for PostgreSQL.
The planned integration is deliberately **not** a runtime Rust-ABI link and does
not require the pgGraph extension to be installed. pgGraph remains an
independent product and the upstream design/source reference.

The goal is true graph-aware hybrid retrieval: topology participates in deciding
which candidates are explored, not merely in a SQL post-filter or final
reranking pass. PostgreSQL rows and relationships remain authoritative;
topology projections, ANN graphs, summaries, and search state are derived and
rebuildable.

### Integration decision: a selective pgContext-owned fork first

There are two viable source-sharing options, but they are not equal roadmap
branches:

| Option | Benefit | Cost | Roadmap decision |
|---|---|---|---|
| Refactor pgGraph so a small topology kernel becomes a separately versioned pure-Rust crate | One implementation and easier two-way fixes | Requires an up-front pgGraph refactor, a second release contract, coordinated compatibility testing, and tighter release coupling | Deferred convergence path |
| Port selected pgGraph modules into pgContext and adapt them behind a pgContext-owned boundary | Lets pgContext control its release cadence, artifact format, memory policy, and retrieval-specific evolution | Some fixes must be reviewed and carried between repositories manually | **Chosen for the first implementation** |

The initial implementation is therefore a **selective first-party fork**, not a
wholesale copy of pgGraph. pgContext owns the imported code after the port,
including its tests, format versions, lifecycle, and compatibility policy. A
shared crate becomes preferable only when all of the following are true:

- the reusable boundary has stabilized around topology primitives rather than
  either extension's SQL or PostgreSQL runtime;
- both repositories are repeatedly carrying the same fixes or algorithms;
- the crate can depend on no `pgrx`, SPI, PostgreSQL catalog, backend-global,
  query-executor, or extension-specific types;
- the crate can have independent semantic versions, exact dependency pins, and
  a compatibility suite without forcing synchronized pgContext and pgGraph
  releases.

If those conditions are later met, the shared crate replaces the internals of
the pgContext topology adapter without changing the query-owned retrieval
contract. The fallback is not to move a large catch-all portion of pgGraph into
a library; only the stable topology kernel qualifies. Direct Rust-ABI calls
into a separately loaded pgGraph extension are not a third option: private Rust
layouts, compiler and feature choices, panic behavior, allocator ownership,
PostgreSQL load order, and symbol visibility do not form a stable extension
boundary.

Every initial port records the pgGraph repository commit, source paths,
applicable license and notices, local changes, and validation fixtures. Updates
are manual, reviewed ports in either direction; there is no automatic subtree
merge and no promise that the two repositories move in lockstep. pgContext's
tests and performance gates are authoritative for code shipped by pgContext.

### pgContext crate and dependency boundary

The planned workspace split follows the boundaries already used by pgContext:

- a new `context-topology` crate owns pure topology identities, adjacency
  views, generation validation, bounded traversal primitives, and topology
  format versions; it depends only on `context-core` and ordinary pure-Rust
  dependencies;
- `context-query` owns beam state, work budgets, diagnostics, and a small
  topology-expansion port. `context-topology` does not depend on
  `context-query`;
- `context-build` constructs topology projections from authoritative edge
  rows, while `context-storage` provides generic versioned segment and mapping
  machinery. Neither layer decides retrieval ranking;
- `context-hybrid` owns graph/vector/lexical score composition and calibration;
- `context-pg` is the composition adapter. It joins the pure crates to
  PostgreSQL snapshots, SPI, catalogs, ACL/RLS checks, interrupts, collection
  lifecycle, and source-row exact rechecks.

The existing `CandidateSource` page contract remains the leaf-candidate
boundary. Incremental graph expansion needs a separate, query-owned port so a
beam can request a bounded neighbor batch for specific frontier states without
exposing topology storage objects to the executor. The concrete adapter lives
at the PostgreSQL edge and composes `context-topology` with `context-storage`.
This direction avoids circular dependencies and keeps pgrx out of the topology
kernel.

The topology artifact format is versioned separately from crate versions,
pgContext extension versions, and any pgGraph format. An unsupported or corrupt
generation is never interpreted optimistically: it reports
`rebuild_required`, and the authoritative rows are used to rebuild it.

### Exact pgGraph scope

The first source audit may select and adapt only these capabilities:

- stable node and relationship identity primitives that can map to pgContext's
  existing `SourceKey` and `PointId` authority;
- directed, typed, and optionally weighted adjacency with forward and reverse
  traversal;
- compact immutable adjacency images, active/tombstone state, and delta-overlay
  algorithms;
- bounded neighbor and frontier expansion, cycle/hub controls, generation
  watermarks, and deterministic traversal fixtures.

The acceptance boundary is capability-based, not a target percentage of the
pgGraph repository. The audit records the exact modules, lines, dependencies,
and tests selected. If the useful slice starts pulling in SQL parsing, catalog
state, or the graph database runtime, the port is too broad and must be reduced
or reimplemented behind the pure boundary.

The port adapts or reimplements these pgContext-specific responsibilities
rather than copying them from pgGraph:

- PostgreSQL snapshot, transaction, interrupt, ACL/RLS, partition, and
  authoritative source-row recheck behavior;
- mapped-generation lifecycle, eviction, memory accounting, artifact naming,
  rebuild, crash recovery, and publication;
- beam state, work admission, multi-provider scoring, dominance, exact rerank,
  diagnostics, and evidence-path assembly.

pgContext does not initially import pgGraph's SQL/GQL surface, catalog and
schema-management layer, general graph analytics, hydration APIs, job system,
background-worker framework, client/package surface, or complete graph database
runtime. The source audit rejects modules that cannot be separated from those
surfaces without pulling their dependency graph into pgContext.

A future optional adapter may ingest relationships authored through pgGraph by
using a documented, versioned SQL/schema contract. It must still materialize a
pgContext-owned projection; it never borrows pgGraph process memory or private
symbols, and the retrieval engine continues to work when pgGraph is absent. The
adapter publishes an explicit supported-version matrix and rejects unsupported
pgGraph versions before reading relationship data.

### Optional residency

The graph layer must be genuinely optional because even a compact topology can
add substantial resident memory. Each query supports `off`, `auto`, and
`required` topology modes, while each projection reports an explicit `absent`,
`cold`, `active`, `paused`, or `rebuild_required` lifecycle state. In `off`
mode pgContext maps no topology generation, allocates no topology frontier, and
runs no topology-maintenance worker for that collection; rebuildable on-disk
artifacts may remain. `auto` may lazily map a compatible generation within a
declared residency budget and fall back visibly. `required` fails with a typed
status rather than silently degrading. Mapped generations are pinned only for
the queries using them and become evictable afterward. Results and EXPLAIN-like
diagnostics state whether topology was used, skipped, or unavailable.

Runtime `off` is the required control because a compile-time Cargo feature does
not help a database that serves graph and non-graph collections in the same
process. A later build feature may omit graph code for smaller packages, but it
does not replace the runtime contract. The graph-disabled RSS gate measures
steady-state memory as well as allocations during query execution.

### Experimental virtualized beam search

"Virtualized beam search" is the roadmap name for a **multi-source lazy beam
search**; it is not presented as an established algorithm name. The engine
maintains one bounded logical beam across HNSW, topology, lexical, and
relational-anchor candidate providers while materializing graph pages, vectors,
and complete evidence paths only when expansion requires them.

Beam states remain compact: source-row identity, optional HNSW point and
topology-node identities, parent-state identity, transition kind, path-pattern
state, hop count, and score components. Parents live in a bounded arena so
states do not copy complete paths. Each round selects the best expandable
states, groups work by provider for batched page/neighbor access, scores newly
admitted states, applies authorization, cycle, hub, duplicate, and dominance
pruning, and retains the next bounded beam. Only selected finalists receive
authoritative row visibility checks, exact full-precision vector reranking, and
evidence-path reconstruction.

Correctness and resource limits are part of the algorithm, not tuning notes.
The request contract bounds beam width, expansion batch width, admitted states,
visited keys, vector expansions, topology edges/pages or bytes, hop depth,
exact reranks, evidence paths, parent-arena bytes, and elapsed time. Dominance
keys include row identity, path-pattern state, and authorization context rather
than collapsing every visit to the same node. Ranking preserves its vector,
lexical, relationship, path-progress, hop, hub, and duplicate components so the
final result is explainable and can be recalibrated without hiding provenance.
With topology `off`, the same executor creates no topology state and behaves as
an HNSW/vector-only search path.

### Staged delivery

1. **Boundary audit and import spike.** Inventory candidate pgGraph modules and
   transitive dependencies, create the source/license/provenance manifest, and
   port the smallest useful fixture into `context-topology`. Reject PostgreSQL
   coupling at the crate boundary and write differential tests against the
   pinned pgGraph behavior. The default result is the selective fork; promote a
   shared-crate extraction only if the convergence conditions above are already
   demonstrably true.
2. **HNSW-only experiment.** Put the existing bounded best-first HNSW traversal
   behind an internal lazy cursor, expose experimental beam controls and
   diagnostics, and demonstrate recall, ordering, cancellation, and memory
   parity with the existing path. This phase does not change public defaults.
3. **Virtual state and memory governance.** Add the parent arena, provider
   batching, dominance table, hard admission/visited budgets, explicit
   budget-exhaustion behavior, and per-component scoring telemetry while
   remaining vector-only.
4. **Topology projection and residency.** Add the versioned projection,
   build/publication path, query-owned expansion port, and runtime lifecycle.
   Prove that `off` is nonresident, `auto` fallback is observable, and
   `required` is deterministic before enabling graph-aware experiments outside
   development builds.
5. **Mixed and deep graph retrieval.** Add graph/vector transitions, typed path
   constraints, query-adaptive transition scoring, bidirectional or
   graph-seeded search where justified, and minimal evidence-subgraph
   reconstruction. Topology must be able to alter candidate discovery, not only
   rerank an already fixed vector set.
6. **Promotion decision.** Consider a stable API or default only after the
   mixed search clears the validation gates below. An experiment may be removed
   or redesigned without compatibility guarantees before promotion.

Scope:

- share stable row/point identifiers and typed relationship references without
  duplicating source records;
- support retrieval that seeds a bounded graph traversal, graph predicates that
  constrain retrieval, and staged interleaving of the two within one
  transaction;
- add relationship-aware candidate expansion and reranking with explicit
  depth, fan-out, path, candidate, memory, and timeout budgets;
- preserve PostgreSQL snapshots, ACL/RLS, partitioning, cancellation, and
  exact source rechecks at every traversal/retrieval boundary;
- return path and edge provenance with each expanded source so evidence
  assembly can explain why a row was included;
- build community summaries as the graph-structured counterpart of
  [hierarchical corpus summaries](#hierarchical-corpus-summaries), reusing the
  source-linked
  [summary-artifact contract](#summary-artifacts-and-deterministic-partition-rollups)
  as rebuildable derived artifacts, informed by
  [Microsoft GraphRAG](https://www.microsoft.com/en-us/research/project/graphrag/),
  while keeping the underlying rows and relationships authoritative;
- benchmark multi-hop relevance, citation/evidence coverage, latency, and graph
  work against vector/lexical retrieval alone on held-out questions.

Validation compares every bounded fixture with an exhaustive vector/topology
oracle and compares the graph-disabled path with the current HNSW baseline. It
also runs differential fixtures for every pgGraph-derived primitive against the
pinned source revision; this is test-time provenance, not a runtime dependency.
It records ANN recall, relevant-path and evidence-chain completeness, latency,
page faults, mapped/resident bytes, admitted/pruned states, exact rechecks, and
fallback reasons. Hub, cycle, paging-thrash, stale-generation, concurrent
mutation, crash/recovery, cancellation, partition, MVCC, and ACL/RLS fixtures
are mandatory. No graph-aware result bypasses the authoritative source-row
visibility and exact-score boundary.

Each capability receives its own maturity row and release notes only after it
clears those real-execution, bounded-work, permissions, mutation, recovery,
graph-off memory, and source-provenance gates. The direction is committed;
specific APIs and dates are not.

## Roadmap Change Policy

A roadmap feature moves into an implementation checklist only when it is
selected for a concrete release or milestone. At that point:

1. copy only the selected feature into a new dependency-ordered build plan;
2. define owners, prerequisites, child checkpoints, and executable gates;
3. update its public maturity classification;
4. do not weaken the real-execution, bounded-work, source-authority, or
   lifecycle requirements stated here;
5. retain the independent-implementation policy and record the primary
   research, derivation, dependency, and format provenance used by the work.
