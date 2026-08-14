# pgContext 0.3.0 — Bounded, Composable Retrieval

pgContext 0.3.0 is a clean-install release for PostgreSQL 17 and 18. It keeps
ordinary PostgreSQL rows authoritative while expanding the extension from its
0.2 vector-search foundation into bounded lexical, multi-model, reranking,
chunking, IVFFlat, and internal multi-source query infrastructure.

This is not an in-place upgrade from pgContext 0.1 or 0.2. Read
[Compatibility and migration](#compatibility-and-migration) before replacing an
existing installation.

## Highlights

### One bounded query architecture

The typed query executor now composes dense, sparse, lexical, fuzzy,
quantized, late-interaction, recommendation, discovery, lookup, prefetch,
threshold, formula, and finalization stages. Candidate, comparison, expansion,
memory, depth, result, and elapsed-time limits are global across the complete
query rather than resetting inside each branch.

Every approximate or externally scored result is resolved back to the current
visible source row. MVCC, deletion state, registered filters, ACL, RLS, profile
identity, source version, and authoritative final scoring remain in force.

### Stable multi-model retrieval

`pgcontext.query_multi_model` can query active and draining embedding profiles
with different representations, dimensions, and metrics over one stable point
namespace. It combines one-based ranks through bounded weighted reciprocal-rank
fusion; it never compares unrelated native model scores. Missing profiles are
either a typed failure or an explicitly degraded result according to caller
policy.

The frozen equal-weight one-million-row contract passes on PostgreSQL 17 and
18 without reducing recall below the stronger individual profile at the same
declared global budget.

### PostgreSQL-native lexical retrieval

Registered lexical sources support weighted text fields, JSON paths, stored or
generated `tsvector` columns, typed query forms, optional GIN/GiST acceleration,
and bounded `ts_headline` hydration. Index, collation, configuration, expression,
and opclass drift fail closed. Final rows are reread under current MVCC and RLS.

Optional `pg_trgm` fuzzy retrieval is implemented but remains Experimental.

### Quantized and segmented HNSW

Quantized HNSW serving is Stable for binary, scalar/SQ8, and product codecs.
Versioned, checksummed segment artifacts bind trained state to their source
generation and always request exact source reranking. The retained one-million-row
release lane passes its recall, latency, memory, size, publication, maintenance,
and restart gates on PostgreSQL 17 and 18.

Segmented HNSW serving, bounded compaction, delta overlays, integer-vector
source types, and their lifecycle machinery are implemented but remain
Experimental where documented. Older experimental HNSW artifacts are not read
as current data; rebuild them from authoritative rows with `REINDEX`.

### Native IVFFlat

The new `pgcontext_ivfflat` access method supports dense, half, signed and
unsigned 8-bit, and bit-vector representations with metric-specific opclasses.
It includes bounded probes and widening, memory-bounded external construction,
DML deltas, VACUUM, compaction, concurrent index creation, partitions,
dump/restore, crash replay, physical replication, and exact source reranking.

IVFFlat remains Experimental pending the retained one-million and ten-million
comparative release campaigns. Its clean v4 pages intentionally reject earlier
development formats and require `REINDEX`.

### Semantic reranking without bundled weights

The Experimental reranking contract is split into two PostgreSQL operations:

1. `prepare_semantic_rerank` releases a bounded envelope containing only text
   that is currently visible and authorized.
2. `finalize_semantic_rerank` validates the untrusted response, rejects added or
   duplicate candidates, and rereads every result under current source hash,
   version, deletion, filter, ACL, and RLS state.

The source distribution includes the deterministic, no-network
`pgcontext-worker` fixture runtime. It does not bundle model weights or claim a
general transformer runtime.

An opt-in local test installer can download revision-pinned Apache-2.0 ONNX
artifacts for `cross-encoder/ms-marco-MiniLM-L6-v2` and
`sentence-transformers/all-MiniLM-L6-v2` into the ignored `target` directory.
The accompanying smoke tests exercise real reranking through PostgreSQL and
real 384-dimension embedding retrieval with citation preservation. These tests
are promotion evidence, not a shipped inference service or Stable certification.
See [Optional real semantic model tests](../contributor_guide/real_semantic_models.md).

### Source-linked automatic chunking

Experimental plain-text, Markdown, and HTML profiles produce deterministic
chunks with original citation spans, structure paths, parent/neighbor identity,
and source hashes through fenced external jobs. PostgreSQL reparses worker
responses, keeps raw staging private, and publishes a complete generation only
after current source, authority, and registration checks pass.

The production publication schema still uses deterministic fixture embeddings.
The optional real-model smoke proves that externally generated embeddings can
be indexed and retrieved with citation spans intact, but it is not yet an
automatic embedding-job integration.

### Exact-first lifecycle and internal beam foundations

Exact-first registration makes a complete authorized exact path available at
commit, then permits fenced jobs to publish structurally verified HNSW or
IVFFlat indexes without removing exact availability. Exactness and recall pass
the PostgreSQL 17 and 18 ten-million-row gates, but latency and temporary-space
ceilings do not; the feature remains Experimental.

The lazy HNSW cursor and bounded virtual beam engine are Internal capabilities.
They provide deterministic statement-local traversal, provider batching,
dominance and cycle pruning, bounded parent storage, typed incomplete outcomes,
and evidence-path reconstruction without adding a SQL API or changing planner
defaults. Topology expansion is deliberately rejected in 0.3.0.

## Feature maturity

The complete, machine-checked inventory is in
[Supported Features](supported_features.md). Important release boundaries are:

| Stable | Experimental | Internal |
|---|---|---|
| Dense vector SQL and exact search | Page-native HNSW format and filtered ANN | Canonical retrieval contracts |
| Table-backed collections, filters, point and payload maintenance | Half, sparse, bit, and integer vector source surfaces | Shared generation primitives |
| Named dense vectors and hybrid dense/lexical fusion | Native IVFFlat | Lazy HNSW cursor |
| PostgreSQL-native lexical retrieval | Fuzzy retrieval and late interaction | Bounded virtual beam engine |
| Composite query execution | Semantic reranking and automatic chunking | |
| Multi-model retrieval | Exact-first and adaptive-dimension retrieval | |
| Quantized HNSW serving | Segmented serving and supervised generation jobs | |
| pgvector migration and compatibility | pgvector coexistence/adoption workflows | |

“Experimental” means implemented and testable, not planned-only. Its API,
storage format, or operational contract may still change before promotion.

## Compatibility and migration

### Clean-install baseline

0.3.0 deliberately has no `0.1 -> 0.3` or `0.2 -> 0.3` extension update
script, and the 0.3 source package does not contain historical install SQL.
Historical artifacts remain available from their release tags.

For an existing 0.1 or 0.2 installation:

1. Back up the database and inventory every object that depends on pgContext.
2. Export collection/profile configuration that must be recreated.
3. Preserve ordinary source tables. Cast or export columns that use
   pgContext-owned source types before removing the old extension.
4. Do not assume `DROP EXTENSION pgcontext CASCADE` preserves dependent tables,
   views, functions, or indexes; inspect the dependency plan first.
5. Install 0.3.0 cleanly, recreate registrations, and rebuild derived indexes
   and artifacts from the authoritative rows.

### pgvector companion removal

The separate `pgcontext_pgvector` companion extension is retired and is not
packaged. Where pgvector 0.8.x owns source types, install both main extensions
and call `pgcontext.enable_pgvector_binding()`. The optional
`pgcontext.enable_pgvector_name_facade()` may claim unqualified `hnsw` and
`ivfflat` names only when those names are free.

Resumable ownership conversion, validation, rollback, and dump/restore tooling
is available for the certified pgvector shapes. See
[pgvector coexistence](pgvector_coexist.md) and
[Migrating from pgvector](pgvector_migration.md).

### Other breaking boundaries

- The mutable model-version registry is replaced by immutable embedding
  profiles and profile-backed migration state.
- Experimental HNSW, IVFFlat, codec, segment, and generated-artifact formats
  may require `REINDEX` or regeneration.
- 0.3.0 does not promise compatibility with 0.1/0.2 Rust APIs, catalog layouts,
  GUC names, JSON request shapes, or experimental derived artifacts.

PostgreSQL source rows remain the recovery boundary throughout these changes.

## Installation

After release artifacts are published, the preferred container tags are:

```sh
docker pull ghcr.io/evokoa/pgcontext:pg17-v0.3.0
docker pull ghcr.io/evokoa/pgcontext:pg18-v0.3.0
```

Source builds require Rust 1.96.0, cargo-pgrx 0.19.1, and PostgreSQL 17 or 18
server development headers. See [Installation](installation.md) for Docker,
PGXN, Homebrew, source-build, verification, and removal instructions.

## Release-candidate verification

The retained pre-merge verification completed:

- all 825 PostgreSQL integration tests on PostgreSQL 17;
- all 825 PostgreSQL integration tests on PostgreSQL 18;
- strict Clippy with warnings denied for both PostgreSQL feature selections;
- workspace tests, source-hygiene, capability, documentation, repository, and
  SQL-artifact gates;
- pgvector coexistence, binding, ownership conversion, dump/restore, and
  vector/halfvec regression gates; and
- opt-in real cross-encoder and embedding smokes, including PostgreSQL
  finalization, HNSW retrieval, and citation preservation.

Release publication still requires the normal clean-commit artifact build,
signing, multi-architecture image promotion, and registry actions after merge.

## Known limitations

- Semantic reranking and automatic chunking are not Stable real-model services.
  A production embedding-job integration and broader retained platform/quality
  evidence remain open.
- Exact-first passes correctness but misses its current query-latency and
  temporary-space promotion ceilings.
- Automatic chunk publication misses the declared 1,000 chunks/second
  promotion floor in both retained one-million-row source-cardinality lanes.
- Adaptive-dimension retrieval preserves exact answers but its current
  scan-based schedule has no demonstrated latency benefit.
- Native IVFFlat and the general page-native HNSW storage compatibility
  contracts remain Experimental.
- The virtual beam is vector-only and Internal; topology, graph residency,
  mixed vector/topology execution, and a public planner surface are later work.

## Related documentation

- [Supported Features](supported_features.md)
- [SQL API](api_reference.md)
- [Retrieval methods](retrieval_methods.md)
- [Indexes](indexes.md)
- [Multi-model retrieval](multi_model.md)
- [Semantic reranking](semantic_reranking.md)
- [Automatic document chunking](automatic_chunking.md)
- [Operations](operations.md)
- [Support and compatibility policy](support_policy.md)
