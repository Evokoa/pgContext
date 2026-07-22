# PostgreSQL 17 Collection Quickstart

This quickstart uses exact table-backed search, filters, count, and facet
APIs. It intentionally avoids experimental HNSW serving so the expected
outputs are deterministic and reproducible.

## Install and Connect

Choose Docker, manual source, or local Compose from the
[installation guide](installation.md) (PGXN and Homebrew follow in a future
update). For a source checkout, install into the
PostgreSQL 17 instance selected by `pg_config`:

```sh
cargo pgrx install \
  -p context-pg \
  --release \
  --pg-config /path/to/postgresql-17/bin/pg_config \
  --no-default-features \
  --features pg17
```

The command writes extension artifacts into that PostgreSQL installation and
may require filesystem privileges appropriate to it. Connect as a role allowed
to install extensions, then run the SQL below. The packaged HNSW/filter demo is
documented separately in [Playground](playground.md).

## Create a Collection

```sql
CREATE EXTENSION IF NOT EXISTS pgcontext;

CREATE TABLE public.docs (
    id text PRIMARY KEY,
    embedding pgcontext.vector(2) NOT NULL,
    status text NOT NULL,
    body text NOT NULL,
    metadata jsonb NOT NULL
);

INSERT INTO public.docs (id, embedding, status, body, metadata) VALUES
    ('doc-1', '[1,0]'::pgcontext.vector, 'published', 'postgres vector search', '{"topic":"postgres"}'),
    ('doc-2', '[0,1]'::pgcontext.vector, 'published', 'rust extension guide', '{"topic":"rust"}'),
    ('doc-3', '[3,0]'::pgcontext.vector, 'draft', 'internal draft', '{"topic":"postgres"}');

SELECT * FROM pgcontext.create_collection('docs', 'public.docs');
```

Expected collection setup result:

```text
 collection_name | table_name
-----------------+-------------
 docs            | public.docs
```

Register the vector and filterable fields:

```sql
SELECT pgcontext.register_vector('docs', 'embedding', 'embedding', 2, 'l2');
SELECT pgcontext.register_filter_column('docs', 'status', 'status');
SELECT pgcontext.register_jsonb_path('docs', 'topic', 'metadata', ARRAY['topic']);
SELECT pgcontext.upsert_points('docs', ARRAY['doc-1', 'doc-2', 'doc-3']);
```

Run exact nearest-neighbor search:

```sql
SELECT source_key, score
FROM pgcontext.search('docs', '[1,0]'::pgcontext.vector, 2);
```

Expected result order:

```text
 source_key | score
------------+-------
 doc-1      | 0
 doc-2      | 1.4142135
```

Add a filter:

```sql
SELECT source_key, score
FROM pgcontext.search(
    'docs',
    '[1,0]'::pgcontext.vector,
    '{"must":[{"key":"status","match":"published"}]}',
    5
);
```

Expected result order:

```text
 source_key | score
------------+-------
 doc-1      | 0
 doc-2      | 1.4142135
```

Count and facet use the same registered filter fields:

```sql
SELECT pgcontext.count(
    'docs',
    '{"must":[{"key":"topic","match":"postgres"}]}'
);

SELECT *
FROM pgcontext.facet('docs', 'topic', NULL, 10);
```

Expected count:

```text
 count
-------
 2
```

Expected facet rows:

```text
 value    | count
----------+-------
 postgres | 2
 rust     | 1
```

For hybrid dense plus full-text retrieval, use `pgcontext.query`. Keep
`pgcontext.search` for single-vector exact or ANN-style retrieval.

## Remove or Reinstall

Collections are backed by application tables, and those tables can depend on
the extension-owned `vector` type. Remove each collection and its dependent
table before dropping the extension:

```sql
SELECT pgcontext.drop_collection('docs');
DROP TABLE public.docs;
DROP EXTENSION pgcontext;
```

`DROP EXTENSION pgcontext` intentionally does not delete PostgreSQL application
tables with `CASCADE`. Review and remove dependent objects explicitly. To
reinstall the same packaged build, run `CREATE EXTENSION pgcontext;` again; no
repository checkout is required once the control, SQL, and shared-library files
have been installed into PostgreSQL 17.
