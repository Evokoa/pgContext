# pgContext Product Roadmap

This document describes pgContext's product direction and release-engineering
plans following the installable GitHub V1 launch.

It outlines broad product direction, not an implementation checklist. Items
marked complete are implemented for 0.2.0; remaining items describe later work.

A feature listed here remains "planned" until its public capability row and
release notes announce its arrival. The document began as a post-V1 plan and
now records completed 0.2 work and remaining direction. A roadmap item only becomes release-blocking once it is
selected into a future dependency-ordered build or release plan.

## Frequently asked since 0.1.0

For transparency, here is where the capabilities most often requested after
0.1.0 stand in 0.2.0 and where the remaining work is headed:

- **Faster index builds** — pgvector currently builds HNSW indexes faster.
  Closing that gap is planned through parallel-build efficiency and
  construction-throughput work under
  [Delivery Phases](#delivery-phases-post-v1-overview); a concurrent builder
  already scales to roughly 3.3–3.5× at eight workers.
- **IVFFlat** — not implemented today. HNSW is pgContext's ANN index; an
  IVFFlat lifecycle stays on the roadmap, evaluated as user demand warrants,
  and the planned migration tooling already detects IVFFlat indexes and
  proposes an explicit retain, exact-search, or rebuild-as-HNSW plan. See
  [pgvector Migration and Compatibility](#pgvector-migration-and-compatibility).
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
- **Drop-in pgvector name compatibility** — the certified companion bridge now
  builds pgContext indexes on existing `vector`, `halfvec`, and bounded
  `sparsevec` columns without data movement; full unqualified name
  compatibility is sequenced later. See
  [pgvector Migration and Compatibility](#pgvector-migration-and-compatibility).

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
   compaction that rebuilds from the index's own pages. When the delta
   segment fills, the insert that fills it compacts the index inline,
   bounded by `pgcontext.hnsw_compact_on_threshold_max_mb`.

   The planned next step is **background-worker compaction**: moving the
   rebuild off the write path so writes are accepted at full speed while a
   separate process maintains the index, improving both sustained
   throughput and tail latency. It is sequenced after the multi-segment
   work below, because segmentation changes the unit of compaction from a
   whole graph to a single bounded segment, which is the right unit for a
   background worker to operate on.
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
4. **Broader pgvector compatibility.** The 0.2 bridge and resumable ownership
   conversion cover the certified PG17 profile. When pgvector is not installed,
   pgvector-spelled SQL (types, operators, opclasses, `USING hnsw`,
   familiar settings) runs unmodified, validated by running the pgvector
   regression suite in CI, plus an in-place adoption tool that converts
   columns without rewriting tables.
5. **Advanced retrieval features (implemented experimentally in 0.2).** Named
   sparse ANN with exact recheck, typed composite fusion, quantized candidates,
   internally maintained late interaction, mapped HNSW, and automatic
   observability now have bounded serving paths. Further certification remains.
6. **Production certification.** Model-checked and fuzz-tested
   concurrency, crash-recovery and replication test matrices, additional
   PostgreSQL major versions, progress reporting, and removal of
   experimental labels surface by surface as certification rows go green.
7. **Ecosystem.** Reproducible public benchmarks — including a corpus-size
   scaling benchmark and third-party-harness/x86 coverage (see
   [Reproducible Public Benchmarks](#reproducible-public-benchmarks)) —
   framework integrations, broader packaging, and scale-out deployment
   playbooks using standard PostgreSQL replication and partitioning.

## Dependency Order

~~~text
PG17 V1 freeze
├── non-dense ANN opclasses
│   └── named sparse ANN
│       └── large-dimension sparse vectors
├── quantized HNSW
│
├── non-dense ANN opclasses + quantized HNSW
│   └── pgvector migration and compatibility
├── internally maintained late interaction
│
├── non-dense ANN + quantized HNSW + named sparse ANN + late interaction
│   └── composite query execution
│
└── mapped HNSW serving
    └── composite query execution + mapped serving
        └── expanded automatic observability
~~~

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

## Quantized HNSW

Status: implemented experimentally for the PostgreSQL 17 profile.

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

## pgvector Migration and Compatibility

> pgContext is positioned as dedicated-engine-grade retrieval inside
> PostgreSQL, led by the registered-collection API. pgvector
> interoperability comes in two stages: **coexist mode** (install alongside
> pgvector and index existing `vector` columns directly) is the first
> deliverable, while full drop-in name compatibility is sequenced later as
> described below.

Status: the bounded PostgreSQL 17 coexistence and migration profile is
implemented and certified; IVFFlat remains an explicit detect-and-plan path.

Depends on: PG17 V1 freeze, non-dense ANN opclasses, and quantized HNSW.

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
- detect IVFFlat and generate an explicit retain, exact-search, or rebuild-as-
  HNSW plan. IVFFlat implementation itself remains an intentional non-goal
  unless measured user demand justifies owning a second ANN lifecycle;
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
- preserve ACL/RLS, drift, NULL/invalid-token, deleted-point, and source
  rechecks.

Validated by end-to-end serving tests before promotion.

## Composite Query Execution

Status: implemented for the PostgreSQL 17 profile.

Depends on: metadata-filtered ANN, quantized HNSW, named sparse ANN, and late
interaction.

Scope:

- execute the typed query IR rather than stopping at JSON constructors;
- compose dense, filtered, sparse, full-text, quantized, and late-interaction
  adapters without infrastructure dependencies in context-query;
- property-test weighted fusion, reciprocal-rank fusion, deduplication, ties,
  stage ordering, rerank, and exact oracles;
- deterministically cover empty/unavailable stages, malicious plans,
  cancellation, budget exhaustion, and semantic errors;
- add the query_plan fuzz target and bounded smoke.

Validated by end-to-end serving tests before promotion.

## Lexical Retrieval Enhancements

Status: planned after composite query execution.

Depends on: composite query execution and the shared fusion layer.

Today the full-text branch computes `to_tsvector('simple', column)` on the fly
and matches `plainto_tsquery('simple', ...)`. That is correct but minimal: a
fixed `simple` configuration (no stemming, stopwords, or language selection), no
stored `tsvector` column, no GIN/GiST full-text index in the fused path, and no
typo tolerance. This item makes lexical retrieval a first-class, configurable,
indexable fusion branch.

Scope:

- configurable full-text search: select the text-search configuration
  (language, stemming, stopwords) per registered text field instead of a
  hardcoded `simple`; accept `websearch_to_tsquery`/`phraseto_tsquery` query
  forms; and let a collection register a stored `tsvector` column so a GIN/GiST
  full-text index serves the branch instead of a scan-time `to_tsvector`;
- trigram fuzzy matching: add a `pg_trgm` similarity candidate source
  (`word_similarity`/`%`/`similarity`, backed by a GIN or GiST trigram index) so
  typo-tolerant and partial-token lexical retrieval can be fused alongside the
  dense, sparse, and full-text branches;
- expose both as typed query-IR branches composable through reciprocal-rank and
  weighted fusion, preserving the authoritative exact recheck and
  ACL/RLS/MVCC contracts on every returned candidate;
- expose counters proving the branch uses an index rather than scanning the full
  collection where an index is available.

Validated by an end-to-end serving test with exact-oracle and bounded-work
assertions before promotion.

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

## Expanded Automatic Observability

Status: implemented and lifecycle-gated for the PostgreSQL 17 profile.

Depends on: executable query outcomes and every serving strategy that it
reports.

Scope:

- automatically persist actual strategy, visits, candidates, filters,
  rechecks, quantization, fallback, latency, cancellation, and budget outcome;
- bound cardinality and exclude vectors, payload values, secrets, and tenant
  identifiers;
- cover success, typed error, cancellation, concurrent updates,
  rebuild-required, not-ready, and corruption.

The query backend uses a bounded nonblocking named-DSM queue; a database-scoped
worker commits observations independently so aborted statements can be
reported without adding a synchronous catalog write to query latency. Queue
health is restricted to `pg_monitor`, and delivery limitations are documented
as best-effort, may-duplicate, and fail-open. The PostgreSQL 17 gate covers the complete
outcome matrix above, privacy, strategy/work accuracy, disabled-vs-enabled
latency, queue health, and worker reclamation.

## Reproducible Public Benchmarks

Status: planned after V1.

Depends on: PG17 V1 freeze and the existing GloVe-100-angular comparison
harness.

The published pgContext-versus-pgvector comparison is measured at a single
corpus size (GloVe-100-angular, roughly 1.18M vectors) on Apple Silicon. Two
directions extend it into a claim that stands on its own:

- **Corpus-size scaling benchmark.** Run the same matched-deployment,
  matched-build, dataset-ground-truth methodology across a range of corpus
  sizes (for example 100k, 1M, and larger) and report how the query-latency
  ratio between pgContext and pgvector varies with corpus size. The goal is to
  measure the trend honestly — whether the advantage grows, holds, or narrows
  as collections grow — rather than to assume it, and to state plainly where
  each engine leads (pgvector currently builds faster, for instance).
- **Third-party harness and hardware coverage.** Publish results from a
  neutral, community-recognized harness (such as ann-benchmarks) and on x86-64
  hardware exercising the AVX2 kernels, so the numbers do not depend on a
  single machine, operator, or CPU architecture.
- **Full benchmark suite.** One reproducible suite that runs the complete matrix
  in a single pass — unfiltered vector search across corpus sizes, filtered
  search across selectivities (including registration-only versus an added
  filter index, to quantify the optional-index speed-up), hybrid retrieval,
  build time, and memory — so every published claim traces back to one archived
  run rather than a collection of ad-hoc lanes.

Each result is archived with its dataset, ground-truth source, deployment,
build budget, hardware, and date, and is regenerable from a committed script.

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

- verify real upgrade, rollback, dump/restore, `pg_upgrade`, and format
  rejection or migration paths from prior extension releases;
- deepen PostgreSQL 17 and 18 certification independently with exactly one pgrx
  feature and a real `pg_config` for each claimed major;
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
- add a Homebrew tap and other package registries only after their support and
  rollback contracts are certified.

### Release cadence and maintenance

- define supported-version windows, security patch policy, deprecation policy,
  and a tested release rollback procedure;
- automate scheduled dependency and security-advisory review;
- keep expensive certification manual or infrequent, and retain bounded checks
  on ordinary pull requests;
- preserve complete benchmark and certification reports for each released
  version.

## Graph-Augmented Retrieval

Status: planned direction — specific features and sequencing still to be scoped;
not yet dependency-ordered.

pgContext is built by Evokoa, which also maintains
[pgGraph](https://github.com/evokoa/pggraph), a graph extension for PostgreSQL.
Because both keep their data in ordinary PostgreSQL tables rather than a
separate store, we plan to bring graph capabilities from pgGraph into pgContext
for **graph-augmented retrieval**: use pgContext to find the most relevant rows
by vector and hybrid search, then expand or re-rank those results along the
relationships modeled in pgGraph — the retrieval pattern behind connected-data
and GraphRAG applications, without copying data between two systems or
reconciling two sources of truth.

Likely building blocks include shared row/point identifiers across the two
extensions, relationship-aware re-ranking of retrieved candidates, and
retrieval that seeds a graph traversal (or vice versa) within a single
transaction.

Each capability will be scoped, designed, and validated on its own, and appears
as a planned feature — with a capability row and release notes — once it clears
the same real-execution and correctness bar as everything else here. The
direction is committed; specific APIs and dates are not yet.

## Roadmap Change Policy

A roadmap feature moves into an implementation checklist only when it is
selected for a concrete release or milestone. At that point:

1. copy only the selected feature into a new dependency-ordered build plan;
2. define owners, prerequisites, child checkpoints, and executable gates;
3. update its public maturity classification;
4. do not weaken the real-execution, bounded-work, source-authority, or
   lifecycle requirements stated here.
