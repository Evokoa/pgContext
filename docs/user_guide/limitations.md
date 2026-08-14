# Known Limitations

This page describes the important limitations of pgContext 0.3.0. The
[Supported Features](supported_features.md) inventory is the canonical maturity
reference; planned-only work remains in the [roadmap](roadmap.md).

## Compatibility and installation

- 0.3.0 is a clean-install baseline. There is no 0.1-to-0.3 or 0.2-to-0.3
  extension update script.
- The separate `pgcontext_pgvector` companion extension is retired. pgvector
  compatibility is enabled through the main extension.
- Experimental HNSW, IVFFlat, codec, segment, and generated-artifact formats
  may require `REINDEX` or regeneration from authoritative rows.
- PostgreSQL 17 and 18 are supported. Other PostgreSQL majors are not supported
  release targets.

See the [release migration procedure](release_notes.md#compatibility-and-migration)
before replacing an existing installation.

## Vector types and indexes

- Dense `vector` SQL and exact search are Stable. The `halfvec`, `sparsevec`,
  `bitvec`, `int8vec`, and `uint8vec` source surfaces remain Experimental even
  where individual opclass names and metric bindings are stable contracts.
- The general page-native `pgcontext_hnsw` format remains Experimental.
  Quantized HNSW serving has a Stable source-reranked contract, but that does
  not promote every HNSW storage or maintenance path.
- Densified HNSW records must fit the documented PostgreSQL page envelope.
  Effective indexable dimensions depend on representation, graph degree, and
  layer shape; oversized records fail before publication.
- Segmented HNSW writes bound delta and compaction work, but a write that
  triggers rotation or pair compaction still pays that maintenance latency.
- Native `pgcontext_ivfflat` is Experimental. Its v4 pages intentionally reject
  earlier development formats, and the retained one-million/ten-million
  comparative certification remains open.

## Retrieval maturity

- Named sparse ANN and internally maintained late interaction remain
  Experimental. Both recheck authoritative source values exactly after
  candidate generation.
- Adaptive-dimension retrieval preserves exact full-dimension results but its
  current scan-based schedule has no demonstrated latency benefit.
- Exact-first readiness preserves exact availability and passes its correctness
  gates, but misses the current indexed/building query-latency and temporary
  storage promotion ceilings.
- Trigram fuzzy retrieval is optional and Experimental. `pg_trgm` is not an
  installation requirement.
- The lazy HNSW cursor and virtual beam engine are Internal. The beam has no SQL
  or planner surface in 0.3.0, remains vector-only, and rejects topology
  expansion.

## Semantic models and chunking

- pgContext does not bundle model weights. Deterministic fixtures remain the
  default CI and worker contract.
- Semantic reranking is Experimental. The detached envelope and PostgreSQL
  finalization boundary are implemented and a revision-pinned MiniLM
  cross-encoder passes the optional local smoke, but a general transformer
  backend is not integrated into `pgcontext-worker`.
- Automatic chunking is Experimental. Its publication schema still uses
  deterministic fixture embeddings. The optional real-model smoke proves
  indexed 384-dimension retrieval with citation preservation, not a production
  embedding-job integration.
- Stable semantic promotion still requires retained held-out quality,
  cancellation/timeout and authority-churn coverage, and hosted platform
  evidence across supported architectures.
- The frozen one-million-row chunk-publication lanes miss the declared 1,000
  chunks/second promotion floor.

## Operations and resources

- PostgreSQL rows remain authoritative. Generated indexes, codec files, mapped
  segments, model downloads, and worker reports are rebuildable or disposable
  artifacts, not backups.
- Memory and work limits are conservative and fail closed. A request can return
  a typed incomplete or budget-exhausted outcome even when PostgreSQL itself
  still has resources available.
- Automatic query telemetry is bounded and content-free but best-effort.
  Contention, worker restart, queue saturation, or database-slot exhaustion can
  lose an observation; the commit/acknowledgement window can duplicate one.
- Generated artifact cleanup handles registered, root-confined files only. It
  does not follow symlinks, recursively delete directories, or repair arbitrary
  catalog paths.
- PostgreSQL 17 remains the primary benchmark and deep-lifecycle target even
  though PostgreSQL 18 is supported and passes the complete integration suite.

## Security boundary

- SQL-visible paths apply PostgreSQL ACL/RLS and current-row rechecks as
  documented. Applications still own source-table grants, RLS policy quality,
  model licensing, model-input policy, and output sanitization.
- `ts_headline` output and other caller-rendered snippets must be sanitized for
  their final HTML or UI context.
- Optional model downloads require explicit operator action and live outside
  the source and release package. Their recorded revision and artifact digest
  must match before inference.
