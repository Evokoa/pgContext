# Dense Vectors and Exact Search

The pgContext Rust core provides dense vector parsing, formatting, distance
metrics, and exact top-k search. The PostgreSQL extension exposes this
through a SQL-facing dense vector type, distance functions, and an
array-based exact search function.

Metric-bound non-dense HNSW opclasses and named sparse ANN are implemented.
Composite query execution is
tracked in the [post-V1 roadmap](roadmap.md).

## Dense Vector Text

Dense vectors use bracketed numeric text:

```text
[1,-2.5,3.25]
```

Whitespace around values is accepted when parsing. Formatting emits a compact
canonical form without spaces. Empty vectors and non-finite values such as
`NaN` or infinity are rejected. Dense vectors are capped by the shared vector
dimension policy, currently `16,000` dimensions.

## Half Vector Core

The Rust core includes `HalfVector` parsing, formatting, and distance metrics
for the same bracketed text shape. Values are widened to `f32` for metric
evaluation but must stay within the finite half-precision range of
`-65504..=65504`; non-finite values and half-precision overflow are rejected.
Half vectors use the same `16,000` dimension cap as dense vectors. The SQL
`halfvec` wrapper currently exposes text input/output, dimensions, exact
distance functions, distance operators, explicit rounding numeric-array casts, and sum/average
aggregates as an experimental surface.

## Sparse Vector Core

The Rust core includes `SparseVector` parsing and canonical formatting for
`{index:value,...}/dimensions` text. Sparse indexes are 1-based and entries are
stored in ascending index order. Duplicate indexes, zero or out-of-range
indexes, negative or overflowing indexes, zero dimensions, and non-finite
values are rejected. Sparse dimensions and the nonzero entry count are both
capped at `16,000`. The SQL `sparsevec` wrapper currently exposes text
input/output, dimensions, construction from aligned `integer[]` indexes and
`real[]` values, casts to and from dense `real[]` arrays, canonical index/value
accessors, and exact L2, inner-product, negative-inner-product, cosine, and L1
distance functions plus distance operators and sum/average aggregates as an experimental
surface. `pgcontext.search_sparse` provides experimental exact top-k over
explicit sparse candidate arrays and exact-rechecked ANN over registered sparse
source columns for `l2`, `inner_product`, `cosine`, and `l1`. Sparse
`pgcontext_hnsw` indexes use dense graph storage. Sparse cosine rejects zero vectors because the distance is
undefined.

```sql
SELECT point_id, score
FROM pgcontext.search_sparse(
  pgcontext.sparsevec('{1:1,3:2}/5'),
  ARRAY[10, 20]::bigint[],
  ARRAY[
    pgcontext.sparsevec('{1:1,3:1}/5'),
    pgcontext.sparsevec('{2:4}/5')
  ],
  'l2',
  2
);
```

Named search falls back to exhaustive exact scoring until a validated HNSW
index is attached. Build the metric-matched index and bind its schema-qualified
identity to the sparse registration:

```sql
CREATE INDEX docs_lexical_hnsw
ON docs USING pgcontext_hnsw
  (lexical pgcontext.sparsevec_hnsw_ops);

SELECT pgcontext.attach_sparse_hnsw_index(
  'docs', 'lexical', 'public.docs_lexical_hnsw'
);
```

Use `sparsevec_hnsw_ip_ops`, `sparsevec_hnsw_cosine_ops`, or
`sparsevec_hnsw_l1_ops` for the corresponding registered metric. Attachment
rejects a different table/column/metric, partial or expression indexes, and
invalid indexes. Every ANN candidate is joined back to the current source row
and exactly rescored under the caller's ACL/RLS snapshot. A missing, dropped,
or configuration-cleared binding falls back to exact search.

The five-argument overload accepts the same registered-field filter JSON as
dense filtered search and uses a sparse HNSW candidate mask:

```sql
SELECT point_id, source_key, score
FROM pgcontext.search_sparse(
  'docs',
  'lexical',
  pgcontext.sparsevec('{1:1,3:2}/4096'),
  '{"must":[{"key":"tenant","match":"acme"}]}',
  10
);
```

`pgcontext.explain_sparse` reports `strategy`, `active_points`,
`scored_count`, `candidate_count`, and `recheck_count`. `scored_count` includes
both graph node scoring and every live vector scored exactly in the HNSW delta
segment, so inserts made after the last build are not hidden from work
accounting. A rebuilt/compacted HNSW generation should show bounded scored and
candidate work while rechecking every produced candidate.

Filtered sparse ANN uses `pgcontext.hnsw_mask_candidate_limit` as both its
executor and AM mask ceiling. Raising that setting therefore permits a larger
registered-field filter set end to end instead of failing first at a separate
fixed query-executor limit. Setting it to `0` disables masked ANN for filtered
queries and selects the authoritative exact fallback.

Named sparse ANN also masks unfiltered traversal to active collection points
whose source rows are visible under the caller's ACL/RLS snapshot. This keeps
closer hidden, unregistered, or logically deleted rows from consuming the
candidate page. When that caller-visible set exceeds
`pgcontext.hnsw_mask_candidate_limit`, pgContext selects exact search; raise the
setting when bounded ANN is preferred for a larger visible collection.

For a collection registered with `pgcontext.register_sparse_vector`, use the
named sparse vector directly:

```sql
SELECT point_id, source_key, score
FROM pgcontext.search_sparse(
  'docs',
  'lexical',
  pgcontext.sparsevec('{1:1,3:2}/4096'),
  10
);
```

## Bit Vector Core

The Rust core includes `BitVector` parsing and canonical formatting for compact
`0`/`1` text. Hamming distance counts differing bits, while Jaccard distance
compares the set bits and returns zero for two all-zero vectors. Bit length
mismatches are rejected, and bit vectors are capped at `16,000` bits. The SQL
`bitvec` wrapper currently exposes text input/output, dimensions, Hamming
distance, Jaccard distance, distance operators, `boolean[]` casts, casts from
PostgreSQL `bit` and `bit varying`, and casts back to PostgreSQL `bit` and
`bit varying`, and `bitvec(n)` typmods as an experimental surface.
Pgvector-compatible bit-vector ANN indexing is available through the explicit
`pgcontext.bitvec_hnsw_hamming_ops` and
`pgcontext.bitvec_hnsw_jaccard_ops` opclasses. Both traverse with the matching
bit metric; Jaccard is not approximated with densified L2.

## Conversion Policy

Core vector representations expose a typed conversion policy. Dense, half,
sparse, and bit identity conversions are lossless. Dense/sparse conversions and
half-to-dense or half-to-sparse conversions are lossless. Dense or sparse values
converted to half vectors are explicit lossy conversions that round to half
precision; inbound array-to-half casts are never implicit or assignment casts.
Integer and double-precision arrays convert to dense vectors only when every
element is exactly representable as `real`; otherwise conversion raises
`numeric_value_out_of_range`. Those narrowing casts are explicit-only as well.
Numeric vector representations do not cast directly to bit vectors; binary
quantization is a separate index-layer operation.

## Distance Metrics

The complete definitions, return types, ordering rules, validation behavior,
and representation-conversion contract are specified in
[Metric Semantics](metric_semantics.md). The operator mapping is generated in
the [Exact Metric and Operator Matrix](metric_operator_matrix.md).

The dense-vector metrics are:

- L2 distance
- Inner product
- Cosine distance
- L1 distance

Metric evaluation rejects vectors with different dimensions. Cosine distance
also rejects zero-magnitude vectors because the result is undefined.

The SQL facade exposes named functions:

```sql
SELECT pgcontext.l2_distance('[1,2,3]'::pgcontext.vector, '[1,2,5]'::pgcontext.vector);
SELECT pgcontext.inner_product('[1,2,3]'::pgcontext.vector, '[4,5,6]'::pgcontext.vector);
SELECT pgcontext.cosine_distance('[1,0]'::pgcontext.vector, '[0,1]'::pgcontext.vector);
SELECT pgcontext.l1_distance('[1,2,3]'::pgcontext.vector, '[2,4,6]'::pgcontext.vector);
```

pgContext's dense-vector surface is pgvector-compatible. It covers text
input/output, dimensions, and named distance functions. Arrays of `real`,
`integer`, and `double precision` values can be cast to `vector`, and vectors
can be cast back to `real[]`.
Pgvector-style distance operators are available as `pgcontext.<->` for L2,
`pgcontext.<#>` for negative inner product, `pgcontext.<=>` for cosine
distance, and `pgcontext.<+>` for L1. Dense vectors also expose comparison
operators and a default btree operator class for index creation.

Dense vectors also support `pgcontext.sum(vector)` and `pgcontext.avg(vector)`
aggregates. Both return `NULL` for empty input and reject mixed dimensions.

Invalid vector text and non-finite vector values use SQLSTATE `22P02`
(`invalid_text_representation`). Dimension mismatches use SQLSTATE `22023`
(`invalid_parameter_value`).

## Exact Search Baseline

Exact top-k search scores every candidate and returns the lowest metric scores
first. Ties are ordered by point id so repeated runs are deterministic.

The default maximum exact-search result limit is defined by the core policy
module and is currently `10,000`.

Use `pgcontext.search` when the request is a single-vector nearest-neighbor
operation. The collection overloads return one dense-vector ranking, optionally
after applying registered filters or rechecking a caller-provided candidate
point batch. Multi-stage retrieval, branch fusion, full-text participation,
exact dense+sparse fusion, recommendations, and discovery-style workflows belong
to `pgcontext.query` or later query-family APIs, not to `search`.

`pgcontext.search` accepts a query vector, candidate point ids, candidate
vectors, a metric name, and a result limit:

```sql
SELECT point_id, score
FROM pgcontext.search(
  '[0,0]'::pgcontext.vector,
  ARRAY[30, 10, 20]::bigint[],
  ARRAY[
    '[2,0]'::pgcontext.vector,
    '[1,0]'::pgcontext.vector,
    '[0,1]'::pgcontext.vector
  ],
  'l2',
  2
);
```

The supported metric names are `l2`, `inner_product`, `cosine`, and `l1`.
For `inner_product`, returned scores are negative inner products so ascending
order returns larger raw inner products first, matching the `<#>` operator.

The `point_ids` and `vectors` arrays must have the same length. Invalid metric
names, invalid limits, negative point ids, and mismatched candidate arrays use
SQLSTATE `22023` (`invalid_parameter_value`).

Table-backed search APIs build on this exact baseline. Registered collections
can be searched directly with `pgcontext.search(collection, vector, limit)`, and
collections with multiple registered dense vectors can use
`pgcontext.search(collection, vector_name, vector, limit)` to choose the scoring
column explicitly. The named-vector selector is also available on filtered
search, candidate recheck, and grouped search overloads. The filter-first overload
`pgcontext.search(collection, vector, filter_json, limit)` restricts active
point mappings through registered filter columns or JSONB paths. When the
registered vector has an attached `pgcontext_hnsw` index, the same statement
materializes bounded filter-matching heap-TID batches, applies them as reusable
masks during persisted HNSW traversal, and then rechecks visible point mappings,
the predicate, and exact scores against the authoritative source rows. Without
an attached HNSW index, filtered search deliberately uses the exact baseline.
The candidate recheck overload
`pgcontext.search(collection, vector, candidate_point_ids, limit)` exact-scores
only active mappings from a supplied point-id batch. Filtered ANN candidate work
is bounded by `pgcontext.hnsw_candidate_budget` and
`pgcontext.hnsw_iterative_expansion_limit`; it never silently substitutes a
whole-collection exact scan when an attached index is selected.

Use `pgcontext.grouped_search(collection, vector, group_by, group_limit, limit)`
when nearest-neighbor results must be capped per registered payload field. The
`group_by` value must be a registered filter column or JSONB path. Rows with
`NULL` or missing group values are skipped, each group is ranked by distance
then point id, and the final output is ordered by distance then point id.

Use the dense+sparse `pgcontext.query` overload when a collection has one
registered dense vector and a registered sparse vector that should be fused
without PostgreSQL full-text search. Both branches are exact table scans and
are fused with reciprocal-rank fusion:

```sql
SELECT point_id, source_key, score
FROM pgcontext.query(
  'docs',
  '[0.1,0.2,0.3]'::pgcontext.vector,
  'lexical',
  pgcontext.sparsevec('{1:1,9:0.5}/4096'),
  10
);
```

Use `pgcontext.recommend` for exact recommendation search from positive and
negative examples. Point-ID inputs must be active and visible in the collection;
they are excluded from the result set. Raw vector inputs build the same
positive-minus-negative recommendation vector without excluding any collection
points:

```sql
SELECT point_id, source_key, score
FROM pgcontext.recommend('docs', ARRAY[101, 205], ARRAY[309], 10);

SELECT point_id, source_key, score
FROM pgcontext.recommend(
  'docs',
  ARRAY['[0.1,0.2,0.3]'::pgcontext.vector],
  ARRAY[]::pgcontext.vector[],
  10
);
```

Use `pgcontext.discover` or its `pgcontext.explore` alias to find active points
that are farthest from the centroid of active context point IDs. This is an
exact diversity-oriented search, not graph traversal: it uses PostgreSQL
source-row visibility, rejects deleted or invisible context points, excludes
context points from results, and orders by descending distance then point id.

```sql
SELECT point_id, source_key, score
FROM pgcontext.discover('docs', ARRAY[101, 205], 10);
```

Query-constructor helpers return validated JSON plans that clients can persist,
inspect, or translate without relying on internal catalog tables:

```sql
SELECT pgcontext.query_rerank(
  pgcontext.query_prefetch(ARRAY[
    pgcontext.query_weight(pgcontext.query_nearest('[0,0,0]'::pgcontext.vector, 50), 0.7),
    pgcontext.query_score_threshold(
      pgcontext.query_recommend(ARRAY[101], ARRAY[309], 20),
      0.0,
      1.0
    ),
    pgcontext.query_formula(pgcontext.query_discover(ARRAY[205], 20), '$score * 0.5'),
    pgcontext.query_lookup(ARRAY[101, 205])
  ]),
  10
);
```

Formula text is preserved as an opaque client-plan value. It must contain 1 to
512 UTF-8 bytes; executable formula semantics are not implied by this JSON
constructor. Query-plan argument validation is shared with the pure query layer,
while PostgreSQL remains responsible for SQL/JSON conversion and SQLSTATEs.

## Late-Interaction Rerank

`pgcontext.rerank_late_interaction` is an experimental exact oracle for models
that emit multiple vectors per point. It accepts query vectors, point IDs,
candidate vectors, and zero-based offsets that partition the candidate vectors
by point. The score is the sum, over each query vector, of the best inner
product against that point's vectors. Results order by descending score and then
ascending point ID.

```sql
SELECT point_id, score
FROM pgcontext.rerank_late_interaction(
  ARRAY['[1,0]'::pgcontext.vector, '[0,1]'::pgcontext.vector],
  ARRAY[10, 20]::bigint[],
  ARRAY['[1,0]'::pgcontext.vector, '[0,1]'::pgcontext.vector, '[0.8,0.1]'::pgcontext.vector, '[0.1,0.7]'::pgcontext.vector],
  ARRAY[0, 2, 4]::integer[],
  2
);
```

`pgcontext.search_late_interaction` applies the same exact MaxSim scoring to a
registered collection source table whose per-point vectors are stored in a
`vector[]` column. It respects collection ownership, source-table `SELECT`,
deleted-point filtering, dimension validation, and the late-interaction
comparison budget.

```sql
SELECT point_id, source_key, score
FROM pgcontext.search_late_interaction(
  'docs',
  ARRAY['[1,0]'::pgcontext.vector, '[0,1]'::pgcontext.vector],
  'token_vectors',
  10
);
```

Use `pgcontext.explain_late_interaction` before running larger table-backed
queries to inspect the active point count, candidate vector count, projected
MaxSim comparisons, comparison budget, and typed ANN-planner diagnostics for
multi-vector serving readiness:

```sql
SELECT stage, strategy, status, estimated_candidates, candidate_budget
FROM pgcontext.explain_late_interaction(
  'docs',
  ARRAY['[1,0]'::pgcontext.vector, '[0,1]'::pgcontext.vector],
  'token_vectors'
);
```

For experimental candidate serving, store one token vector per row in a
companion table with a `NOT NULL` source-key column and a `pgcontext_hnsw`
index on a dimensioned `vector(n)` token column. The declared typmod is the
O(1) dimension contract used before serving, including when the token table is
empty; untyped `vector` token columns are rejected as not ready. The token HNSW
index must be unfiltered;
partial indexes do not satisfy the serving prerequisite because the candidate
collector issues an unqualified nearest-neighbor scan over the token table.
`pgcontext.search_late_interaction_ann` uses that table only to collect and
deduplicate candidate source keys, then hydrates the authoritative collection
source table and applies exact MaxSim for final scores:

```sql
CREATE TABLE doc_tokens (
  source_key text NOT NULL,
  token_embedding pgcontext.vector(2) NOT NULL
);

CREATE INDEX doc_tokens_embedding_idx
  ON doc_tokens USING pgcontext_hnsw (token_embedding);

SELECT point_id, source_key, score
FROM pgcontext.search_late_interaction_ann(
  'docs',
  ARRAY['[1,0]'::pgcontext.vector, '[0,1]'::pgcontext.vector],
  'token_vectors',
  'public.doc_tokens',
  'source_key',
  'token_embedding',
  32,
  10
);
```

`pgcontext.explain_late_interaction_ann` validates the same source-table
ownership, drift, and ACL checks plus the companion token-table HNSW index. Its
`ann_planner` row reports `ann_candidate_serving` with
`AnnCandidateServingReady` when the candidate path is available. The search path
uses strict collection `max_candidate_budget` against the total projected token
candidate work, uses the same planner-projected budget before collecting token
candidates, and still enforces the actual hydrated exact-rerank budget during
source-table rerank. These budget failures use SQLSTATE `54000`. Token-table
ACL, missing or nullable source-key-column, token-vector-type, and missing or
partial-index failures are pinned to stable SQLSTATEs. Query vectors must share
one dimension and match the token column's declared `vector(n)` dimension;
mismatches use SQLSTATE `22023`. Approximate scores are never returned as final
SQL scores. The
`late_interaction_ann_baseline` benchmark records deterministic token
candidate latency, exact MaxSim rerank latency, token graph bytes, vector
payload bytes, projected comparisons, and recall against exact MaxSim for the
algorithmic serving shape. Tests cover HNSW token-candidate serving,
deduplicated source keys, exact source-table rerank, deleted-point filtering,
and budget rejection; broader certification of this experimental serving path
remains open.
