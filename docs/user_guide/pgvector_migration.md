# Migrating from pgvector

pgContext supports an incremental coexistence workflow for existing pgvector
databases. The two main extensions can be installed in either order because
pgvector owns `public.*` types while pgContext owns canonical `pgcontext.*`
types. Keep an existing pgvector column in place and install the certified
`pgcontext_pgvector` companion bridge before building a `pgcontext_hnsw` index
over it. The bridge profile is PostgreSQL 17 with pgContext 0.1.0 and pgvector
0.8.x installed in `public`. Dense `vector` and `halfvec` layouts are
byte-certified; `sparsevec`
ownership conversion remains fail-closed because its physical layouts differ. See
[Trying pgContext on an Existing pgvector Database](pgvector_coexist.md) for
the live workflow and inventory tools.

Explicit pgContext HNSW opclasses cover half and sparse L2, inner product,
cosine, and L1, plus bit Hamming and Jaccard. The names and metric bindings are
stable, while the variant SQL types and HNSW on-disk format remain
experimental; review the index-specific single-page dimension envelope before
rebuilding large pgvector indexes.

## Dense Vectors

Existing pgContext-owned `vector` columns can be registered as named collection
vectors:

```sql
SELECT pgcontext.create_collection('docs', 'public.docs');
SELECT pgcontext.register_vector('docs', 'embedding', 'embedding', 1536, 'cosine');
```

Dense `vector(n)` typmods, text, casts from numeric arrays, distance functions,
distance operators, comparison operators, and dense vector aggregates are
implemented with pgvector-compatible behavior. Assignments to dimensioned
columns reject mismatches with SQLSTATE `22023`. Intentional differences are
documented with tests.

An existing column owned by the pgvector extension can be indexed and
registered through the companion bridge. Run `pgcontext.migration_report()` first:
it verifies the type owner and reports defaults, arrays, generated columns,
partitions, dependent views, and complex indexes that must be handled before an
ownership cutover. Index adoption never changes the column type.

## Filters and Hybrid Retrieval

Register payload columns and JSONB paths that should be filterable:

```sql
SELECT pgcontext.register_filter_column('docs', 'tenant_id', 'tenant_id');
SELECT pgcontext.register_jsonb_path('docs', 'topic', 'metadata', ARRAY['topic']);
```

Filters are Qdrant-style JSON objects that render through typed SQL and SPI
parameters. Full-text hybrid retrieval can combine a registered dense vector
with a text column through reciprocal rank fusion.

## Indexes

Exact search is the correctness baseline. Keep existing PostgreSQL indexes for
high-cardinality filters, joins, and partitioning. Add pgContext index paths only
after recall checks and operational diagnostics show that approximate retrieval
is appropriate for the workload.

pgContext does not implement pgvector IVFFlat indexes for the first production
surface. The production serving path is exact table-backed search first, with
`pgcontext_hnsw` maturing behind explicit recall, visibility, filter, and
restart gates. IVFFlat's training/list maintenance model is not the selected
artifact shape for pgContext's PostgreSQL-native source-table ownership model.
Applications that depend on IVFFlat during migration should keep those pgvector
indexes in place for that workload, and register the same source tables with
pgContext for exact search, filters, hybrid retrieval, diagnostics, and HNSW
evaluation. `pgcontext.adopt_pgvector()` inventories IVFFlat and emits a
rebuild-as-HNSW plan; pgContext does not translate IVFFlat options or claim an
IVFFlat implementation.

## Current Gaps

Experimental SQL wrappers exist for `halfvec`, `sparsevec`, and pgContext's
`bitvec` bit-vector type. They support text input/output, dimension helpers,
exact distance helpers, and distance operators, and they reject malformed values
through the same core validators used by Rust code. `halfvec` also supports
explicit-only numeric-array casts that round to half precision, `halfvec(n)`
typmods, and sum/average aggregates.
`sparsevec` also supports `sparsevec(n)` typmods, a structured constructor from
aligned `integer[]` indexes and `real[]` values plus canonical index/value
accessors, dense `real[]`/`vector` casts, and sum/average aggregates.
Experimental `pgcontext.search_sparse` provides exact top-k over
explicit sparse candidate arrays and registered sparse source columns. `bitvec`
also supports `bitvec(n)` typmods, `boolean[]` casts for structured SQL
construction and extraction, casts from PostgreSQL `bit` and `bit varying`, and
casts back to PostgreSQL `bit` and `bit varying`. Pgvector-compatible built-in
`bit` Hamming and Jaccard functions plus `<~>` and `<%>` operator overloads
delegate through the same checked `bitvec` path. `bitvec` also supports bitwise
OR/AND aggregates through `pgcontext.bit_or(bitvec)` and
`pgcontext.bit_and(bitvec)`. The variant types also install default btree
ordering opclasses for deterministic
comparison and ordinary PostgreSQL btree indexes.

PgContext installs first-class HNSW opclasses for halfvec and sparsevec L2,
inner product, cosine, and L1, plus bitvec Hamming and Jaccard. These classes
store dense graph payloads but bind traversal and SQL ordering to the selected
metric. Bitvec remains explicit—choose
`pgcontext.bitvec_hnsw_hamming_ops` or
`pgcontext.bitvec_hnsw_jaccard_ops`; a default `pgcontext_hnsw` attempt still
fails with SQLSTATE `42704` rather than guessing a bit metric.
Quantized candidate generation, sparse exact array search, and exact reranking
are available from SQL as experimental APIs while serving-path integration
continues to mature.
