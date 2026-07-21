# Troubleshooting and Maintenance Runbook

Use SQLSTATEs and typed diagnostic statuses for automation. Error text can
become more specific over time, but documented SQLSTATE categories are part of
the stable SQL API.

## Common Failures

| Symptom | SQLSTATE or status | Inspect | Corrective action |
|---|---:|---|---|
| Collection, vector, model, or index name is missing | `42704` | `pgcontext.collection_info`, `pgcontext.optimization_status`, `pgcontext.index_status` | Check the qualified name, recreate the missing registration, or restore the catalog from backup. |
| Registered source table is gone or renamed | `42P01` | PostgreSQL catalogs and `pgcontext.collection_info` | Restore the source table, rename it back, or drop and recreate the collection registration. |
| Registered payload, filter, text, or vector column is gone | `42703` | `pgcontext.collection_info` and table definitions | Restore the column, re-register the collection against the new shape, or remove the stale registration. |
| Source column type no longer matches the registration | `42804` | Table definitions and vector dimensions | Restore the expected type or recreate the registration with the intended column and dimensions. |
| ACL or RLS blocks search, diagnostics, or registration | `42501` | Table grants, schema grants, function grants, ownership, and RLS policies | Grant the caller access to the source table and pgContext SQL API, or run the query as an allowed role. Do not bypass application RLS for user-facing search. |
| Invalid vector, filter, dimension, limit, recall threshold, or tuning value | `22P02` or `22023` | Input payloads, registered dimensions, and current GUC values | Fix the request before retrying. These errors are caller-data failures, not transient operational failures. |
| Recall-check input exceeds the policy budget | `54000` | `pgcontext.explain` recall budget and `pgcontext.recall_check` input array sizes | Reduce the exact or candidate point-id arrays, split validation into batches, or validate on a smaller fixture. |
| Index exists but cannot serve queries | `IndexNotReady` / `55000`, or `pgcontext.index_status.status <> 'Ready'` | `pgcontext.index_diagnostics(index_name)` and `pgcontext.index_status(index_name)` | Wait for build completion, drop an invalid index, or rebuild with `REINDEX` or `CREATE INDEX` after the source-table issue is fixed. |
| Artifact or index validation detects corruption | `IndexCorrupt` / `XX001` | `pgcontext.index_diagnostics`, storage loader logs, and PostgreSQL relation checks | Stop relying on the affected artifact or index. Rebuild it from source tables, or restore from a known-good PostgreSQL backup if source data is damaged. |
| Optimization unexpectedly falls back to exact search | `pgcontext.optimization_status.status` | `pgcontext.optimization_status(collection)` | Check for missing HNSW indexes, stale registrations, invalid indexes, and unsupported vector/index combinations. Exact fallback preserves correctness but can change latency. |
| HNSW build fails or produces poor recall | Build error or failing `pgcontext.recall_check` | `pgcontext.index_status`, `pgcontext.vacuum_advice`, `pgcontext.recall_check`, and HNSW GUCs | Validate dimensions and source rows, increase search/build budgets, rebuild the index, or keep exact search for collections that cannot meet the recall target. |

## Maintenance Procedures

Use normal PostgreSQL maintenance for source tables. pgContext source data lives
in ordinary application tables, so `VACUUM`, `ANALYZE`, partition maintenance,
backup, restore, and privileges should follow the same runbooks as the rest of
the database.

Run `VACUUM (ANALYZE)` on tables that receive heavy updates or deletes before
treating vector recall or latency changes as index defects. Then inspect:

```sql
SELECT *
FROM pgcontext.vacuum_advice('public.docs_embedding_idx');
```

Use `REINDEX INDEX` when a PostgreSQL index is invalid, corrupted, or needs to
be rebuilt after a material source-table correction. For planned HNSW tuning
changes, create a replacement index with the desired GUCs in a controlled
maintenance window and validate recall before controlled rollout. Dense HNSW
is implemented but experimental; use exact search whenever the measured
workload does not meet its recall or lifecycle requirements.

Validate approximate search against exact search before and after major data
loads, index rebuilds, model migrations, and PostgreSQL upgrades:

```sql
SELECT *
FROM pgcontext.recall_check(
  ARRAY[10,20,30]::bigint[],
  ARRAY[20,30,40]::bigint[],
  0.95
);
```

Treat `Failing` recall as a release gate failure for that collection. Increase
candidate budgets, rebuild the index, or use exact search until the measured
fixture passes.

Track embedding-model changes with `pgcontext.register_model_version`,
`pgcontext.create_embedding_migration`, `pgcontext.update_embedding_migration`,
and `pgcontext.embedding_migrations`. Keep old and new model versions explicit
until backfill progress reaches the planned total and recall checks pass for
the migrated collection.

Back up and restore with PostgreSQL-native tooling. Rebuildable pgContext
artifacts are cache data, not authoritative data. If an artifact cannot be
loaded after restore, rebuild it from source tables and catalog metadata rather
than copying an unvalidated file into service.

After extension upgrade or restore, run smoke checks for collection
registration, exact search, filters, `pgcontext.optimization_status`,
`pgcontext.index_status`, telemetry, and any HNSW indexes before declaring the
database ready for production traffic.
