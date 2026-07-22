# Trying pgContext on an Existing pgvector Database

pgContext can be installed alongside pgvector and can index existing
`vector` columns directly — no data movement, no schema changes, no
downtime. This page covers coexist mode: what works, what to expect, and
how to migrate when you are ready.

## Install order matters

Install pgvector first, pgContext second:

```sql
CREATE EXTENSION vector;     -- already present on a pgvector database
CREATE EXTENSION pgcontext;  -- detects pgvector and binds to its types
```

When pgvector is present, pgContext skips creating its own `vector`,
`halfvec`, and `sparsevec` types and instead binds its operators, index
operator classes, and functions to pgvector's types. The two vector
representations are byte-for-byte identical, so there is no conversion
anywhere on the query path.

If pgContext was installed first, its own types occupy the public type
names and `CREATE EXTENSION vector` will fail. Reaching coexist mode
then requires reinstalling in the right order (`DROP EXTENSION
pgcontext`, install pgvector, reinstall pgContext);
`pgcontext.enable_pgvector_binding()` explains the same steps in-band.

## Indexing an existing pgvector column

```sql
CREATE INDEX ON items USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
```

The pgvector index (if any) and the pgContext index coexist on the same
column; PostgreSQL's planner picks one per query. Available operator
classes: `pgcontext.vector_hnsw_ops` (L2),
`pgcontext.vector_hnsw_cosine_ops`, `pgcontext.vector_hnsw_ip_ops`,
`pgcontext.vector_hnsw_l1_ops`.

The registered-collection API works over pgvector columns the same way:
`pgcontext.create_collection` / `register_vector` accept them directly.

## The advisory notice

The first time a backend builds or scans a pgContext index over a
pgvector-typed column, it emits one `NOTICE` recommending
`pgcontext.migration_report()`. Results are always complete — the notice
is guidance, never a gate. Disable it with:

```sql
SET pgcontext.pgvector_compat_warnings = off;
```

## Migration tooling

- **`pgcontext.migration_report()`** — read-only inventory: every
  pgvector-typed scalar or array column, its exact representation,
  dimensions, existing pgvector and pgContext indexes, conversion readiness,
  explicit dependency blockers, and the suggested `CREATE INDEX` command.
  The first conversion profile is deliberately narrow: scalar `vector` and
  `halfvec` columns without defaults, generated expressions, partitions,
  dependent views, or complex indexes.
- **`pgcontext.adopt_pgvector(target => NULL, dry_run => true,
  drop_old => false)`** — migrates pgvector `hnsw`/`ivfflat` indexes to
  `pgcontext_hnsw` equivalents with the matching metric. The default is
  a dry run that only prints the commands; pass `dry_run => false` to
  execute. The planner refuses expression, partial, multicolumn, INCLUDE,
  partitioned, invalid, counterfeit-opclass, and untranslatable IVFFlat
  shapes. It preserves supported HNSW build options and tablespace.
  `drop_old => true` validates the replacement against the exact oracle and
  requires `recall_at_10 >= 0.99` before dropping the pgvector index; failure
  aborts the transaction and leaves the source index intact. Index creation
  inside the function uses plain
  `CREATE INDEX` (it cannot run `CONCURRENTLY`); on busy tables, prefer
  taking the dry-run commands and running them yourself with
  `CREATE INDEX CONCURRENTLY`.

Column types are never changed by these tools: in coexist mode your
data stays in pgvector's types, and dropping the pgvector extension is
not yet possible. The ownership-conversion workflow is a separate, staged
operation; do not use `DROP EXTENSION vector CASCADE` as a conversion tool.

## Comparing indexes side by side

`pgcontext.compare_indexes(table_name, column_name, queries => 20)`
measures every ANN index on a column — pgvector `hnsw`/`ivfflat` and
`pgcontext_hnsw` alike — using sampled stored vectors as queries. For
each reachable operator family it times the planner-chosen index scan
and scores top-10 recall against an exact sequential-scan oracle with
the same operator, returning one row per index:
`(index_name, access_method, operator, p50_ms, p95_ms, recall_at_10)`.
Indexes the planner never chose (for example one shadowed by a cheaper
index of the same operator family) report NULL measurements. The
function is read-only; expect it to run `2 x queries` statements per
measured family.

## Current limitations

- `vector` and `halfvec` columns are fully supported: pgContext's
  storage for both is byte-for-byte pgvector's layout (halfvec elements
  are true IEEE 754 binary16, canonicalized with round-to-nearest-even
  on input exactly as pgvector does). `sparsevec` columns are not yet
  served in coexist mode: pgContext operations over them fail with an
  error rather than producing results (that representation has not yet
  been certified byte-compatible).
- Restoring a dump of a coexist database requires pgvector to be created
  before pgContext, which matches the order `pg_dump` preserves.
- PostgreSQL exposes prepared statements only for the current backend, so no
  extension can inventory every client session's prepared SQL. Drain or
  recycle application sessions at the ownership cutover.
- `DROP EXTENSION vector CASCADE` in coexist mode also drops the
  pgContext objects bound to pgvector's types. Run
  `pgcontext.migration_report()` first and migrate deliberately instead.
