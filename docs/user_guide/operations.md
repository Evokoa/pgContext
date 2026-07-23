# Operations and Support

pgContext is designed to keep source data in ordinary PostgreSQL tables.
Extension catalogs store configuration and operational state, while source
tables, backups, ownership, row level security, and normal indexes remain under
standard PostgreSQL administration.

## Recall and Diagnostics

Use SQL-visible diagnostics before treating approximate search as a production
default:

- `pgcontext.index_status` reports collection and index readiness.
- `pgcontext.index_diagnostics` reports typed `Ready`, `IndexNotReady`,
  `IndexCorrupt`, or `UnsupportedAccessMethod` status with SQLSTATE and repair
  advice where a pgContext serving index cannot be used.
- `pgcontext.recall_check` compares approximate candidates with exact search on
  fixed inputs.
- `pgcontext.optimization_status` shows whether a collection currently uses
  exact-only, indexed, or fallback behavior.
- `pgcontext.index_advisor` suggests ordinary PostgreSQL indexes and statistics
  actions for registered filter fields.
- `pgcontext.vacuum_advice` reports index-level tuple and page counters.
- `pgcontext.telemetry`, `pgcontext.query_cohort_stats`, and
  `pgcontext.query_execution_stats` expose local counters
  for monitoring trends, including candidates considered, rows rechecked, rows
  pruned, recall targets and achieved recall, latency buckets, and serving
lifecycle state.

`query_execution_stats` is populated automatically by executor-backed
`search` and `execute_query` calls. Its rows contain only bounded strategy,
completion, lifecycle, latency-bucket, and numeric work fields. Apart from its
collection association, the membership-filtered view does not expose vectors,
payloads, filters, query text, source keys, roles, or caller-provided tenant
dimensions.

Automatic events are offered nonblockingly to a bounded named shared-memory
queue and committed by a database-scoped background worker. Members of
`pg_monitor` should alert on nonzero `dropped_contention`, `dropped_full`,
`dropped_orphaned`, `database_slot_exhausted`, or `worker_launch_failures` from:

```sql
SELECT * FROM pgcontext.query_telemetry_queue_stats();
```

The queue provides bounded best-effort delivery that may duplicate, not an audit log:
shared-memory allocation or attachment failure leaves the query unobserved, a
postmaster restart loses pending events, and commit/acknowledgement failure can
produce a duplicate. The worker retries a not-yet-visible collection for 60
seconds and exits after five idle seconds. Retain or aggregate `_query_stats`
according to the deployment's write volume. Superusers may disable new events
with `SET pgcontext.query_telemetry_enabled = off` for incident response or
controlled measurement.

Diagnostics return typed statuses and counters. They are not intended to expose
vector contents, filters, payload values, or literal query text.

## Backup and Restore

Back up pgContext databases with normal PostgreSQL tools such as `pg_dump`,
physical base backups, and WAL archiving. User-owned source tables are not
extension-owned and should be included in the same backup plan as the rest of
the application schema.

Rebuildable artifacts are an optimization layer, not the source of truth for
user data. The first production surface does not expose stable SQL
import/export/rebuild functions for those artifacts. If an artifact is missing
or rejected by validation, rebuild it from catalog metadata and source tables;
use PostgreSQL backup/restore for authoritative recovery.

Exercise logical backup and restore with the same roles and extension version
used by the target environment:

```sh
pg_dumpall --globals-only --file=postgres-globals.sql
pg_dump --format=custom --file=pgcontext.dump appdb
# Review postgres-globals.sql before applying role/tablespace changes.
psql --dbname=postgres --file=postgres-globals.sql
createdb appdb_restore
pg_restore --dbname=appdb_restore --exit-on-error pgcontext.dump
psql --dbname=appdb_restore \
  -c "SELECT extversion FROM pg_extension WHERE extname = 'pgcontext';" \
  -c "SELECT * FROM pgcontext.collection_info('collection_name');"
```

Include application schemas, pgContext-owned extension catalogs, roles/grants,
and any global objects required by the application backup policy. Globals can
contain role definitions and password hashes: restrict the file, review it, and
apply it only to the intended restore host. Alternatively, provision required
roles/tablespaces through normal infrastructure before `pg_restore`. Do not copy
experimental segment files as a substitute for PostgreSQL backup. After restore,
rebuild invalid or missing acceleration artifacts and validate exact results,
filters, HNSW plans, and recall before returning an indexed path to service.

## Maintenance Procedures

Run routine table maintenance against the application tables that own vectors
and payload data. `VACUUM`, `ANALYZE`, partition maintenance, ordinary
PostgreSQL indexes, grants, RLS policies, and backup schedules remain normal
database administration responsibilities.

Use `pgcontext.vacuum_advice(index_name)` after heavy update/delete workloads to
decide whether table cleanup or fresh statistics should happen before
investigating vector recall or latency. Use `REINDEX` or a replacement
`CREATE INDEX` when an index is invalid, corrupted, or intentionally rebuilt for
new HNSW tuning. Validate the rebuilt path with `pgcontext.recall_check` before
controlled rollout, and keep production indexed serving gated by the final
release notes.

Use `pgcontext.hnsw_serving_stats()` when investigating first-query latency
cliffs or repeated slow queries after writes: `pack_builds` counts how many
times this backend rebuilt its packed graph generation (each rebuild costs
roughly `last_pack_millis`), and `pack_reuses` counts queries served from
the existing pack. `mapped_attaches`, `mapped_publishes`, and
`mapped_publish_skips` describe immutable file-backed generations
(`pgcontext.hnsw_mmap_serving`). Each file is bound to the database, logical
index, physical relfilenode, directory epoch, and metapage LSN; stale or corrupt
files fall through without changing query results. `shared_attaches`, `shared_publishes`, and
`shared_publish_skips` describe activity against the cross-backend shared
registry (`pgcontext.hnsw_shared_serving`); a backend that attaches instead
of building skips the pack cost entirely. A packed generation is whole: when
writes stale it, the next query rebuilds it rather than patching it, so a
rising `pack_builds` under a write-heavy workload means writes are staling
packs faster than queries can amortize a rebuild. Inserts absorbed by the
segmented delta region do not stale a pack; inline graph splices do, so this
usually points at a delta region that has filled.

`delta_segment_records` and `delta_segment_scans` describe a different,
persisted mechanism: the segmented write path (`pgcontext.hnsw_delta_segment_limit`)
that absorbs inserts into a bounded on-disk delta region instead of splicing
every write into the HNSW graph. `delta_segment_records` counts rows appended
to that region (including VACUUM tombstones for rows never spliced into the
base graph); `delta_segment_scans` counts queries that merged an exact scan
over that region with base-graph results.

Use `pgcontext.index_diagnostics(index_name)` before enabling an indexed serving
path. `IndexNotReady` includes `55000` and points to build completion or
statistics refresh actions. `IndexCorrupt` includes `XX001` and recommends
`REINDEX` or rebuilding from the source table. Unsupported access methods return
a typed row without a pgContext error category.

Use `pgcontext.index_advisor(collection)` when filter latency or filtered ANN
planning changes. It reports typed recommendations such as `CreateBtreeIndex`,
`CreateGinIndex`, `AnalyzeTable`, and `TuneHnswSettings`; suggested SQL is
advisory and should be reviewed with the application schema owner.

For embedding-model changes, register the old and new model versions, track the
backfill with the embedding migration APIs, and keep exact search or the prior
serving path available until migrated fixtures pass recall validation.

Before building or rebuilding a `pgcontext_hnsw` index, size
`maintenance_work_mem` for the corpus: the build enforces it as a hard budget
and stops with SQLSTATE `22023` plus a suggested-setting `HINT` when the
estimate exceeds it. PostgreSQL's default 64MB covers only about 100,000
384-dimensional vectors; set a session-level budget for large builds and
`RESET` it afterwards. See the sizing rule in
[Indexes — Build Memory Budget](indexes.md#build-memory-budget).

## Backend-Local Build Metadata

Experimental build-job metadata records progress for explicit operator-driven
pgContext artifact or projection work without introducing a shared Rust worker.
The `index` and `sparse_index` target labels mean derived pgContext projections,
never native PostgreSQL `CREATE INDEX` work. A backend creates a job with
`pgcontext.start_build_job`, records bounded progress with
`pgcontext.update_build_job`, and reaches a terminal `Completed`, `Failed`, or
`Cancelled` status before another backend can retry it. Collection owners can
list rows with `pgcontext.build_jobs`, request cooperative cancellation with
`pgcontext.request_build_cancel`, and retry failed, cancelled, or abandoned jobs
with `pgcontext.retry_build_job`.

`pgcontext.run_build_job(build_job_id, units_per_step)` is a narrow
backend-local runner for experimental `segment` and `mmap` jobs. It executes
synchronously in the calling PostgreSQL backend, advances at most one bounded
step per call, observes cancellation requests that are already visible before
that step starts, and records a terminal row through the same catalog state
machine. Retry rebuilds from the authoritative source tables; no mutable Rust
heap is shared across backends. A concurrent cancellation request may wait for
the runner transaction to release the build-job row lock.

The catalog distinguishes `Running` and `CancelRequested` rows whose backend
identity still appears in `pg_stat_activity` from `Abandoned` rows whose
backend is gone. This is metadata for safe ownership, progress, cancellation,
retry, and replacement-build decisions. When a new build starts for the same
target, stale `Running` or `CancelRequested` rows whose backend disappeared are
persisted as `Abandoned` and backend ownership is cleared before the replacement
row is inserted. This does not make PostgreSQL access-method `CREATE INDEX`
resumable, does not publish or mark a partial artifact ready for serving, and
does not replace `pgcontext.index_status` or `pgcontext.index_diagnostics`
before query rollout.

For materialized experimental segment artifacts, use
`pgcontext.artifact_segment_diagnostics(collection)` to classify missing,
corrupt, checksum-drifted, or catalog-drifted files. The diagnostic advice is
deterministic: `ready` needs no action; metadata-only, retired, or pathless
manifests are not cleanup candidates; `path_rejected` requires fixing or
removing the invalid catalog path before cleanup; and missing, corrupt,
checksum-drifted, or metadata-drifted artifacts should be retired or rebuilt
after investigation. Its `cleanup_eligible` flag is true only for root-confined
materialized artifact paths that `pgcontext.retire_artifact_segment(artifact_id)`
may clean up. Collection owners can call that retire function to mark the
manifest retired and attempt to remove its generated, root-confined artifact
file before rebuilding or republishing from source tables. The retire operation
does not repair arbitrary catalog paths, rebuild artifacts, or make mmap/vector
serving stable.

Use `pgcontext.cleanup_artifact_segments(collection, dry_run)` for collection
cleanup. It reports or retires manifest-known missing, corrupt, checksum-drifted,
or metadata-drifted files, and it also reports or removes regular generated
`.pgctxseg` files that are no longer referenced by a visible manifest. That
orphan-file cleanup covers the crash window after atomic file materialization
and before catalog publication. It does not follow symlinks, recurse through
directories, or remove non-segment files.

`pgcontext.artifact_segment_mmap_payload(collection, artifact_name,
max_mapped_bytes)` is the experimental SQL compatibility primitive for
file-materialized `mmap` artifacts. It validates the file through the same
serving-readiness checks and returns copied payload bytes only when the artifact is
root-confined, checksum-valid, catalog-consistent, and inside the mapped-byte
budget. It fails closed for missing, corrupt, drifted, metadata-only,
non-`mmap`, path-escaped, or over-budget artifacts. The returned bytes are not a
stable vector search contract. `pgcontext.search_mmap_hnsw_artifact` is the
experimental HNSW mmap serving path: internally it holds a read-only OS mapping
and generation pin, traverses persisted graph links, merges post-generation
inserts, then rechecks live source-table rows for final scoring.

The detailed symptom-to-action runbook is in
[Troubleshooting and maintenance](troubleshooting.md).

## Upgrades

Install and upgrade the extension with ordinary PostgreSQL extension workflows
as a PostgreSQL superuser. pgContext creates an access method, and the supported
standalone 0.1-to-0.2 update also performs a version-pinned extension-namespace
catalog repair. Fresh 0.2 SQL types live in `pgcontext`; qualify them (for
example, `pgcontext.vector`) or deliberately add `pgcontext` to the application
role/database `search_path`.
Upgrade scripts must not discover user data or start index builds during
extension installation. After an upgrade, run smoke queries against collection
registration, exact search, filters, telemetry, and any deployed index paths.

## Normal PostgreSQL Indexes

Add regular PostgreSQL indexes for columns that are used heavily by filters,
joins, partition pruning, or ownership predicates. pgContext filter correctness
does not require pgContext-specific payload indexes; normal B-tree, GIN, BRIN,
and partitioning choices remain useful for reducing candidate sets before or
after vector search.

## Ownership, Privacy, and Support

The application team owns source-table schema, data quality, embedding models,
roles, grants, RLS, backup retention, and query acceptance criteria. The
database team owns PostgreSQL capacity, maintenance, recovery, and extension
installation. Name an operator for HNSW rebuild/recall decisions before using
the experimental indexed path in a controlled pilot.

pgContext has no external telemetry service. SQL-visible telemetry is stored in
the database and is designed for counters, buckets, and typed statuses—not
vectors, payload values, filters, literal query text, credentials, or secrets.
Control access with PostgreSQL grants and apply the application's normal
retention/privacy policy.

Use [GitHub issues](https://github.com/evokoa/pgcontext/issues) for public bugs
and support questions. Do not report vulnerabilities publicly; follow the
[security policy](../../SECURITY.md) or email
[team@evokoa.com](mailto:team@evokoa.com).

## Support Matrix

PostgreSQL 17 and 18 are supported V1 release targets. Release images are
built and runtime-verified on amd64 and arm64. PostgreSQL 17 remains the primary
benchmark and deep-lifecycle qualification target.

Current known limitations:

- SQL `halfvec`, `sparsevec`, and `bitvec` wrappers are experimental; typmods
  and dense/sparse vector casts are available. `halfvec` and `sparsevec` L2
  HNSW indexing and explicit `bitvec` Hamming HNSW indexing are experimental,
  while non-L2 sparse and bit-vector Jaccard ANN indexing are still planned.
- HNSW ordered scans traverse metric-bound durable page records without silent
  exact fallback; bounded recall, restart, replica, VACUUM, and source-recheck
  evidence passes for PG17. The access method remains experimental.
- Backend-local build-job metadata is experimental and does not make
  PostgreSQL access-method builds or mmap serving resumable.
- Rebuildable segment artifacts are validated by internal loaders, but stable
  SQL import/export/rebuild APIs are deferred. They are not a replacement for
  PostgreSQL backups.
