# Migrating from pgvector

pgContext supports an incremental coexistence workflow for existing pgvector
databases on PostgreSQL 17 and 18. The extensions can be installed in either
order because pgvector owns `public.*` types while pgContext owns canonical
`pgcontext.*` types. The `pgcontext` extension owner may explicitly call
`pgcontext.enable_pgvector_binding()` to add certified opclasses and casts for
pgvector 0.8.x in `public`; there is no companion extension. Dense `vector` and
`halfvec` layouts are byte-certified. Existing `public.sparsevec` columns can
be indexed directly, and ownership conversion is available through a validated
restricted-online rewrite because the sparse physical layouts differ. See
[Trying pgContext on an Existing pgvector Database](pgvector_coexist.md) for
the live workflow and inventory tools.

### Moving a 0.1 or 0.2 installation to 0.3

0.3.0 is a clean-install baseline and has no in-place extension update from
0.1 or 0.2. Export pgContext collection, vector, profile, and filter
registrations and inventory every object depending on the old pgContext
extension before any `DROP EXTENSION ... CASCADE`; CASCADE can remove
application views and functions as well as indexes. Preserve the pgvector-owned
source columns, install the current pgContext extension, enable its pgvector
binding, recreate the registrations and dependent objects, and rebuild
`pgcontext_hnsw` or `pgcontext_ivfflat` indexes over the unchanged pgvector
columns. This workflow does not rewrite or retype those source columns.

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
registered through the main-extension binding. Run
`pgcontext.migration_report()` first:
it verifies the type owner and reports defaults, arrays, generated columns,
partitions, dependent views, and complex indexes that must be handled before an
ownership cutover. Index adoption never changes the column type.

## Converting Column Ownership

Enable the certified binding when direct pgContext HNSW service over the source
type is needed, inventory the target, and choose one of two fail-closed modes:

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
certified pgvector HNSW indexes as `pgcontext_hnsw` and IVFFlat indexes as
`pgcontext_ivfflat`. Dimensioned
sources become an unmodified canonical base type plus a validated dimension
CHECK constraint; `NOT NULL`, values, and index options/tablespace are
preserved when the target AM can represent them. Because `pgcontext_hnsw` does
not currently expose pgvector's per-index HNSW reloptions, a source HNSW index
with nondefault options is refused rather than silently changed. IVFFlat
`lists` is preserved on the native IVFFlat rebuild; session-level
`ivfflat.probes` translates to `pgcontext.ivfflat_probes`.
Invalid source indexes and indexes with comments are also refused. The caller
must retain `CREATE` on the table schema and on any preserved nondefault
tablespace needed to rebuild an index.
The operation is one transaction.

Fast mode deliberately rejects `public.sparsevec`: its packed pgvector layout
is not binary-compatible with `pgcontext.sparsevec`, so a metadata-only type
swap would corrupt values. Use restricted-online mode for sparsevec. The binding
decodes and validates pgvector's packed indices and values during backfill and
same-transaction dual writes; conversion fails closed for malformed data or
dimensions above pgContext's 16,000-dimension limit.

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
product, cosine, and L1, plus sparsevec restricted-online conversion and
same-transaction writes on both sides of cutover. It compares exact distances
before and after each conversion, terminates a backend between bounded online
batches and resumes from the persisted cursor, validates rollback to untouched
pgvector objects, disables the main-extension binding and drops pgvector after
finalization, and
restores a custom format dump into a clean database. A pgvector-derived
`pg_regress` profile also
keeps the pgvector-owned columns and query operators unchanged while replacing
only the HNSW access method and opclass. Run the live gates with:

```sh
scripts/check-pgvector-ownership-conversion.sh
scripts/check-pgvector-regression-compat.sh
```

The regression and ownership gates run on PostgreSQL 17 and 18. They cover
native HNSW and IVFFlat rebuilds, but do not claim binary compatibility with
pgvector index pages or silently alias pgvector's HNSW iterative-scan modes.

Online mode adds a physical column, so applications must use explicit INSERT
column lists throughout the migration. PostgreSQL cannot inventory prepared SQL
in other backends; the `sessions_drained` value is an operator attestation, not
automatic global detection. PostgreSQL also does not record column dependencies
for application SQL or ordinary string-bodied SQL/PLpgSQL functions, so
`application_dependencies_reviewed => true` is a required operator attestation
that those call sites were inventoried and can accept the type-ownership change.
The conversion refuses catalog-discoverable unsupported dependencies including
RLS, comments, custom column statistics/storage, and unsupported index options
rather than attempting partial rewrites. Arrays/domains, partitions, and
composite-row dependencies remain unsupported.

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

pgContext now provides its own experimental `pgcontext_ivfflat` access method.
It is not the pgvector `ivfflat` access method and does not make existing index
objects or reloptions binary-compatible. The current adoption and ownership
conversion APIs still inventory a pgvector IVFFlat index conservatively and do
not translate it automatically. Keep the pgvector index live until a separately
built pgContext index passes exact-oracle recall, filter/RLS, latency, DML,
backup, and recovery checks.

A reviewed manual rebuild chooses the matching pgContext source type, metric
opclass, and list count, then creates a new index with
`USING pgcontext_ivfflat`. SQ8/PQ are pgContext-specific choices and must be
certified independently. Automatic pgvector option/name conversion belongs to
the migration phase and is not implied by native IVFFlat availability.

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
