# pgContext User Guide

pgContext is an open-source PostgreSQL extension for AI vector and hybrid
retrieval.

This guide distinguishes stable, implemented behavior from experimental and
planned paths. pgContext 0.2.0 targets PostgreSQL 17.

## Current Status

The repository currently exposes dense vectors, array-based exact search,
collection catalog APIs, dense vector-column registration, stable point ID
mappings, basic exact search over registered table-backed collections, stable
scroll cursors, filter-first exact search, filtered facets, and dense plus
full-text hybrid retrieval with reciprocal rank fusion. The Rust core also has
half-vector parsing and distance metrics plus sparse-vector canonicalization;
bit-vector Hamming and Jaccard distances are also implemented in core.
Named dense vector registration and search-by-name selection are part of the
stable table-backed search surface. Stable metadata functions expose
per-vector HNSW/quantization/status containers, but the option semantics remain
experimental. Named sparse vector registration and
storage/index/status metadata are also SQL-visible experimentally. Exact sparse
top-k over explicit arrays and named sparse source columns is available through
`pgcontext.search_sparse`, and exact dense+sparse RRF fusion is available
through `pgcontext.query`. Named sparse search can bind a metric-matched HNSW
index for bounded candidates with exact source rerank and exact fallback.
Experimental SQL wrappers expose `halfvec`, `sparsevec`, and `bitvec`
input/output, typmods, dimension helpers, exact distance helpers, and distance
operators. `halfvec` also has explicit rounding numeric-array casts and aggregates; `sparsevec`
also has structured construction, dense `real[]`/`vector` casts, and aggregates.
`bitvec` also has `boolean[]`, PostgreSQL `bit`, and PostgreSQL `bit varying` casts plus
pgvector-compatible built-in `bit` distance functions/operators and bitwise
OR/AND aggregates. All three variant types also install default btree ordering
opclasses. The first-class non-dense HNSW surface covers L2, inner product,
cosine, and L1 for both `halfvec` and `sparsevec`, plus explicit Hamming and
Jaccard opclasses for `bitvec`. Default `pgcontext_hnsw` attempts on `bitvec`
columns still fail with SQLSTATE `42704`, requiring callers to select the
intended bit metric.

## Capability Areas

- Collections over ordinary PostgreSQL tables.
- Named dense vector registration/search and experimental per-vector planner
  metadata.
- Qdrant-style filter JSON over ordinary columns and JSONB metadata.
- Exact search as the correctness baseline.
- Hybrid dense plus full-text retrieval and experimental exact dense+sparse
  fusion.
- Experimental persisted dense HNSW indexes and adaptive filtered ANN, with
  exact source rechecks and bounded PostgreSQL 17 lifecycle evidence.
- Stable metric-bound HNSW opclass names for half and sparse L2, inner product,
  cosine, and L1, plus bit Hamming and Jaccard; their SQL types and the HNSW
  on-disk format remain experimental.

## Advanced and Intentionally Different Capabilities

Named sparse ANN, revision-bound quantized candidate traversal with exact
reranking, internally maintained late-interaction tokens, typed composite query
execution, immutable mapped graph generations, and bounded automatic executor
telemetry are implemented. These advanced paths retain the maturity labels and
operational limits documented in their individual guides. IVFFlat is
intentionally not part of pgContext's V1 product.

Their dependency order and acceptance requirements are in the
[post-V1 product roadmap](roadmap.md).

## Implemented Core Behavior

- [Installation](installation.md)
- [Configuration](configuration.md)
- [Packaged playground](playground.md)
- [Collections](collections.md)
- [Production quickstart](quickstart.md)
- [SQL API contract](api_reference.md)
- [Dense vectors and exact search](vector_search.md)
- [Multi-tenancy runbook](multi_tenancy.md)
- [Client-facing examples](client_examples.md)
- [Filters](filters.md)
- [Hybrid retrieval](hybrid_retrieval.md)
- [Retrieval methods overview](retrieval_methods.md)
- [Indexes](indexes.md)
- [Rebuildable storage artifacts](storage.md)
- [Operations and support](operations.md)
- [Troubleshooting and maintenance runbook](troubleshooting.md)
- [Known limitations](limitations.md)
- [pgvector and Qdrant parity matrix](parity_matrix.md)
- [Exact metric and operator matrix](metric_operator_matrix.md)
- [Metric definitions and edge-case semantics](metric_semantics.md)
- [Post-V1 product roadmap](roadmap.md)
- [Installed SQL object and option inventory](sql_object_inventory.md)
- [Migrating from pgvector](pgvector_migration.md)
- [Support, version, upgrade, and deprecation policy](support_policy.md)
- [Rollback and repair plan](rollback.md)
- [First production release notes](release_notes.md)
- [Error categories and SQLSTATEs](errors.md)
- [Security model](security.md)

## PostgreSQL Support

PostgreSQL 17 is the supported V1 release target. PostgreSQL 15, 16, and
18 require their later version-specific gates; PostgreSQL 14 is legacy
best-effort only.
