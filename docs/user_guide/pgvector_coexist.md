# Using pgContext Alongside pgvector

pgContext and pgvector can be installed in either order on PostgreSQL 17 or
18. Their types have distinct extension ownership and OIDs:

- pgvector owns `public.vector`, `public.halfvec`, and `public.sparsevec`;
- pgContext owns `pgcontext.vector`, `pgcontext.halfvec`,
  `pgcontext.sparsevec`, and `pgcontext.bitvec`.

The exact `pgcontext` extension-owner role owns all optional interoperability
objects. They intentionally remain outside extension membership so `pg_dump`
includes their DDL and live dependent indexes restore correctly. The former
`pgcontext_pgvector` companion extension is retired and is not packaged.

## Existing pgvector columns

Install both extensions, then let the `pgcontext` extension owner enable the
certified binding explicitly:

```sql
CREATE EXTENSION vector;
CREATE EXTENSION pgcontext;
SELECT pgcontext.enable_pgvector_binding();
```

The binding requires pgvector 0.8.x in `public`. It adds extension-owner-owned
dense binary casts, validated sparse conversion casts, and HNSW opclasses over
pgvector-owned types. Repeating the call is a no-op; a partial or foreign
installation fails closed.

Call the lifecycle functions as the exact extension owner, using `SET ROLE` if
the session role is only a member of that role. `DROP EXTENSION pgcontext`
remains restricted while optional objects exist; disable the binding or facade
first. Facade lifecycle additionally requires that extension owner to be a
superuser because PostgreSQL access-method DDL is superuser-only.

```sql
CREATE INDEX items_embedding_pgc
    ON items USING pgcontext_hnsw
       (embedding pgcontext.vector_hnsw_pgvector_cosine_ops);

SELECT id
FROM items
ORDER BY embedding <=> $1::public.vector
LIMIT 10;
```

The scan exact-rechecks candidates with pgvector's source operator. Sparse
values are decoded and validated at the binding boundary; values outside
pgContext's documented sparse coordinate limit fail explicitly.

`pgcontext.disable_pgvector_binding()` removes the optional objects. PostgreSQL
blocks it with dependent-object SQLSTATE `2BP01` while an index still uses a
binding opclass. Disable the binding before dropping pgvector after completing
an ownership conversion.

## Conflict-safe pgvector names

A database without pgvector can opt into unqualified `hnsw` and `ivfflat`
access-method names:

```sql
CREATE EXTENSION pgcontext;
SELECT pgcontext.enable_pgvector_name_facade();

SET search_path = pgcontext, public;
CREATE INDEX items_embedding_hnsw
    ON items USING hnsw (embedding vector_cosine_ops);
```

The facade uses canonical pgContext types and native pgContext storage. It does
not claim pgvector's type OIDs or index-page format. Installation fails with
duplicate-object SQLSTATE `42710` if either name is already owned, including
when pgvector is installed. Existing facade indexes block
`pgcontext.disable_pgvector_name_facade()` until they are dropped or rebuilt on
`pgcontext_hnsw` or `pgcontext_ivfflat`.

Use the always-available native names in shared databases:

```sql
CREATE INDEX items_embedding_hnsw
    ON items USING pgcontext_hnsw
       (embedding pgcontext.vector_hnsw_cosine_ops);

CREATE INDEX items_embedding_ivf
    ON items USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_cosine_ops)
    WITH (lists = 100);
```

## Published compatibility inventory

Query the executable matrix instead of assuming that a pgvector spelling is an
alias:

```sql
SELECT * FROM pgcontext.pgvector_compatibility_inventory();
```

The matrix classifies types, operators, helper families, HNSW, IVFFlat, and
settings as `compatible`, `translated`, or `unsupported`. In particular,
`ivfflat.probes`, `hnsw.ef_search`, and `ivfflat.iterative_scan` translate to
documented pgContext settings. pgvector's HNSW iterative modes and expression
indexes using `subvector`, `binary_quantize`, or vector arithmetic are rejected
rather than silently accepted.

## Converting ownership

`pgcontext.migration_report()` inventories pgvector columns and both ANN access
methods without enabling the binding. The resumable conversion APIs support
certified dense `vector` and `halfvec` metadata swaps and validated sparse
rewrites. HNSW sources rebuild on `pgcontext_hnsw`; IVFFlat sources rebuild on
`pgcontext_ivfflat` and preserve `lists`. Source pages are never reinterpreted.

Fast conversion is atomic. Restricted-online conversion maintains a shadow
column in the source DML transaction, checkpoints bounded backfill, emits the
top-level concurrent-index command, and requires a session-drain attestation at
cutover. Rollback restores untouched pgvector ownership and indexes;
finalization removes the rollback boundary. See
[Migrating from pgvector](pgvector_migration.md#converting-column-ownership).

Prepared statements must be drained across a type-OID cutover. The preflight
also rejects unsupported views, stored functions, expression indexes, arrays,
domains, partitions, RLS, triggers, publications, comments, and custom storage
or statistics rather than guessing how to rewrite application dependencies.
