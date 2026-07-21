# Migrating from pgvector

pgContext is designed to make migration from pgvector incremental and safe,
but it is not currently a drop-in replacement. pgContext defines its own
PostgreSQL vector types and index access method, so identical SQL type
names do not make values with different PostgreSQL type OIDs
interchangeable. Coexistence with pgvector in one database is still
evolving and is not yet fully supported.

The non-dense HNSW metric pairs not included in V1 are tracked in the
[post-V1 roadmap](roadmap.md).

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

An existing column owned by the pgvector extension must not be assumed to pass
pgContext registration merely because its displayed type name is `vector`.
Until the
[migration and compatibility roadmap](roadmap.md#pgvector-migration-and-compatibility)
is implemented, preserve the original database and use an explicit copy/export
and validation procedure in a separate test database before changing extension
ownership or dropping pgvector indexes.

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
evaluation only when the involved vector columns are verified as pgContext-
compatible. The roadmap requires a real coexistence or conversion contract;
that contract does not exist yet.

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

Full pgvector parity remains planned for non-L2 sparse and bit-vector Jaccard
ANN index classes. `halfvec` and `sparsevec` have experimental L2
`pgcontext_hnsw` opclasses that store dense vector payloads and keep exact
variant distances as the SQL ordering contract. `bitvec` has an explicit
experimental `pgcontext.bitvec_hnsw_hamming_ops` opclass for Hamming order;
default `pgcontext_hnsw` index attempts on `bitvec` columns still fail with
SQLSTATE `42704` instead of silently choosing an unsupported metric.
Quantized candidate generation, sparse exact array search, and exact reranking
are available from SQL as experimental APIs while serving-path integration
continues to mature.
