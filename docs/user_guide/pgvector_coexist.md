# Using pgContext Alongside pgvector

pgContext and pgvector can be installed in either order. Their SQL types are
separate extension-owned objects:

- pgvector owns `public.vector`, `public.halfvec`, and `public.sparsevec`;
- pgContext owns `pgcontext.vector`, `pgcontext.halfvec`,
  `pgcontext.sparsevec`, and `pgcontext.bitvec`.

The type OIDs are intentionally distinct. The main `pgcontext` extension has no
catalog dependency on the `vector` extension, so installing or dropping
pgvector does not remove or disable canonical pgContext objects.

The optional `pgcontext_pgvector` companion is shipped as a separate extension
artifact. Its certified profile is PostgreSQL 17, pgContext 0.2.0, and pgvector
0.8.x installed in `public`; installation fails closed outside that profile.

```sql
CREATE EXTENSION vector;
CREATE EXTENSION pgcontext;
CREATE EXTENSION pgcontext_pgvector; -- only for existing pgvector columns
```

The reverse order is valid as well.

## Existing pgvector columns

The main extension does not pretend that a pgvector-owned column has a
pgContext-owned type. Direct HNSW service over an existing `public.vector` or
`public.halfvec` column requires the separately installed
`pgcontext_pgvector` companion extension. That privileged bridge owns only the
certified binary casts and pgvector-operator-bound opclasses; it keeps the main
extension's dependency boundary clean.

`make install` installs both control/SQL artifacts. If the main extension was
installed directly with `cargo pgrx install`, install the SQL-only companion
and upgrade artifacts with
`scripts/install-pgcontext-upgrades.sh /path/to/pg_config` and
`scripts/install-pgvector-bridge.sh /path/to/pg_config` before
running `CREATE EXTENSION pgcontext_pgvector`. The companion does not activate
automatically and does not create pgvector itself.

Build a pgContext index over the existing column without changing its type:

```sql
CREATE INDEX items_embedding_pgc
    ON items USING pgcontext_hnsw
       (embedding pgcontext.vector_hnsw_pgvector_cosine_ops);

-- Existing pgvector-spelled SQL is unchanged and selects the index above.
SELECT id
FROM items
ORDER BY embedding <=> $1::public.vector
LIMIT 10;
```

The bridge exact-rechecks and reranks its bounded ANN candidate set with the
pgvector heap operator. This preserves pgvector's `double precision` distance
semantics; the conservative initial lower bound favors correctness over scan
work until a tighter certified bound is available.

Without the bridge, `pgcontext.migration_report()` remains available
as a read-only inventory. It discovers pgvector columns and indexes by extension
ownership, reports arrays and dependency blockers, and detects both HNSW and
IVFFlat. `pgcontext.adopt_pgvector(..., dry_run => true)` may be used to inspect
the proposed bridge opclasses and preserved HNSW options. Executing the plan
fails closed unless `pgcontext_pgvector` is installed.

## Canonical pgContext columns

New pgContext-owned columns should name the type explicitly:

```sql
CREATE TABLE items (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector(768) NOT NULL
);

CREATE INDEX items_embedding_hnsw
    ON items USING pgcontext_hnsw
       (embedding pgcontext.vector_hnsw_cosine_ops);
```

These columns remain usable after `DROP EXTENSION vector`. Do not use
`DROP EXTENSION vector CASCADE` as a migration mechanism for pgvector-owned
application columns; use the inventory and ownership-conversion workflow.

## Conversion boundary

Dense `vector` and `halfvec` layouts are byte-certified. `sparsevec` ownership
conversion remains fail-closed because pgContext's sparse representation is not
pgvector's packed layout. PostgreSQL exposes prepared statements only for the
current backend, so drain or recycle application sessions at a type-ownership
cutover.

Use `pgcontext.start_pgvector_ownership_conversion` plus
`run_pgvector_ownership_conversion` for an atomic metadata-only conversion, or
select `restricted_online` for bounded shadow backfill and a caller-executed
`CREATE INDEX CONCURRENTLY`. The online profile requires explicit INSERT column
lists; both modes require explicit review of application and string-bodied
stored-function dependencies that PostgreSQL cannot inventory. The online
profile supports at most one source ANN index with the requested metric and
requires the caller to have schema/index-build privileges. It refuses
catalog-discoverable unsupported dependencies. After its drained cutover,
`rollback_pgvector_ownership_conversion` restores the synchronized pgvector
column and original index; `finalize_pgvector_ownership_conversion` instead
removes that rollback boundary. See
[Migrating from pgvector](pgvector_migration.md#converting-column-ownership) for
the complete sequence and operational restrictions.

Dropping either prerequisite is blocked while the bridge is installed. Bridge
indexes in turn block `DROP EXTENSION pgcontext_pgvector` under `RESTRICT`.
Remove or convert those indexes first; dropping the bridge then removes its
casts, support functions, and opclasses without removing either parent
extension.

IVFFlat remains an inventory-and-plan input, not a pgContext access method. A
supported conversion rebuilds it as HNSW after validation rather than claiming
an in-place IVFFlat implementation.
