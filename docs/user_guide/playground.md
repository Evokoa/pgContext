# Playground

The repository playground is a runnable, packaged-build contract, not a
mocked demo. [demo.sql](../../playground/demo.sql) creates ordinary
PostgreSQL rows, registers vector/filter fields, runs exact and
metadata-filtered search, creates a persisted cosine HNSW index, and
verifies an indexed ordered scan.

```sh
scripts/quickstart.sh
```

Expected behavior:

- exact ordering starts with `postgres`, then `rust`, then `vectors`;
- the `category = database` filter returns `postgres` and `vectors`;
- `EXPLAIN` names `pgcontext_playground_docs_hnsw` for the forced indexed scan.

Inspect interactively with `scripts/quickstart.sh psql`. Remove all disposable
state with `scripts/quickstart.sh clean`. The local Compose password and port
mapping are development defaults and must not be reused for a shared system.
