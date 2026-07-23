# Hybrid Retrieval

pgContext uses reciprocal rank fusion (RRF) as the deterministic merge step
for hybrid retrieval, combining keyword and vector search. Each branch
returns points in rank order, and the fusion step adds `1 / (k + rank)` for
each point in each branch. The default constant is `k = 60`.

RRF uses rank only. Dense vector scores, full-text scores, exact sparse
scores, and future ANN sparse scores stay local to their branch ordering,
which avoids score-normalization requirements between retrieval methods.

Ties are stable: fused results sort by descending RRF score and then by
ascending point ID. Repeated point IDs within one branch count only at their
first rank.

Use `pgcontext.query` when retrieval needs multiple stages or multiple branches.
The current stable shape combines a registered dense-vector branch with one
PostgreSQL full-text branch and returns a fused score. An experimental overload
also fuses dense vector retrieval with a registered sparse vector branch.
Single-vector exact, filtered, candidate-recheck, or ANN-style nearest-neighbor
retrieval should use `pgcontext.search` instead.

## Query a Table-Backed Collection

Use `pgcontext.query` to combine the registered dense vector branch with a
full-text branch over one source-table column:

```sql
SELECT point_id, source_key, score
FROM pgcontext.query(
  'docs',
  '[0,0]'::pgcontext.vector,
  'database internals',
  'body',
  10
);
```

The source table must have a registered dense vector column. The text column is
validated at execution time and is read with PostgreSQL `simple` text search.
Logically deleted point mappings are excluded from both branches.

The returned `score` is the fused RRF score, not the dense distance or full-text
rank. Results are ordered by fused score and then by point ID for stable ties.

## Explain a Hybrid Query

Use `pgcontext.explain` to inspect the SQL-visible stages that `pgcontext.query`
will use for a collection and text column:

```sql
SELECT stage,
       detail,
       branch,
       strategy,
       status,
       estimated_candidates,
       candidate_budget
FROM pgcontext.explain('docs', 'body');
```

The output includes the source table, dense vector branch, full-text branch, and
fusion stage. `status` is the typed enum `pgcontext."QueryExplainStatus"` with `Ready`,
`Fallback`, and `Policy`. `estimated_candidates` reports active collection point
counts where they are meaningful before query literals are known, and
`candidate_budget` reports policy budgets such as the search limit or recall
check limit. The function validates the same collection ownership, source-table
privilege, registered vector, and text-column drift checks as `pgcontext.query`.

## Optimization Status

Use `pgcontext.optimization_status(collection)` to inspect the catalog artifacts
that affect query strategy for a collection:

```sql
SELECT collection_name,
       table_schema,
       table_name,
       has_source_table,
       source_table_exists,
       registered_vectors,
       active_points,
       filter_fields,
       hnsw_indexes,
       status
FROM pgcontext.optimization_status('docs');
```

`status` is the typed enum `pgcontext."OptimizationStatus"` with `Indexed`, `ExactOnly`,
`MissingArtifacts`, or `StaleCatalog`. `Indexed` means at least one registered
vector has a matching `pgcontext_hnsw` index. `ExactOnly` means the collection
has the artifacts needed for exact table-backed retrieval but no matching HNSW
index. Missing collections use SQLSTATE `42704` (`undefined_object`).

## Telemetry

Use `pgcontext.telemetry()` to list collection-level rollups for monitoring:

```sql
SELECT collection_name,
       table_schema,
       table_name,
       has_source_table,
       source_table_exists,
       registered_vectors,
       active_points,
       deleted_points,
       filter_fields,
       hnsw_indexes,
       status
FROM pgcontext.telemetry();
```

`status` is the typed enum `pgcontext."TelemetryStatus"` with `Active`, `Empty`,
`MissingArtifacts`, or `StaleCatalog`. Counts are catalog-derived and intended
for trend monitoring; query-level cohort counters are a separate surface.

## Automatic Query Execution Stats

Executor-backed `search` and `execute_query` calls automatically record the
bounded strategy and work that actually ran. Read the membership-filtered
rollup with:

```sql
SELECT collection_name,
       query_kind,
       strategy,
       query_count,
       total_visits,
       total_filter_candidates,
       total_candidates,
       total_rechecks,
       total_stages,
       total_expansions,
       completion,
       latency_bucket,
       lifecycle_state,
       avg_latency_ms
FROM pgcontext.query_execution_stats();
```

`strategy`, `completion`, latency, lifecycle, and work counters are bounded
labels or numeric values. Apart from the collection association, pgContext
never writes vectors, payloads, filter values, query text, source keys, role
names, or caller-provided tenant dimensions into automatic rows.

The query backend performs no telemetry SQL write. It makes a nonblocking
attempt to enqueue one fixed-size event in a 1,024-entry named shared-memory
queue, and a database-scoped background worker persists the event in a separate
transaction. Consequently, terminal errors and cancellations can still be
observed even though the query transaction aborts. Delivery is bounded and
best effort and may duplicate: shared-memory allocation/attachment failure, lock contention, a
full queue, worker-launch failure, or exhaustion of the 64 database slots can
drop observations, while a worker failure after commit but before
acknowledgement can duplicate one. Events whose collection
transaction is not visible yet are retried for 60 seconds and then counted as
orphaned; the queue itself does not survive a postmaster restart.

Members of PostgreSQL's `pg_monitor` role can inspect transport health without
seeing query contents:

```sql
SELECT * FROM pgcontext.query_telemetry_queue_stats();
```

The worker exits after five idle seconds. A superuser can set
`pgcontext.query_telemetry_enabled = off` for incident response or a controlled
latency baseline; disabled queries are intentionally unobserved.

## Manual Query Cohort Stats

Use `pgcontext.record_query_stat` to store application-defined SQL-visible query telemetry
without storing vector contents, filter values, or literal query text:

```sql
SELECT pgcontext.record_query_stat(
  'docs',
  'tenant:acme',
  'search_filtered',
  12,
  120,
  8.4
);
```

Detailed telemetry can use the overload that records explicit serving counters:

```sql
SELECT pgcontext.record_query_stat(
  'docs',
  'tenant:acme',
  'candidate_recheck',
  12,
  240,
  120,
  120,
  0.95,
  0.98,
  42.0,
  'Indexed'::pgcontext."QueryLifecycleState"
);
```

`query_kind` must be `search`, `search_filtered`, `candidate_recheck`, or
`hybrid`. `cohort` is an operator-defined label, not free-form text: it may
contain only ASCII letters, digits, `_`, `-`, `.`, `:`, or `/`, and should not
contain PII.

Use `pgcontext.query_cohort_stats()` to aggregate recorded samples:

```sql
SELECT collection_name,
       cohort,
       query_kind,
       query_count,
       total_results,
       total_candidates,
       total_rows_rechecked,
       total_rows_pruned,
       avg_recall_threshold,
       avg_recall_achieved,
       latency_bucket,
       lifecycle_state,
       avg_latency_ms,
       status
FROM pgcontext.query_cohort_stats();
```

`latency_bucket` is the typed enum `pgcontext."QueryLatencyBucket"`. `lifecycle_state` is
the typed enum `pgcontext."QueryLifecycleState"`, with values for exact, indexed, fallback,
not-ready, corrupt, missing-artifact, and unspecified samples. `status` is the
typed enum `pgcontext."QueryCohortStatus"` with `Observed`. Missing collections use SQLSTATE
`42704` (`undefined_object`), and invalid counters, recall values, latency,
cohorts, or query kinds use SQLSTATE `22023` (`invalid_parameter_value`).
