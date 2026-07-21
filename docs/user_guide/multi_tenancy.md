# Multi-Tenancy Runbook

pgContext deliberately avoids introducing a separate, opaque tenant store.
Instead it builds on ordinary PostgreSQL tables, grants, row-level security
(RLS), partitioning, standard B-tree indexes, and telemetry labels.
pgContext search paths recheck source rows against these PostgreSQL
controls on every query, so tenant isolation is enforced by PostgreSQL's
own RLS and grants, not by a separate access-control layer.

## Single Collection, Tenant Filter

For shared-table tenants, store a tenant discriminator on the source table and
register it as a filter field:

```sql
CREATE TABLE docs (
  id bigint PRIMARY KEY,
  tenant_id text NOT NULL,
  embedding vector NOT NULL,
  body text NOT NULL
);

SELECT pgcontext.create_collection('docs', 'public.docs');
SELECT pgcontext.register_vector('docs', 'embedding', 'embedding', 3, 'l2');
SELECT pgcontext.register_filter_column('docs', 'tenant_id', 'tenant_id');
```

Every tenant-scoped read should include the tenant filter:

```sql
SELECT point_id, source_key, score
FROM pgcontext.search(
  'docs',
  '[0,0,0]'::vector,
  '{"must":[{"key":"tenant_id","match":"acme"}]}',
  10
);
```

Create a B-tree index for the tenant discriminator, or run
`pgcontext.index_advisor('docs')` and apply the suggested SQL when it reports a
missing filter index.

## Row-Level Security

Use RLS when tenant isolation must not depend on application-provided filters:

```sql
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
ALTER TABLE docs FORCE ROW LEVEL SECURITY;

CREATE POLICY docs_tenant_policy
  ON docs
  USING (tenant_id = current_setting('app.tenant_id', true));
```

Search, count, facet, grouped search, backfill, and hybrid query functions read
source tables as the invoking role, so PostgreSQL RLS policies still decide
which source rows are visible.

## Partitioned Tenants

For large tenants or tenant-local maintenance windows, partition the source
table by tenant and register the partitioned parent:

```sql
CREATE TABLE docs (
  id bigint NOT NULL,
  tenant_id text NOT NULL,
  embedding vector NOT NULL,
  PRIMARY KEY (tenant_id, id)
) PARTITION BY LIST (tenant_id);

CREATE TABLE docs_acme PARTITION OF docs FOR VALUES IN ('acme');
```

Registered tenant filters remain ordinary SQL predicates, so PostgreSQL can
apply partition pruning while pgContext keeps one logical collection contract.

## Per-Tenant Recall

Compute exact tenant results first, then compare candidate IDs with
`pgcontext.recall_check`:

```sql
WITH exact AS (
  SELECT array_agg(point_id ORDER BY score, point_id) AS point_ids
  FROM pgcontext.search(
    'docs',
    '[0,0,0]'::vector,
    '{"must":[{"key":"tenant_id","match":"acme"}]}',
    100
  )
)
SELECT *
FROM pgcontext.recall_check(
  (SELECT point_ids FROM exact),
  ARRAY[101, 205, 309]::bigint[],
  0.95
);
```

Run this per tenant or per tenant cohort when changing indexes, quantization, or
candidate budgets.

## Noisy-Neighbor Diagnostics

Record query stats with bounded cohort labels such as `tenant:acme` or
`tier:enterprise`:

```sql
SELECT pgcontext.record_query_stat(
  'docs',
  'tenant:acme',
  'search_filtered',
  10,
  200,
  12.5
);

SELECT cohort, query_kind, avg_latency_ms, latency_bucket, total_candidates
FROM pgcontext.query_cohort_stats()
WHERE collection_name = 'docs'
ORDER BY avg_latency_ms DESC;
```

Use these rows to identify tenants with larger candidate sets, slower latency
buckets, or degraded recall before changing global collection limits.
