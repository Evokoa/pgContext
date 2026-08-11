# Retrieval Methods Overview

This page maps every retrieval method pgContext exposes and gives a decision
guide for choosing among them. It is an index, not a reference: each method
links to the guide that carries its authoritative SQL signatures, semantics,
and maturity label. Where this page and a per-method guide disagree, the
per-method guide wins.

Maturity is labeled inline as **Stable**, **Experimental**, or **Planned**.
Stable behavior is covered by the SQL API contract; experimental paths are
SQL-visible but their semantics or on-disk formats may still change; planned
items are on the [product roadmap](roadmap.md) and are not yet implemented.

## Choosing a Method

Start from the question you are answering, not the index type.

| You want to retrieve by… | Use | Method | Maturity |
| --- | --- | --- | --- |
| Meaning of a query embedding | `pgcontext.search` | Dense vector | Stable (exact) / Experimental (ANN) |
| Exact keyword / phrase match | `pgcontext.query` registered lexical branch | Lexical (full-text) | Stable |
| Learned term-weight overlap | `pgcontext.search_sparse` | Sparse vector | Experimental |
| Token-level fine-grained match | `pgcontext.rerank_late_interaction` / `pgcontext.search_late_interaction` | Late-interaction | Experimental |
| Old and new embedding profiles during cutover | `pgcontext.query_multi_model` | Multi-model weighted RRF | Stable |
| Similarity to example points | `pgcontext.recommend` / `pgcontext.discover` | Example-based | Experimental |
| A blend of the above | `pgcontext.query` / `pgcontext.execute_query` | Hybrid fusion | Stable (dense+full-text) / Experimental (others) |

Two orthogonal choices then apply to whichever method you pick:

- **Exact vs. approximate.** Exact search is the correctness baseline and scans
  the registered source. Approximate (ANN) search uses an HNSW index for
  bounded work and rechecks its candidates against the exact source. See
  [Exact vs. Approximate](#exact-vs-approximate) below.
- **Filtered vs. unfiltered.** Any method can be constrained by a Qdrant-style
  filter over ordinary columns and JSONB metadata. Filtering is applied
  in-graph for indexed search rather than as a naive post-filter. See
  [Filtered retrieval](#filtered-retrieval).

## Semantic (Dense Vector) Retrieval

Nearest-neighbor search over a registered dense vector column, by distance
metric (L2, inner product, cosine, L1). This is the default method for
"find items whose meaning is close to this embedding."

- Entry point: `pgcontext.search` over a collection with a registered dense
  vector; `pgcontext.query_nearest` as a typed branch inside composite queries.
- Exact search is **Stable**; persisted dense HNSW and adaptive filtered ANN are
  **Experimental**, always with an exact source recheck.
- Guide: [Dense vectors and exact search](vector_search.md),
  [Indexes](indexes.md).

### Variant Vector Types

The same distance-based retrieval is available over non-dense element types:

- **Half vectors** (`halfvec`) — half-precision dense storage. **Experimental**.
- **Sparse vectors** (`sparsevec`) — explicit index/value pairs; see
  [Sparse retrieval](#sparse-learned-retrieval). **Experimental**.
- **Bit vectors** (`bitvec`) — Hamming and Jaccard distance. **Experimental**;
  the default `pgcontext_hnsw` opclass on a `bitvec` column fails with SQLSTATE
  `42704` until you name the intended bit metric.

Metric-bound HNSW opclass names for half and sparse (L2, inner product, cosine,
L1) and bit (Hamming, Jaccard) are stable identifiers, but the variant SQL
types and the HNSW on-disk format remain experimental. Guide:
[Vector search — variant cores](vector_search.md).

## Sparse (Learned) Retrieval

Sparse vectors carry an explicit set of dimension-weight pairs rather than a
dense array. In practice they encode **learned sparse** representations
(SPLADE-style): a model expands a document or query into weighted vocabulary
terms — including terms not literally present — so retrieval matches on learned
term importance instead of raw string overlap. Unlike classical full-text
search, weights are learned; unlike dense retrieval, dimensions stay
interpretable as terms.

- Exact top-k over explicit arrays and registered sparse columns:
  `pgcontext.search_sparse`. **Experimental**.
- Named sparse ANN can bind a metric-matched HNSW index for bounded candidates
  with exact source rerank and exact fallback. **Experimental**.
- Register with `pgcontext.register_sparse_vector`. Guide:
  [Vector search — sparse core](vector_search.md).

## Late-Interaction Rerank

Token-level scoring (ColBERT-style MaxSim) that compares per-token embeddings
between query and document instead of a single pooled vector. It is a
high-precision reranking stage over a candidate set produced by a cheaper first
method, with pgContext maintaining the late-interaction tokens internally.

- Rerank a candidate set: `pgcontext.rerank_late_interaction`.
- Retrieve then rerank: `pgcontext.search_late_interaction` /
  `pgcontext.search_late_interaction_ann`; inspect with
  `pgcontext.explain_late_interaction`. **Experimental**.
- Guide: [Vector search — late-interaction rerank](vector_search.md).

## Lexical (Full-Text) Retrieval

Keyword, phrase, and fuzzy matching through PostgreSQL text search. You
register a **lexical source** — ordered weighted text or JSON-path fields, or a
stored `tsvector` column — with its own text-search configuration, ranker,
normalization, and rank weights. PostgreSQL owns parsing, dictionaries,
matching, ranking, and collation; pgContext owns validated metadata, bounded
orchestration, an exact fallback, and authoritative source recheck.

- Registration: `pgcontext.register_lexical_source`,
  `pgcontext.register_lexical_document_source`,
  `pgcontext.register_lexical_tsquery`.
- Indexes: `pgcontext.create_lexical_index` / `pgcontext.attach_lexical_index`
  (GIN or GiST). Without one, the exact path evaluates the complete
  invoker-visible corpus or reports budget exhaustion.
- Typed branch: `pgcontext.query_lexical` with the `plain`, `structured`,
  `phrase`, `web_search`, `prefix`, `distance`, `boolean`, `weight_restricted`,
  and `registered_tsquery` forms. As a fused branch: the lexical-source argument
  to `pgcontext.query`. **Stable.**
- Fuzzy: `pgcontext.register_fuzzy_source` plus `pgcontext.query_fuzzy` over
  optional `pg_trgm`, in `similarity`, `word_similarity`, or
  `strict_word_similarity` mode. **Experimental**; `pg_trgm` is not an install
  requirement.
- Highlighting: `pgcontext.lexical_headline` returns bounded `ts_headline`
  fragments that the caller must sanitize for its output context.
- Guide: [Lexical retrieval](lexical_retrieval.md),
  [Hybrid retrieval](hybrid_retrieval.md).

## Multi-Model Retrieval

During an embedding-model cutover, separate immutable profiles may use
different vector representations, dimensions, metrics, and HNSW indexes while
sharing one stable point occurrence. `pgcontext.query_multi_model` validates a
query value and source-version binding for each profile, executes the same
filter and PostgreSQL authorization boundary per branch, authoritatively
rechecks current rows, and combines ranks through weighted RRF. Native branch
scores are diagnostics only and are never compared across profiles.

The default all-profile policy fails closed. Callers must explicitly allow a
degraded report when a named profile is unavailable. **Stable**. Guide:
[Multi-model retrieval](multi_model.md).

## Example-Based Retrieval

Retrieve by similarity to example points rather than a supplied query vector —
"more like these, less like those" and exploratory discovery.

- `pgcontext.recommend` / `pgcontext.query_recommend` — positive/negative
  example points.
- `pgcontext.discover` / `pgcontext.query_discover` — context-guided
  exploration; `pgcontext.query_lookup` resolves points by key.
- **Experimental**. Guide: [Vector search](vector_search.md),
  [SQL API contract](api_reference.md).

## Filtered Retrieval

Every method above can be constrained by a Qdrant-style filter JSON over
ordinary columns and JSONB metadata. Filtered exact search is the correctness
baseline; filtered ANN applies the predicate as an in-graph candidate mask
(bounded by `pgcontext.hnsw_mask_candidate_limit`) with an adaptive
exact-vs-masked strategy chosen by selectivity, and rechecks survivors against
the exact source. This avoids the post-filter failure mode where a naive
"ANN then filter" returns too few or badly ranked rows under selective
predicates.

- Register filterable surfaces with `pgcontext.register_filter_column` and
  `pgcontext.register_jsonb_path`. Guide: [Filters](filters.md),
  [Multi-tenancy](multi_tenancy.md).

## Exact vs. Approximate

| | Exact | Approximate (ANN) |
| --- | --- | --- |
| Backing | Scans registered source | HNSW index + exact recheck |
| Recall | Exhaustive baseline | Bounded, recall-checked |
| Work | Grows with collection | Bounded by search/candidate budgets |
| Maturity | **Stable** | **Experimental** |

Every approximate path in pgContext rechecks its candidates against the
authoritative source table and can fall back to exact search, so ACL, RLS, and
MVCC visibility hold for every returned row regardless of index state. Use
`pgcontext.recall_check` to measure ANN recall against the exact oracle, and
`pgcontext.optimization_status` to see whether a collection is `Indexed` or
`ExactOnly`.

## Fusion and Ranking

When more than one branch contributes candidates, pgContext merges them
deterministically.

- **Reciprocal rank fusion (RRF).** The stable merge step: each branch returns
  points in rank order and the fusion adds `1 / (k + rank)` per point, default
  `k = 60`. RRF uses rank only, so dense, full-text, and sparse scores never
  need cross-branch normalization. Ties break by ascending point ID. **Stable**
  for dense + full-text; **Experimental** for dense + sparse.
- **Weighted RRF and formulas.** Composite queries can weight branches
  (`pgcontext.query_weight`) and fuse them by rank with the configured
  `query_prefetch` overload, or transform scores within one compatible profile
  with a scoring formula
  (`pgcontext.query_formula`) and apply a floor with
  `pgcontext.query_score_threshold`. **Experimental**.
- **Prefetch then rerank.** `pgcontext.query_prefetch` gathers a candidate set
  that a later stage (`pgcontext.query_rerank`, including late-interaction)
  reorders. **Experimental**.
- **Port-backed stages.** `query_external_rerank` binds a plan to an immutable
  model revision, while `query_topology_expand` bounds graph expansion depth.
  Each adapter receives the remaining comparisons, memory, hydration, and
  elapsed allowance. Execution fails closed when the adapter is unavailable,
  exceeds that envelope, duplicates output IDs, or reports partial work. These
  transport-neutral IR and port contracts are stable, but the bundled SQL
  executor does not yet attach external-rerank or topology providers; executing
  either constructor through `pgcontext.execute_query` therefore fails closed
  until the later provider phases.
  The memory allowance covers extension-owned transient and returned data;
  PostgreSQL executor-internal SPI/sort memory is governed by PostgreSQL, while
  pgContext bounds its admitted row set and applies the elapsed-time guard
  before materializing Rust-owned results.

## Composite Query Execution

For multi-stage or multi-branch retrieval, build a typed query IR and run it
with `pgcontext.execute_query`. The IR builders (`pgcontext.query_nearest`,
`pgcontext.query_lexical`, `pgcontext.query_fuzzy`,
`pgcontext.query_sparse_nearest`,
`pgcontext.query_prefetch`, `pgcontext.query_rerank`,
`pgcontext.query_score_threshold`, `pgcontext.query_weight`,
`pgcontext.query_formula`, `pgcontext.query_recommend`,
`pgcontext.query_discover`, `pgcontext.query_lookup`,
`pgcontext.query_external_rerank`, `pgcontext.query_topology_expand`) compose into one plan
whose stages you can inspect with `pgcontext.explain`. Constructors validate
the entire child tree immediately. Bundled dense, sparse, lexical, fuzzy,
quantized, late-interaction, recommendation, discovery, lookup, fusion, and
score-transform execution is **Stable**. External-rerank and topology
constructors currently provide stable transport-neutral contracts only; their
bundled SQL providers remain unavailable.

The simpler `pgcontext.query` entry point covers the common dense + lexical
case without assembling an IR; reach for `execute_query` when you need explicit
stages, weighting, or reranking. Guide: [Hybrid retrieval](hybrid_retrieval.md).

## Where Each Method Lives

| Method | Primary entry points | Guide |
| --- | --- | --- |
| Dense vector (exact / ANN) | `pgcontext.search`, `pgcontext.query_nearest` | [vector_search.md](vector_search.md), [indexes.md](indexes.md) |
| Variant types (half/sparse/bit) | typed cores + metric HNSW opclasses | [vector_search.md](vector_search.md) |
| Sparse (learned) | `pgcontext.search_sparse` | [vector_search.md](vector_search.md) |
| Late-interaction | `pgcontext.rerank_late_interaction`, `pgcontext.search_late_interaction` | [vector_search.md](vector_search.md) |
| Lexical (full-text) | `pgcontext.query` lexical branch, `pgcontext.query_lexical` | [lexical_retrieval.md](lexical_retrieval.md) |
| Fuzzy (trigram) | `pgcontext.query_fuzzy` | [lexical_retrieval.md](lexical_retrieval.md) |
| Example-based | `pgcontext.recommend`, `pgcontext.discover` | [vector_search.md](vector_search.md) |
| Filtered | filter JSON on any method | [filters.md](filters.md), [multi_tenancy.md](multi_tenancy.md) |
| Hybrid / fusion | `pgcontext.query`, `pgcontext.execute_query` | [hybrid_retrieval.md](hybrid_retrieval.md) |

For the full, contract-guaranteed signatures see the
[SQL API contract](api_reference.md); for the installed object inventory see the
[SQL object inventory](sql_object_inventory.md); for dependency order and
acceptance requirements of experimental and planned paths see the
[product roadmap](roadmap.md).
