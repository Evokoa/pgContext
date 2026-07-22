# Using pgContext Alongside pgvector

pgContext and pgvector can be installed in either order. Their SQL types are
separate extension-owned objects:

- pgvector owns `public.vector`, `public.halfvec`, and `public.sparsevec`;
- pgContext owns `pgcontext.vector`, `pgcontext.halfvec`,
  `pgcontext.sparsevec`, and `pgcontext.bitvec`.

The type OIDs are intentionally distinct. The main `pgcontext` extension has no
catalog dependency on the `vector` extension, so installing or dropping
pgvector does not remove or disable canonical pgContext objects.

> **Checkpoint status:** the canonical main-extension boundary is implemented.
> The `pgcontext_pgvector` companion named below is the next compatibility
> checkpoint and is not included in the standalone `pgcontext` artifact yet.

```sql
CREATE EXTENSION vector;
CREATE EXTENSION pgcontext;
```

The reverse order is valid as well.

## Existing pgvector columns

The main extension does not pretend that a pgvector-owned column has a
pgContext-owned type. Direct HNSW service over an existing `public.vector` or
`public.halfvec` column requires the separately installed
`pgcontext_pgvector` companion extension. That privileged bridge owns only the
certified binary casts and pgvector-operator-bound opclasses; it keeps the main
extension's dependency boundary clean.

Until the bridge is installed, `pgcontext.migration_report()` remains available
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

IVFFlat remains an inventory-and-plan input, not a pgContext access method. A
supported conversion rebuilds it as HNSW after validation rather than claiming
an in-place IVFFlat implementation.
