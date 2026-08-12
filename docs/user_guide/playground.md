# Playground

The repository playground is a runnable, packaged-build contract, not a
mocked demo. [demo.sql](../../playground/demo.sql) creates ordinary
PostgreSQL rows, registers vector/filter fields, runs exact and
metadata-filtered search, creates a persisted cosine HNSW index, and
verifies an indexed ordered scan.

```sh
scripts/quickstart.sh
```

For the agent-memory walkthrough:

```sh
scripts/quickstart.sh agent-memory
```

Expected behavior for `scripts/quickstart.sh`:

- exact ordering starts with `postgres`, then `rust`, then `vectors`;
- the `category = database` filter returns `postgres` and `vectors`;
- `EXPLAIN` names `pgcontext_playground_docs_hnsw` for the forced indexed scan.

[agent_memory.sql](../../playground/agent_memory.sql) models durable agent
decisions with filtered dense search, hybrid retrieval, and a hydrated context
pack. Expected behavior for `scripts/quickstart.sh agent-memory`:

- filtered billing search returns `d-billing-refund` for tenant `acme` and user `u-alice`;
- hybrid search ranks the billing decision highest for `billing refund`;
- the context pack includes the billing summary and session topic.

Inspect interactively with `scripts/quickstart.sh psql`. Remove all disposable
state with `scripts/quickstart.sh clean`. The local Compose password and port
mapping are development defaults and must not be reused for a shared system.
