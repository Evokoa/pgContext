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
| Exact keyword / phrase match | `pgcontext.query` full-text branch | Lexical (full-text) | Stable |
| Learned term-weight overlap | `pgcontext.search_sparse` | Sparse vector | Experimental |
| Token-level fine-grained match | `pgcontext.rerank_late_interaction` / `pgcontext.search_late_interaction` | Late-interaction | Experimental |
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

Keyword and phrase matching through PostgreSQL full-text search. Today the
full-text branch computes `to_tsvector` on the fly with the `simple`
configuration and matches `plainto_tsquery`; it is the deterministic keyword
branch fused inside `pgcontext.query`.

- As a fused branch: the text-column argument to `pgcontext.query`; as a typed
  branch: `pgcontext.query_full_text`. **Stable** (dense + full-text RRF).
- **Planned:** configurable text-search configuration (language/stemming/
  stopwords), `websearch_to_tsquery`/`phraseto_tsquery` forms, a stored
  `tsvector` column served by a GIN/GiST index, and trigram (`pg_trgm`) fuzzy
  matching as a fusible candidate source. See *Lexical Retrieval Enhancements*
  in the [roadmap](roadmap.md).
- Guide: [Hybrid retrieval](hybrid_retrieval.md).

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
- **Weighted and formula fusion.** Composite queries can weight branches
  (`pgcontext.query_weight`) or combine them with a scoring formula
  (`pgcontext.query_formula`) and apply a floor with
  `pgcontext.query_score_threshold`. **Experimental**.
- **Prefetch then rerank.** `pgcontext.query_prefetch` gathers a candidate set
  that a later stage (`pgcontext.query_rerank`, including late-interaction)
  reorders. **Experimental**.

## Composite Query Execution

For multi-stage or multi-branch retrieval, build a typed query IR and run it
with `pgcontext.execute_query`. The IR builders (`pgcontext.query_nearest`,
`pgcontext.query_full_text`, `pgcontext.query_sparse_nearest`,
`pgcontext.query_prefetch`, `pgcontext.query_rerank`,
`pgcontext.query_score_threshold`, `pgcontext.query_weight`,
`pgcontext.query_formula`, `pgcontext.query_recommend`,
`pgcontext.query_discover`, `pgcontext.query_lookup`) compose into one plan
whose stages you can inspect with `pgcontext.explain`. **Experimental**.

The simpler `pgcontext.query` entry point covers the common dense + full-text
case without assembling an IR; reach for `execute_query` when you need explicit
stages, weighting, or reranking. Guide: [Hybrid retrieval](hybrid_retrieval.md).

## Where Each Method Lives

| Method | Primary entry points | Guide |
| --- | --- | --- |
| Dense vector (exact / ANN) | `pgcontext.search`, `pgcontext.query_nearest` | [vector_search.md](vector_search.md), [indexes.md](indexes.md) |
| Variant types (half/sparse/bit) | typed cores + metric HNSW opclasses | [vector_search.md](vector_search.md) |
| Sparse (learned) | `pgcontext.search_sparse` | [vector_search.md](vector_search.md) |
| Late-interaction | `pgcontext.rerank_late_interaction`, `pgcontext.search_late_interaction` | [vector_search.md](vector_search.md) |
| Lexical (full-text) | `pgcontext.query` text branch, `pgcontext.query_full_text` | [hybrid_retrieval.md](hybrid_retrieval.md) |
| Example-based | `pgcontext.recommend`, `pgcontext.discover` | [vector_search.md](vector_search.md) |
| Filtered | filter JSON on any method | [filters.md](filters.md), [multi_tenancy.md](multi_tenancy.md) |
| Hybrid / fusion | `pgcontext.query`, `pgcontext.execute_query` | [hybrid_retrieval.md](hybrid_retrieval.md) |

For the full, contract-guaranteed signatures see the
[SQL API contract](api_reference.md); for the installed object inventory see the
[SQL object inventory](sql_object_inventory.md); for dependency order and
acceptance requirements of experimental and planned paths see the
[product roadmap](roadmap.md).
