# Agent Memory Example

This guide shows how to model **persistent agent memory** with pgContext on
ordinary PostgreSQL tables. The pattern mirrors the hosted
[Memory demo](https://polygres.com/user-demo) on Polygres, but runs entirely
from the open-source checkout with fixture embeddings.

## Problem

Agent applications often stuff prior turns, notes, and decisions into prompts.
That approach does not scale and drifts from the source of truth. pgContext lets
you keep memory in Postgres rows and retrieve ranked context with SQL:

- **Relational facts** — users, sessions, decisions
- **Semantic recall** — dense vectors over decision text
- **Keyword recall** — full-text branch in hybrid queries
- **Scoped memory** — tenant and user filters

## Schema

```
agent_users ──< agent_sessions ──< agent_messages
      │
      └──< agent_decisions (embedding + summary + body)
```

Decisions are the durable memory surface. Sessions and messages provide
provenance when you hydrate ranked hits for an LLM context block.

## Run the example

### Docker quickstart

From the repository root:

```sh
scripts/quickstart.sh agent-memory
```

This starts the packaged PostgreSQL 17 image and runs
[playground/agent_memory.sql](../../playground/agent_memory.sql).

### Pre-built image

```sh
docker run -d --name pgcontext-agent-memory \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=pgcontext \
  -p 5433:5432 \
  ghcr.io/evokoa/pgcontext:pg17-v0.2.0

PGPASSWORD=postgres psql -h 127.0.0.1 -p 5433 -U postgres -d pgcontext \
  -f examples/sql/06_agent_memory.sql
```

### Runnable SQL file

The full script lives at
[examples/sql/06_agent_memory.sql](../../examples/sql/06_agent_memory.sql).

## Walkthrough

1. **Create tables** for users, sessions, messages, and decisions.
2. **Register a collection** on `agent_decisions` with `pgcontext.create_collection`.
3. **Register vector and filter columns** for embedding, `tenant_id`, `user_id`, and `category`.
4. **Upsert points** so pgContext maps decision primary keys to catalog point IDs.
5. **Query memory** with filtered dense search or hybrid retrieval.

### Filtered dense search

Use when you already have a query embedding and metadata scope:

```sql
SELECT source_key, score
FROM pgcontext.search(
    'agent_memory_decisions',
    $query_embedding,
    '{
       "must": [
         {"key": "tenant_id", "match": "acme"},
         {"key": "user_id", "match": "u-alice"},
         {"key": "category", "match": "billing"}
       ]
     }'::jsonb,
    5
);
```

### Hybrid memory search

Use when the agent question includes natural language keywords:

```sql
SELECT source_key, score
FROM pgcontext.query(
    'agent_memory_decisions',
    $query_embedding,
    'billing refund',
    'body',
    5
);
```

The returned `score` is a fused RRF score, not a raw distance. See
[Hybrid retrieval](hybrid_retrieval.md).

### Context pack

Join ranked hits back to relational rows for prompt assembly:

```sql
SELECT d.summary, d.body, d.decided_at, s.topic AS session_topic, hits.score
FROM (
    SELECT source_key, score
    FROM pgcontext.query(
        'agent_memory_decisions',
        $query_embedding,
        'billing refund',
        'body',
        5
    )
) hits
JOIN agent_decisions d ON d.id = hits.source_key
LEFT JOIN agent_sessions s ON s.id = d.session_id
ORDER BY hits.score DESC;
```

Application code should add provenance labels, deduplicate overlapping rows, and
apply a token budget before sending the pack to an LLM.

## Embeddings

pgContext does not call embedding models. The example uses 4-dimensional fixture
vectors so it runs offline. In production:

1. Generate embeddings in application code or a worker when decisions are written.
2. Store vectors in the source table column registered with the collection.
3. Pass a query embedding with the same dimension and metric as the collection.

## Optional GraphRAG appendix

Evokoa's [pgGraph](https://github.com/evokoa/pggraph) extension can traverse
relationships over the same tables. Graph-augmented retrieval inside pgContext
is on the [product roadmap](roadmap.md#graph-augmented-retrieval); until fused
APIs ship, you can compose manually:

1. Run hybrid search to find seed decisions.
2. Register `agent_users`, `agent_sessions`, and foreign keys in pgGraph.
3. Call `graph.traverse` from a seed row to expand session or user neighborhood.
4. Re-rank or filter the expanded set in application code.

This keeps vectors and graph topology in one Postgres database without copying
data to external systems.

## Related pages

- [Client-facing examples](client_examples.md)
- [Hybrid retrieval](hybrid_retrieval.md)
- [Filters](filters.md)
- [Playground](playground.md)
