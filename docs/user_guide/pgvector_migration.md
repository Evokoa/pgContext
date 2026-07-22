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

## Converting Column Ownership

Install the certified bridge, inventory the target, and choose one of two
fail-closed modes:

```sql
SELECT *
FROM pgcontext.start_pgvector_ownership_conversion(
    'public.items'::regclass,
    'embedding',
    'fast',
    'cosine',
    application_dependencies_reviewed => true
);

SELECT *
FROM pgcontext.run_pgvector_ownership_conversion(
    1,
    sessions_drained => true
);
```

Fast mode takes `ACCESS EXCLUSIVE`, refuses named prepared statements in the
calling backend, changes a certified `public.vector`/`public.halfvec` column to
the corresponding pgContext-owned type without rewriting the heap, and rebuilds
certified pgvector HNSW or IVFFlat indexes as `pgcontext_hnsw`. Dimensioned
sources become an unmodified canonical base type plus a validated dimension
CHECK constraint; `NOT NULL`, values, and index options/tablespace are
preserved when the target AM can represent them. Because `pgcontext_hnsw` does
not currently expose pgvector's per-index HNSW reloptions, a source HNSW index
with nondefault options is refused rather than silently changed. IVFFlat `lists`
is intentionally not translated when that access method is rebuilt as HNSW.
Invalid source indexes and indexes with comments are also refused. The caller
must retain `CREATE` on the table schema and on any preserved nondefault
tablespace needed to rebuild an index.
The operation is one transaction.

Restricted-online mode is for the narrow supported profile when the long lock
is unacceptable:

```sql
SELECT *
FROM pgcontext.start_pgvector_ownership_conversion(
    'public.items'::regclass,
    'embedding',
    'restricted_online',
    'cosine',
    application_uses_column_lists => true,
    application_dependencies_reviewed => true
);

-- Repeat in separate transactions until status = 'index_pending'.
SELECT * FROM pgcontext.run_pgvector_ownership_conversion(1, 1000);

-- Execute the returned next_command as a top-level statement, then certify it.
CREATE INDEX CONCURRENTLY ...;
SELECT * FROM pgcontext.run_pgvector_ownership_conversion(1);

-- Drain/recycle application sessions before the short locked swap.
SELECT * FROM pgcontext.cutover_pgvector_ownership_conversion(
    1,
    sessions_drained => true
);
```

The shadow trigger runs in the same source DML transaction and overwrites direct
shadow assignments from the authoritative column. Backfill calls persist a
heap-TID range cursor and examine a bounded range; an authoritative full scan is
reserved for the end of a pass and resets the cursor if concurrent locks or
drift left mismatches behind. That scan runs without the cutover's
`ACCESS EXCLUSIVE` lock; the certified trigger preserves equality until the
short lock upgrade freezes DML. Candidate index construction is deliberately
emitted to the caller because PostgreSQL forbids `CREATE INDEX CONCURRENTLY`
inside a function transaction. After cutover, the trigger maintains the old
pgvector column for rollback. Use `rollback_pgvector_ownership_conversion(1)`
to restore the original column and indexes, or
`finalize_pgvector_ownership_conversion(1)` to validate once more and
irreversibly remove the rollback column.

The caller that executes `next_command` must own the table and have `CREATE` on
its schema. Final validation, like cutover validation, scans while the reverse
trigger is active under `ACCESS SHARE`; only the final trigger/column DDL uses
the upgraded exclusive lock.

The release gate exercises `vector` and `halfvec` conversions for L2, inner
product, cosine, and L1. It compares exact distances before and after each
conversion, terminates a backend between bounded online batches and resumes
from the persisted cursor, validates rollback to untouched pgvector objects,
drops both the bridge and pgvector after finalization, and restores a custom
format dump into a clean database. A pgvector-derived `pg_regress` profile also
keeps the pgvector-owned columns and query operators unchanged while replacing
only the HNSW access method and opclass. Run the live gates with:

```sh
scripts/check-pgvector-ownership-conversion.sh
scripts/check-pgvector-regression-compat.sh
```

The regression profile is deliberately bounded to the supported PostgreSQL 17
HNSW migration contract; it does not claim IVFFlat implementation or pgvector's
HNSW-specific GUC surface.

Online mode adds a physical column, so applications must use explicit INSERT
column lists throughout the migration. PostgreSQL cannot inventory prepared SQL
in other backends; the `sessions_drained` value is an operator attestation, not
automatic global detection. PostgreSQL also does not record column dependencies
for application SQL or ordinary string-bodied SQL/PLpgSQL functions, so
`application_dependencies_reviewed => true` is a required operator attestation
that those call sites were inventoried and can accept the type-ownership change.
The conversion refuses catalog-discoverable unsupported dependencies including
RLS, comments, custom column statistics/storage, and unsupported index options
rather than attempting partial rewrites. `sparsevec`, arrays/domains, partitions,
and composite-row dependencies remain unsupported.

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
Applications that depend on IVFFlat during bind-mode evaluation should keep
those pgvector indexes in place for that workload, and register the same source
tables with pgContext for exact search, filters, hybrid retrieval, diagnostics,
and HNSW evaluation. `pgcontext.adopt_pgvector()` and fast ownership conversion
inventory IVFFlat and emit or execute a rebuild-as-HNSW plan; pgContext does not
translate IVFFlat options or claim an IVFFlat implementation.

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
