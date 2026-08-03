# Selected Roadmap Implementation Contract

This document freezes the cross-cutting decisions for the C1, C3, I2-I5, I7,
I12, Q1, Q4, Q6-Q10, G1-G8, and PostgreSQL production-certification program.
It is an implementation contract for contributors. User-facing maturity remains
defined by the capability registry and the supported-features inventory.

The program is a clean break. It targets a new `0.3.0` clean-install baseline
and does not preserve pgContext 0.1 or 0.2 Rust APIs, SQL APIs, catalog layouts,
GUC names, JSON request shapes, or experimental derived-artifact formats.
PostgreSQL source rows remain authoritative and must survive rebuilds,
dump/restore, recovery, and derived-artifact rejection. Tables using only
PostgreSQL-owned types also survive a non-cascading extension removal. Tables
using pgContext-owned source types require an explicit export or cast migration
before removal; pgContext never promises that `DROP EXTENSION ... CASCADE`
preserves dependent objects.

## Scope interpretation

The requested `W2` label is treated as roadmap item O2, PostgreSQL production
certification, because the roadmap inventory has no W family. Relabeling that
item does not alter the dependency order or its evidence gates.

The following narrow enabling slices are part of the program because selected
features cannot be completed without them:

- set-based mapping, validation, and projection operations from C2;
- the generic background build and compaction worker from C4;
- bounded parallel and external-build primitives from C6;
- internal structural format verification from C7;
- the existing mapped-serving foundation from I9;
- strict per-stage work budgets from QP5;
- the source-linked summary artifact contract needed by G7 from R4; and
- the benchmark datasets and reports needed for selected 1M/10M gates from O1.

These slices do not promote the broader roadmap items by themselves.

## Decision 1: canonical ownership

Every retrieval fact has exactly one owning crate. Callers use the owner's
public types instead of declaring parallel enums or string vocabularies.

| Owner | Canonical responsibility |
|---|---|
| `context-core` | logical IDs, source authority, vector representation, metric and score order, index kind, generation/configuration/profile IDs, readiness reasons |
| `context-codec` | codec specifications, trained codec state, encoded contiguous views, query scorers, codec validation |
| `context-filter` | registered filter AST and safe predicate rendering |
| `context-hybrid` | pure fusion and score calibration over canonical candidates |
| `context-index` | HNSW, IVFFlat, cursor, and index-algorithm behavior |
| `context-topology` | topology IDs, typed weighted adjacency, overlays, bounded traversal primitives, topology format descriptors |
| `context-query` | query IR, candidate/provenance envelopes, budgets, execution, rerank and topology-expansion ports, completion status |
| `context-build` | job and generation state machines, build planning, publication protocol, bounded build primitives |
| `context-storage` | byte layouts, checksums, validated mappings, atomic artifact file operations |
| `context-pg` | SQL, catalogs, SPI, PostgreSQL background workers, ACL/RLS/MVCC rechecks, SQLSTATE translation, AM callbacks |
| `pgcontext-worker` | external parsing, tokenization, inference/provider transport, and supervised asynchronous work for Q7/Q8 |

Ports are declared by their consumer. Implementations live at the outer edge.
Pure crates never depend on pgrx, PostgreSQL catalogs, SPI, SQLSTATEs, or the
external worker runtime.

## Decision 2: workspace boundaries

Two pure library crates and one later external process are justified:

- `context-codec` depends only on `context-core` and ordinary pure-Rust
  dependencies. It does not own files, SQL, catalogs, or publication.
- `context-topology` depends only on `context-core` and ordinary pure-Rust
  dependencies. It does not own query ranking, SQL, catalogs, or pgGraph
  runtime integration.
- `pgcontext-worker` is introduced only when Q7/Q8 requires it. The package has
  a library plus binary, owns its Tokio runtime, supervises every task, and
  communicates through versioned bounded envelopes.

Before dispatching work outside PostgreSQL, `context-pg` applies current MVCC,
ACL, and RLS checks and projects only a registered, bounded field allowlist.
The external worker receives neither database credentials nor direct table
access. Its responses are untrusted inputs: identities, revisions, sizes,
authorization context, and result contents are validated again at ingress and
final source rows are rechecked before use.

`context-pg` must not depend on Tokio or run a general asynchronous runtime in a
PostgreSQL backend. PostgreSQL background work uses PostgreSQL processes and
durable leased jobs. CPU-only parallel work may use a bounded backend-local
pool only behind host-wide admission tokens. Every worker has a retained join
handle, cooperative cancellation, and a backend-exit join; PostgreSQL APIs and
borrowed backend memory never cross to its worker threads. Admission failure is
visible and falls back or fails according to the typed execution policy.

## Decision 3: source authority and result correctness

An ordinary visible PostgreSQL row is the final authority for every result.
Provider-native `int8vec`, `uint8vec`, and `bitvec` values can be authoritative
source representations. Codes trained or generated from another source vector
are derived artifacts.

Every approximate or externally scored path must:

1. produce bounded candidates with explicit provenance;
2. resolve each candidate to the current visible source row;
3. enforce ACL, RLS, MVCC, registered predicates, and source/profile identity;
4. compute or validate the authoritative final score required by the contract;
5. discard stale, missing, unauthorized, non-finite, or mismatched rows; and
6. report incomplete, degraded, or exhausted execution explicitly.

No index, codec, topology projection, summary, worker result, or cached row is a
second source of truth.

## Decision 4: candidate and provenance contract

The query-owned candidate envelope carries, at minimum:

- logical point and stable occurrence IDs;
- candidate source and branch identity;
- approximate and optional exact scores with canonical score order;
- vector/profile/configuration and artifact generation IDs when applicable;
- source authority and current source version;
- bounded diagnostics and work counters; and
- completion/degradation status separate from an empty result set.

Fusion never compares raw scores from unrelated models or rankers. Cross-source
fusion uses rank-based or explicitly calibrated contracts, and final ties are
deterministic.

## Decision 5: one generation protocol

All rebuildable artifact kinds share the `context-build` publication protocol:

~~~text
planned -> building -> staged -> validating -> ready_to_publish
        -> published -> retired -> reclaimed
~~~

Cancellation, failure, supersession, lease expiry, and restart are explicit
transitions. Repeating a completed transition is idempotent. Publication is the
only visibility cutover, and readers either pin one complete generation or use
the previous complete generation. They never assemble a mixed generation.
The snake_case labels shown above are the canonical Rust and serialized state
spellings; prose may hyphenate ordinary English but may not define aliases.

Reader pins are separate leases, not lifecycle states. A published or retired
generation may have zero or many live pins. Reclamation requires both the
`retired` state and no live reader pins; pin expiry and release are idempotent.

Artifact-specific data stays in typed manifests referenced by the generic
generation record; it does not expand the lifecycle enum with per-feature
states.

## Decision 6: clean storage and SQL break

The new line uses extension version `0.3.0`. There is no 0.1/0.2-to-0.3 upgrade
script and no old pgContext artifact reader. Historical install and upgrade SQL
belongs on historical release tags, not in the new install line. The separate
`pgcontext_pgvector` companion extension is removed in 0.3.0; I12 compatibility
surfaces are owned and versioned by the main `pgcontext` extension. Existing
companion-extension installations must unload it before a clean 0.3.0 install.

Each new derived format has its own magic, version, checked lengths, allocation
budgets, integrity fields, and artifact kind. Recognized old formats and unknown
future versions return `rebuild_required`; they are never interpreted using a
best-effort layout. Rebuild uses current authoritative rows.

The clean break does not waive:

- PostgreSQL 17 and 18 support;
- same-version dump/restore, physical recovery, replication, VACUUM, REINDEX,
  crash restart, partitioning, and concurrent DML requirements;
- source-table ownership and extension-drop survival; or
- I12's externally visible pgvector compatibility and migration contract.

## Decision 7: SQL naming

The canonical integer source types are `pgcontext.int8vec` and
`pgcontext.uint8vec`; `pgcontext.bitvec` remains the canonical binary source
type. The names describe coordinate width rather than PostgreSQL's `int8`
scalar alias.

Native access methods are `pgcontext_hnsw` and `pgcontext_ivfflat`. I12 may
provide unqualified pgvector-compatible `hnsw` and `ivfflat` facade names only
when those names are not owned by another installed extension. It must fail
explicitly on conflicts and must never silently retarget an existing index.

Model, chunk, codec, and topology configuration is immutable and identified by
a canonical content hash plus a typed revision ID. Mutation creates and
publishes a new revision.

## Decision 8: errors, unsafe code, and allocation

Library crates expose typed errors. `context-pg` is the only SQLSTATE mapping
edge, and `pgcontext-worker` adds application context without erasing typed
protocol failures.

Unsafe code remains denied by default. `context-topology`, `context-query`,
`context-build`, `context-hybrid`, and the worker domain library forbid unsafe
code. `context-storage` and `context-pg` retain their narrow audited opt-ins.
`context-core` retains only its existing audited SIMD-kernel opt-in and scalar
oracle; it does not become a general unsafe owner. `context-codec` begins safe;
a measured SIMD kernel may add one isolated safe wrapper after scalar-oracle,
architecture, Miri, and sanitizer evidence.

Every allocation or work count derived from SQL, files, network envelopes, or
catalog data is checked for overflow and against a single-source policy limit
before allocation or iteration.

## Decision 9: graph ownership and provenance

Graph retrieval is a selective pgContext-owned source port from pgGraph, not a
runtime Rust ABI and not an installation dependency. The port imports only pure
identity, normalization, adjacency, overlay, and traversal primitives that
survive the crate boundary audit.

Imported code records the exact pgGraph commit, source paths, license, local
adaptations, and differential fixtures. pgContext owns the resulting types,
format, tests, lifecycle, and release cadence. pgGraph SQL, GQL, catalogs,
backend globals, extension entrypoints, and file formats are excluded.

Topology is optional at runtime. `off` maps no topology generation, allocates no
topology frontier, and starts no topology worker for the collection. `auto`
falls back visibly. `required` returns a typed unavailable or rebuild-required
outcome.

Topology generations are permission-scoped and may contain only relationships
visible under their recorded authorization scope. Every relationship used for
traversal, ranking, explanation, or GraphRAG evidence is resolved back to its
current relationship row and rechecked for MVCC, ACL, and RLS before it can
affect or justify a result. Missing, stale, or unauthorized relationships are
discarded and may make bounded execution explicitly incomplete.

## Decision 10: promotion is evidence-driven

Implementation does not equal promotion. A capability is added or changed in
`supported_features.md` only when its code, installed SQL, focused tests,
lifecycle/recovery gate, security boundary, detailed user documentation, and
capability-contract row agree.

| Stage | Minimum evidence |
|---|---|
| Internal | pure behavior and invariant tests; not installed or documented as available |
| Experimental | installed surface, focused tests, lifecycle/recovery evidence, explicit limits and diagnostics |
| Stable | PG17/18 matrix, compatibility policy, upgrade policy for the new line, concurrency/security/backup evidence, frozen SQL contract |
| Production-certified | architecture/platform matrix, 1M/10M thresholds where applicable, long-duration and failure testing, preserved reproducible reports |

Performance claims require a pinned baseline and raw evidence. A regression is
not waived because a feature is otherwise functionally complete.

Selected capabilities use the following frozen promotion ownership. A target is
the minimum evidence level required to close its phase, not permission to claim
broader composition. Numeric thresholds are declared in the named phase-owned
`context-test` manifest before the candidate implementation is benchmarked;
reports record the manifest hash, dataset hash, hardware, GUCs, and raw samples.

| Roadmap / capability-contract ID | Target | Capability-specific evidence and threshold authority |
|---|---|---|
| C1 / `CAP-CANONICAL-RETRIEVAL` | Internal | Exhaustive registration, property/differential parity, crate-boundary, allocation, binary-size, and existing exact/HNSW no-regression gates; P1 manifest owns thresholds. |
| C3 / `CAP-SEGMENTED-HNSW` | Stable | Segment rotation/compaction/restart/crash/VACUUM/concurrent-DML tests, bounded fan-out and resident bytes, 1M recall/QPS/build/write-amplification gates; P3 manifest owns thresholds. |
| I2 / `CAP-INT8-VECTOR` and I3 / `CAP-BINARY-VECTOR` | Stable | SQL type/cast/operator/opclass matrices, malformed binary inputs, dump/restore and HNSW lifecycle parity; P4 matrix owns exactness and performance limits. |
| I4 / `CAP-QUANTIZED-HNSW` and I5 / `CAP-QUANTIZED-SERVING` | Stable | Scalar-oracle differential tests, corrupt-code rejection, generation publication/recovery, exact rerank, per-codec recall/latency/memory at 1M; P5 codec manifest owns thresholds. |
| I7 / `CAP-IVFFLAT-AM` | Stable | Native AM planner/scan/build, deterministic training, DML/VACUUM/REINDEX/crash/replication, filtered recall and 1M/10M build/latency/memory; P6 IVF manifest owns thresholds. |
| I12 / `CAP-PGVECTOR-MIGRATION` | Stable | Conflict-safe facade creation, operator/opclass/DDL parity, coexist/adopt/rebuild/rollback/dump-restore matrices on PG17/18; P7 compatibility matrix is authoritative. |
| Q1 / `CAP-COMPOSITE-QUERY` | Stable | Typed IR validation, global budgets, deterministic fusion/provenance, RLS/ACL/MVCC and incomplete-result tests; P8 execution manifest owns work and latency ceilings. |
| Q9 / `CAP-LEXICAL-RETRIEVAL` | Stable | PostgreSQL parser/dictionary/ranking parity, generated/triggered maintenance, transactional visibility, fusion and language/config matrices; P9 manifest owns latency thresholds. |
| Q4 / `CAP-ADAPTIVE-DIMENSION` | Stable | Immutable profile validation, prefix/normalization oracle, profile isolation, mixed-generation rejection and recall/latency curves; P10 profile manifest owns thresholds. |
| Q6 / `CAP-MULTI-MODEL` | Stable | Immutable model profiles, calibrated/rank fusion, missing-model degradation, tenant/RLS isolation, dimension mismatch and deterministic ties; P11 profile matrix owns limits. |
| Q7 / `CAP-SEMANTIC-RERANK` | Stable | Provider-neutral contract, pre-dispatch authorization/allowlist, untrusted-response validation, timeout/retry/circuit-breaker, no-egress mode and quality/latency/cost budgets; P12 provider fixture owns thresholds. |
| Q8 / `CAP-AUTOMATIC-CHUNKING` | Stable | Deterministic tokenizer/chunker fixtures, source linkage, trigger/worker rollback, concurrent edits, RLS/ACL, restart and backfill bounds; P13 corpus manifest owns throughput limits. |
| Q10 / `CAP-EXACT-FIRST-READINESS` | Stable | Immediate exact availability, visible readiness reasons, concurrent build/cancel/restart/rebuild, no duplicate/missing writes and automatic cutover; P14 readiness manifest owns convergence limits. |
| G3 / `CAP-LAZY-HNSW-CURSOR` and G4 / `CAP-VIRTUAL-BEAM` | Internal | Differential HNSW parity, deterministic cursors, strict work/memory budgets, serialization rejection and graph-off parity; P15/P16 manifests own overhead ceilings. |
| G1 / `CAP-TOPOLOGY-KERNEL` | Internal | Selective-port provenance, license audit, differential adjacency/traversal fixtures, malformed input and bounded-work properties; P17 fixtures are authoritative. |
| G2 / `CAP-TOPOLOGY-RESIDENCY` | Stable | Permission-scoped projection, MVCC/ACL/RLS edge recheck, publication/pins/recovery, off/auto/required behavior and zero-allocation off mode; P18 manifest owns residency limits. |
| G5 / `CAP-MIXED-BEAM` and G6 / `CAP-DEEP-TOPOLOGY` | Stable | Deterministic mixed expansion, cycle/depth/fan-out budgets, stale-edge rejection, topology-off parity, adversarial graphs and recall/latency curves; P19/P20 manifests own thresholds. |
| G7 / `CAP-GRAPH-COMMUNITIES` | Stable | Deterministic communities, source-linked summary invalidation, permission scope, generation recovery and quality/size bounds; P21 graph-corpus manifest owns thresholds. |
| G8 / `CAP-GRAPHRAG` | Stable | Evidence provenance, relationship revalidation, hallucination/answer-quality corpus, tenant isolation, degraded modes, latency and cost; P22 GraphRAG corpus owns thresholds. |
| O2/W2 / `CAP-PRODUCTION-CERTIFIED` | Production-certified | PG17/18 architecture/platform matrix, 1M/10M gates, long-duration concurrency, crash/backup/restore/replication/security tests and signed report index; P22 certification manifest is authoritative. |

## Phase order

The dependency spine is fixed:

1. canonical retrieval contracts;
2. generic generation, job, and certification infrastructure;
3. segmented serving, integer/binary sources, codecs, IVFFlat, and pgvector
   compatibility;
4. composite, lexical, adaptive-dimension, multi-model, reranking, chunking,
   and exact-first query work;
5. lazy HNSW cursor and virtual beam foundations;
6. topology kernel/residency, mixed and deep topology retrieval, and graph
   community summaries; and
7. GraphRAG plus PostgreSQL production certification.

Later phases may develop in parallel only after every contract they consume is
merged and frozen. Each phase ends with focused tests, an independent review,
review fixes, capability documentation reconciliation, and a scoped commit.
